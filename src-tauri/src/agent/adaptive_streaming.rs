//! Adaptive rate control, profile selection, RTCP feedback and stream stats
//! window. Pulled out of `webrtc.rs` so the streaming pipeline tuning lives
//! next to its config without being entangled in the WebRTC plumbing.
//!
//! Public exports (consumed by `session.rs` via `super::webrtc`):
//!  - [`FpsTier`] and [`StreamQualityProfile`] for user-facing presets.

use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rtp::packet::Packet;
use sysinfo::{CpuRefreshKind, RefreshKind, System};

use super::signaling::SignalingClient;
use super::video_encoder::VideoEncoderPreset;

// ─── Stream stats window ──────────────────────────────────────────────────────

pub(super) struct StreamStatsWindow {
    started_at: Instant,
    flush_interval: Duration,
    sent_bytes: usize,
    sent_frames: usize,
    capture_ms_sum: f64,
    encode_ms_sum: f64,
    samples: u64,
    dropped_before_encode: u64,
    dropped_before_send: u64,
    rtcp_nack: u64,
    rtcp_pli: u64,
    rtcp_fir: u64,
}

impl StreamStatsWindow {
    pub(super) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            flush_interval: stream_log_interval(),
            sent_bytes: 0,
            sent_frames: 0,
            capture_ms_sum: 0.0,
            encode_ms_sum: 0.0,
            samples: 0,
            dropped_before_encode: 0,
            dropped_before_send: 0,
            rtcp_nack: 0,
            rtcp_pli: 0,
            rtcp_fir: 0,
        }
    }

    pub(super) fn record_frame(&mut self, frame_bytes: usize) {
        self.sent_bytes += frame_bytes;
        self.sent_frames += 1;
    }

    pub(super) fn record_rtp_packet(&mut self, packet: &Packet) {
        self.sent_bytes += packet.payload.len();
        if packet.header.marker {
            self.sent_frames += 1;
        }
    }

    pub(super) fn record_pipeline_sample(
        &mut self,
        capture_ms: f64,
        encode_ms: f64,
        dropped_before_encode: u64,
        dropped_before_send: u64,
        rtcp_delta: RtcpDelta,
    ) {
        self.capture_ms_sum += capture_ms;
        self.encode_ms_sum += encode_ms;
        self.samples = self.samples.saturating_add(1);
        self.dropped_before_encode = self
            .dropped_before_encode
            .saturating_add(dropped_before_encode);
        self.dropped_before_send = self
            .dropped_before_send
            .saturating_add(dropped_before_send);
        self.rtcp_nack = self.rtcp_nack.saturating_add(rtcp_delta.nack);
        self.rtcp_pli = self.rtcp_pli.saturating_add(rtcp_delta.pli);
        self.rtcp_fir = self.rtcp_fir.saturating_add(rtcp_delta.fir);
    }

    pub(super) async fn flush_if_due(&mut self, signaling: &Arc<SignalingClient>) {
        let elapsed = self.started_at.elapsed();
        if elapsed < self.flush_interval {
            return;
        }

        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let mbps = (self.sent_bytes as f64 * 8.0) / (elapsed_sec * 1_000_000.0);
        let fps = self.sent_frames as f64 / elapsed_sec;
        let bytes_per_second = (self.sent_bytes as f64 / elapsed_sec).round() as i64;
        let avg_capture_ms = if self.samples > 0 {
            self.capture_ms_sum / self.samples as f64
        } else {
            0.0
        };
        let avg_encode_ms = if self.samples > 0 {
            self.encode_ms_sum / self.samples as f64
        } else {
            0.0
        };

        if let Err(err) = signaling
            .send_stream_stats(mbps, fps, bytes_per_second)
            .await
        {
            tracing::warn!("Failed to send stream stats: {err}");
        }

        if stream_structured_log_enabled() {
            let event = serde_json::json!({
                "event": "stream_stats",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "mbps": mbps,
                "fps": fps,
                "bytesPerSecond": bytes_per_second,
                "captureMsAvg": avg_capture_ms,
                "encodeMsAvg": avg_encode_ms,
                "droppedBeforeEncode": self.dropped_before_encode,
                "droppedBeforeSend": self.dropped_before_send,
                "rtcpNack": self.rtcp_nack,
                "rtcpPli": self.rtcp_pli,
                "rtcpFir": self.rtcp_fir,
                "logIntervalMs": self.flush_interval.as_millis(),
            });
            tracing::info!("{}", event);
        }

        self.started_at = Instant::now();
        self.sent_bytes = 0;
        self.sent_frames = 0;
        self.capture_ms_sum = 0.0;
        self.encode_ms_sum = 0.0;
        self.samples = 0;
        self.dropped_before_encode = 0;
        self.dropped_before_send = 0;
        self.rtcp_nack = 0;
        self.rtcp_pli = 0;
        self.rtcp_fir = 0;
    }
}

// ─── Streaming profile / FPS tier ─────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AdaptiveVideoConfig {
    pub target_fps: u32,
    pub bitrate_bps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpsTier {
    Idle,
    Normal,
    Active,
}

impl FpsTier {
    pub fn from_payload(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "idle" | "eco" => Some(Self::Idle),
            "normal" | "balanced" | "equilibre" => Some(Self::Normal),
            "active" | "performance" | "reactive" => Some(Self::Active),
            _ => None,
        }
    }

    pub(super) fn target_fps(self) -> u32 {
        match self {
            Self::Idle => 15,
            Self::Normal => 30,
            Self::Active => 60,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamQualityProfile {
    Responsive,
    Quality,
}

impl StreamQualityProfile {
    pub fn from_payload(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "responsive" | "reactive" | "reactif" => Some(Self::Responsive),
            "quality" | "qualite" => Some(Self::Quality),
            _ => None,
        }
    }
}

pub(super) fn profile_target_for_preset(
    profile: StreamQualityProfile,
    preset: VideoEncoderPreset,
) -> AdaptiveVideoConfig {
    let base = AdaptiveVideoConfig {
        target_fps: preset.target_fps.max(1),
        bitrate_bps: preset.bitrate_bps,
    };

    match profile {
        StreamQualityProfile::Quality => base,
        StreamQualityProfile::Responsive => AdaptiveVideoConfig {
            target_fps: base.target_fps.min(30),
            bitrate_bps: base.bitrate_bps.min(4_000_000),
        },
    }
}

// ─── Rate controller ──────────────────────────────────────────────────────────

pub(super) struct AdaptiveRateController {
    current: AdaptiveVideoConfig,
    ceiling: AdaptiveVideoConfig,
    min_fps: u32,
    min_bitrate_bps: u32,
    last_adjust_at: Instant,
    stress_ema: f64,
}

#[derive(Default)]
pub(super) struct StreamingActivityState {
    last_input_unix_ms: AtomicU64,
}

impl StreamingActivityState {
    pub(super) fn mark_input_activity(&self) {
        self.last_input_unix_ms
            .store(unix_time_millis(), Ordering::Relaxed);
    }

    pub(super) fn is_active_recent(&self, window_ms: u64) -> bool {
        let last = self.last_input_unix_ms.load(Ordering::Relaxed);
        last > 0 && unix_time_millis().saturating_sub(last) <= window_ms
    }
}

pub(super) struct CpuUsageSampler {
    system: System,
    last_sample_at: Instant,
    last_cpu_usage: f64,
}

impl CpuUsageSampler {
    pub(super) fn new() -> Self {
        // `System::new_all()` initialise CPU + RAM + processus + disques +
        // reseau (lourd : ~30-100ms sur Windows). On n'a besoin que du CPU,
        // donc on cible explicitement le refresh — diminue drastiquement le
        // cout de spawn d'un encoder.
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage()),
        );
        system.refresh_cpu_usage();
        Self {
            system,
            last_sample_at: Instant::now(),
            last_cpu_usage: 0.0,
        }
    }

    pub(super) fn sample(&mut self) -> f64 {
        if self.last_sample_at.elapsed() >= Duration::from_secs(1) {
            self.system.refresh_cpu_usage();
            let cpus = self.system.cpus();
            if !cpus.is_empty() {
                let total: f32 = cpus.iter().map(|cpu| cpu.cpu_usage()).sum();
                self.last_cpu_usage = (total / cpus.len() as f32) as f64;
            }
            self.last_sample_at = Instant::now();
        }
        self.last_cpu_usage
    }
}

pub(super) fn choose_auto_fps_tier(
    cpu_usage: f64,
    user_active: bool,
    network_constrained: bool,
) -> FpsTier {
    if network_constrained || cpu_usage >= 85.0 {
        return FpsTier::Idle;
    }

    if user_active && cpu_usage < 70.0 {
        return FpsTier::Active;
    }

    FpsTier::Normal
}

pub(super) fn apply_external_limits(
    mut profile_limit: AdaptiveVideoConfig,
    fps_tier: FpsTier,
    bitrate_override: Option<u32>,
) -> AdaptiveVideoConfig {
    profile_limit.target_fps = profile_limit.target_fps.min(fps_tier.target_fps());
    if let Some(override_bitrate) = bitrate_override {
        profile_limit.bitrate_bps = profile_limit.bitrate_bps.min(override_bitrate.max(100_000));
    }
    profile_limit
}

#[derive(Clone, Copy, Default)]
pub(super) struct AdaptiveFeedback {
    pub dropped_before_encode: u64,
    pub dropped_before_send: u64,
    pub send_error: bool,
    pub rtcp_nack_delta: u64,
    pub rtcp_pli_delta: u64,
    pub rtcp_fir_delta: u64,
    pub rtcp_feedback_stale: bool,
}

#[derive(Clone, Copy, Default)]
pub(super) struct RtcpDelta {
    pub total: u64,
    pub nack: u64,
    pub pli: u64,
    pub fir: u64,
    pub feedback_stale: bool,
}

#[derive(Default)]
pub(super) struct RtcpFeedbackState {
    total_reports: AtomicU64,
    nack_reports: AtomicU64,
    pli_reports: AtomicU64,
    fir_reports: AtomicU64,
    last_feedback_unix_ms: AtomicU64,
}

#[derive(Clone, Copy, Default)]
pub(super) struct RtcpFeedbackSnapshot {
    pub total_reports: u64,
    pub nack_reports: u64,
    pub pli_reports: u64,
    pub fir_reports: u64,
    pub last_feedback_unix_ms: u64,
}

impl RtcpFeedbackState {
    pub(super) fn mark_feedback_from_packet_text(&self, packet_text: &str) {
        self.total_reports.fetch_add(1, Ordering::Relaxed);

        let lowercase = packet_text.to_ascii_lowercase();
        if lowercase.contains("nack") {
            self.nack_reports.fetch_add(1, Ordering::Relaxed);
        }
        if lowercase.contains("picturelossindication") || lowercase.contains(" pli") {
            self.pli_reports.fetch_add(1, Ordering::Relaxed);
        }
        if lowercase.contains("fullintrarequest") || lowercase.contains(" fir") {
            self.fir_reports.fetch_add(1, Ordering::Relaxed);
        }

        self.last_feedback_unix_ms
            .store(unix_time_millis(), Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> RtcpFeedbackSnapshot {
        RtcpFeedbackSnapshot {
            total_reports: self.total_reports.load(Ordering::Relaxed),
            nack_reports: self.nack_reports.load(Ordering::Relaxed),
            pli_reports: self.pli_reports.load(Ordering::Relaxed),
            fir_reports: self.fir_reports.load(Ordering::Relaxed),
            last_feedback_unix_ms: self.last_feedback_unix_ms.load(Ordering::Relaxed),
        }
    }
}

pub(super) fn collect_rtcp_delta(
    rtcp_feedback: &Arc<RtcpFeedbackState>,
    last_snapshot: &mut RtcpFeedbackSnapshot,
) -> RtcpDelta {
    let current = rtcp_feedback.snapshot();
    let delta = RtcpDelta {
        total: current
            .total_reports
            .saturating_sub(last_snapshot.total_reports),
        nack: current
            .nack_reports
            .saturating_sub(last_snapshot.nack_reports),
        pli: current
            .pli_reports
            .saturating_sub(last_snapshot.pli_reports),
        fir: current
            .fir_reports
            .saturating_sub(last_snapshot.fir_reports),
        feedback_stale: current.last_feedback_unix_ms > 0
            && unix_time_millis().saturating_sub(current.last_feedback_unix_ms) > 5_000,
    };
    *last_snapshot = current;
    delta
}

pub(super) fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

impl AdaptiveRateController {
    pub(super) fn new(initial: AdaptiveVideoConfig) -> Self {
        Self {
            current: initial,
            ceiling: initial,
            min_fps: 10,
            min_bitrate_bps: 800_000,
            last_adjust_at: Instant::now(),
            stress_ema: 0.0,
        }
    }

    pub(super) fn on_feedback(&mut self, feedback: AdaptiveFeedback) -> Option<AdaptiveVideoConfig> {
        let now = Instant::now();

        let hard_congestion = feedback.send_error
            || feedback.dropped_before_send > 0
            || feedback.rtcp_pli_delta > 0
            || feedback.rtcp_fir_delta > 0;
        let network_congestion = feedback.rtcp_nack_delta >= 2;
        let local_encode_pressure = feedback.dropped_before_encode > 2
            && feedback.dropped_before_send == 0
            && !feedback.send_error
            && feedback.rtcp_nack_delta == 0
            && feedback.rtcp_pli_delta == 0
            && feedback.rtcp_fir_delta == 0;

        let instant_stress = {
            let mut score = 0.0;
            score += (feedback.dropped_before_encode.min(5) as f64) * 0.20;
            score += (feedback.dropped_before_send.min(5) as f64) * 1.25;
            score += (feedback.rtcp_nack_delta.min(6) as f64) * 0.8;
            score += (feedback.rtcp_pli_delta.min(4) as f64) * 2.8;
            score += (feedback.rtcp_fir_delta.min(4) as f64) * 3.5;
            if feedback.send_error {
                score += 4.0;
            }
            if feedback.rtcp_feedback_stale && score > 0.0 {
                score += 0.5;
            }
            score.min(20.0)
        };

        self.stress_ema = self.stress_ema * 0.75 + instant_stress * 0.25;

        if hard_congestion && now.duration_since(self.last_adjust_at) >= Duration::from_millis(700) {
            self.last_adjust_at = now;
            let next_fps = self.current.target_fps.saturating_sub(7).max(self.min_fps);
            let next_bitrate = ((self.current.bitrate_bps as f64) * 0.80) as u32;
            let next_bitrate = next_bitrate.max(self.min_bitrate_bps);
            return self.set_if_changed(next_fps, next_bitrate);
        }

        if local_encode_pressure
            && now.duration_since(self.last_adjust_at) >= Duration::from_millis(1500)
        {
            self.last_adjust_at = now;
            let next_fps = self.current.target_fps.saturating_sub(2).max(self.min_fps);
            let next_bitrate = self.current.bitrate_bps;
            return self.set_if_changed(next_fps, next_bitrate);
        }

        if (self.stress_ema >= 3.0 || network_congestion)
            && now.duration_since(self.last_adjust_at) >= Duration::from_millis(1300)
        {
            self.last_adjust_at = now;
            let next_fps = self.current.target_fps.saturating_sub(3).max(self.min_fps);
            let next_bitrate = ((self.current.bitrate_bps as f64) * 0.89) as u32;
            let next_bitrate = next_bitrate.max(self.min_bitrate_bps);
            return self.set_if_changed(next_fps, next_bitrate);
        }

        if self.stress_ema <= 0.6
            && !hard_congestion
            && now.duration_since(self.last_adjust_at) >= Duration::from_secs(3)
        {
            self.last_adjust_at = now;
            let next_fps =
                (self.current.target_fps + 2).min(self.ceiling.target_fps.max(self.min_fps));
            let next_bitrate =
                (self.current.bitrate_bps + 500_000).min(self.ceiling.bitrate_bps);
            return self.set_if_changed(next_fps, next_bitrate);
        }

        None
    }

    fn set_if_changed(&mut self, target_fps: u32, bitrate_bps: u32) -> Option<AdaptiveVideoConfig> {
        let next = AdaptiveVideoConfig {
            target_fps,
            bitrate_bps,
        };
        if next == self.current {
            return None;
        }
        self.current = next;
        Some(next)
    }

    pub(super) fn apply_profile_limit(
        &mut self,
        limit: AdaptiveVideoConfig,
    ) -> Option<AdaptiveVideoConfig> {
        self.ceiling = AdaptiveVideoConfig {
            target_fps: limit.target_fps.max(self.min_fps),
            bitrate_bps: limit.bitrate_bps.max(self.min_bitrate_bps),
        };

        let clamped_fps = self.current.target_fps.min(self.ceiling.target_fps);
        let clamped_bitrate = self.current.bitrate_bps.min(self.ceiling.bitrate_bps);
        self.set_if_changed(clamped_fps, clamped_bitrate)
    }
}

// ─── Stream logging tunables ──────────────────────────────────────────────────

pub(super) fn stream_structured_log_enabled() -> bool {
    super::webrtc::env_flag_true("LUMIERE_STREAM_STRUCTURED_LOG")
}

pub(super) fn stream_log_interval() -> Duration {
    let interval_ms = env::var("LUMIERE_STREAM_LOG_INTERVAL_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(250, 10_000))
        .unwrap_or(1_000);
    Duration::from_millis(interval_ms)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::video_encoder::VideoEncoderPreset;

    // ── FpsTier parsing ──────────────────────────────────────────────────────

    #[test]
    fn fps_tier_from_payload_idle_aliases() {
        assert_eq!(FpsTier::from_payload("idle"), Some(FpsTier::Idle));
        assert_eq!(FpsTier::from_payload("ECO"), Some(FpsTier::Idle));
        assert_eq!(FpsTier::from_payload("  eco  "), Some(FpsTier::Idle));
    }

    #[test]
    fn fps_tier_from_payload_normal_aliases() {
        assert_eq!(FpsTier::from_payload("normal"), Some(FpsTier::Normal));
        assert_eq!(FpsTier::from_payload("balanced"), Some(FpsTier::Normal));
        assert_eq!(FpsTier::from_payload("equilibre"), Some(FpsTier::Normal));
    }

    #[test]
    fn fps_tier_from_payload_active_aliases() {
        assert_eq!(FpsTier::from_payload("active"), Some(FpsTier::Active));
        assert_eq!(FpsTier::from_payload("performance"), Some(FpsTier::Active));
    }

    #[test]
    fn fps_tier_from_payload_unknown_returns_none() {
        assert_eq!(FpsTier::from_payload("foo"), None);
        assert_eq!(FpsTier::from_payload(""), None);
    }

    #[test]
    fn fps_tier_target_fps_ordering() {
        assert!(FpsTier::Idle.target_fps() < FpsTier::Normal.target_fps());
        assert!(FpsTier::Normal.target_fps() < FpsTier::Active.target_fps());
    }

    // ── StreamQualityProfile parsing ─────────────────────────────────────────

    #[test]
    fn quality_profile_from_payload_responsive_aliases() {
        assert_eq!(StreamQualityProfile::from_payload("responsive"), Some(StreamQualityProfile::Responsive));
        assert_eq!(StreamQualityProfile::from_payload("reactif"), Some(StreamQualityProfile::Responsive));
        assert_eq!(StreamQualityProfile::from_payload("REACTIVE"), Some(StreamQualityProfile::Responsive));
    }

    #[test]
    fn quality_profile_from_payload_quality_aliases() {
        assert_eq!(StreamQualityProfile::from_payload("quality"), Some(StreamQualityProfile::Quality));
        assert_eq!(StreamQualityProfile::from_payload("qualite"), Some(StreamQualityProfile::Quality));
    }

    #[test]
    fn quality_profile_from_payload_unknown() {
        assert_eq!(StreamQualityProfile::from_payload("blah"), None);
    }

    // ── choose_auto_fps_tier ────────────────────────────────────────────────

    #[test]
    fn auto_tier_idle_when_network_constrained() {
        assert_eq!(choose_auto_fps_tier(20.0, true, true), FpsTier::Idle);
    }

    #[test]
    fn auto_tier_idle_on_high_cpu() {
        assert_eq!(choose_auto_fps_tier(90.0, true, false), FpsTier::Idle);
    }

    #[test]
    fn auto_tier_active_when_user_active_and_low_cpu() {
        assert_eq!(choose_auto_fps_tier(40.0, true, false), FpsTier::Active);
    }

    #[test]
    fn auto_tier_normal_when_user_idle() {
        assert_eq!(choose_auto_fps_tier(40.0, false, false), FpsTier::Normal);
    }

    #[test]
    fn auto_tier_normal_when_cpu_medium_active() {
        // CPU between 70 and 85 with user active → Normal (not Active).
        assert_eq!(choose_auto_fps_tier(75.0, true, false), FpsTier::Normal);
    }

    // ── apply_external_limits ───────────────────────────────────────────────

    #[test]
    fn external_limits_caps_fps_by_tier() {
        let base = AdaptiveVideoConfig { target_fps: 60, bitrate_bps: 5_000_000 };
        let capped = apply_external_limits(base, FpsTier::Idle, None);
        assert_eq!(capped.target_fps, FpsTier::Idle.target_fps());
        assert_eq!(capped.bitrate_bps, 5_000_000);
    }

    #[test]
    fn external_limits_caps_bitrate_when_override_lower() {
        let base = AdaptiveVideoConfig { target_fps: 30, bitrate_bps: 5_000_000 };
        let capped = apply_external_limits(base, FpsTier::Normal, Some(2_000_000));
        assert_eq!(capped.bitrate_bps, 2_000_000);
    }

    #[test]
    fn external_limits_keeps_existing_bitrate_when_override_higher() {
        let base = AdaptiveVideoConfig { target_fps: 30, bitrate_bps: 1_500_000 };
        let capped = apply_external_limits(base, FpsTier::Normal, Some(5_000_000));
        assert_eq!(capped.bitrate_bps, 1_500_000);
    }

    #[test]
    fn external_limits_enforces_minimum_bitrate_floor() {
        // Override below the 100 kbps floor must be raised back to 100 kbps.
        let base = AdaptiveVideoConfig { target_fps: 30, bitrate_bps: 5_000_000 };
        let capped = apply_external_limits(base, FpsTier::Normal, Some(50_000));
        assert!(capped.bitrate_bps >= 100_000);
    }

    // ── profile_target_for_preset ───────────────────────────────────────────

    #[test]
    fn responsive_profile_caps_fps_at_30_and_bitrate_at_4mbps() {
        // A preset asking for 60 FPS / 8 Mbps must be clamped under "Responsive".
        let preset = VideoEncoderPreset { target_fps: 60, bitrate_bps: 8_000_000 };
        let target = profile_target_for_preset(StreamQualityProfile::Responsive, preset);
        assert!(target.target_fps <= 30);
        assert!(target.bitrate_bps <= 4_000_000);
    }

    #[test]
    fn quality_profile_passes_preset_through() {
        let preset = VideoEncoderPreset { target_fps: 60, bitrate_bps: 8_000_000 };
        let target = profile_target_for_preset(StreamQualityProfile::Quality, preset);
        assert_eq!(target.target_fps, 60);
        assert_eq!(target.bitrate_bps, 8_000_000);
    }

    #[test]
    fn profile_target_treats_zero_fps_as_one() {
        // Defensive: a preset with target_fps=0 must not produce a zero-FPS config.
        let preset = VideoEncoderPreset { target_fps: 0, bitrate_bps: 1_000_000 };
        let target = profile_target_for_preset(StreamQualityProfile::Quality, preset);
        assert!(target.target_fps >= 1);
    }

    // ── RtcpFeedbackState detection ─────────────────────────────────────────

    #[test]
    fn rtcp_state_counts_nack() {
        let state = RtcpFeedbackState::default();
        state.mark_feedback_from_packet_text("RTCP NACK report");
        let snap = state.snapshot();
        assert_eq!(snap.nack_reports, 1);
        assert_eq!(snap.total_reports, 1);
    }

    #[test]
    fn rtcp_state_counts_pli_variants() {
        let state = RtcpFeedbackState::default();
        state.mark_feedback_from_packet_text("PictureLossIndication packet");
        state.mark_feedback_from_packet_text("rtcp pli message");
        let snap = state.snapshot();
        assert_eq!(snap.pli_reports, 2);
        assert_eq!(snap.total_reports, 2);
    }

    #[test]
    fn rtcp_state_counts_fir_variants() {
        let state = RtcpFeedbackState::default();
        state.mark_feedback_from_packet_text("FullIntraRequest");
        state.mark_feedback_from_packet_text(" fir trigger");
        let snap = state.snapshot();
        assert_eq!(snap.fir_reports, 2);
    }

    #[test]
    fn rtcp_state_records_timestamp() {
        let state = RtcpFeedbackState::default();
        state.mark_feedback_from_packet_text("NACK");
        let snap = state.snapshot();
        assert!(snap.last_feedback_unix_ms > 0);
    }
}
