//! Screen capture → H264 encode → RTP send pipeline.
//!
//! Hosts the three encoder-backend loops the agent picks between at runtime:
//!  - [`run_openh264_screen_sender`] : pure-Rust software encoder (portable).
//!  - [`run_media_foundation_screen_sender`] : hardware-accelerated MF (Windows).
//!  - [`run_ffmpeg_rtp_screen_sender`] : sidecar FFmpeg process emitting RTP/SRTP.
//!
//! Each loop captures frames via [`super::capture`], hands them through the
//! adaptive rate controller (see [`super::adaptive_streaming`]), packetizes
//! H264 NAL units and pushes RTP packets to the WebRTC `TrackLocalStaticRTP`.
//! Pure pipeline code — no peer connection lifecycle here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;
use rtp::codecs::h264::H264Payloader;
use rtp::packet::Packet;
use rtp::packetizer::Payloader;
use tokio::sync::{broadcast, watch};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::TrackLocalWriter;
use webrtc_util::marshal::Unmarshal;

#[cfg(windows)]
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod, TIMERR_NOERROR};

use super::adaptive_streaming::{
    apply_external_limits, choose_auto_fps_tier, collect_rtcp_delta, profile_target_for_preset,
    AdaptiveFeedback, AdaptiveRateController, AdaptiveVideoConfig, CpuUsageSampler, FpsTier,
    RtcpFeedbackState, StreamQualityProfile, StreamStatsWindow, StreamingActivityState,
};
use super::capture::{
    capture_primary_screen_even_bgra, create_default_capturer, resolve_scale_request,
};
use super::h264_helpers::{
    reorder_and_cache_sps_pps, resolve_h264_payload_type, resolve_video_ssrc,
    split_annexb_nalus, summarize_nalus, NalSummary,
};
use super::media_foundation_encoder::MediaFoundationEncoderWorker;
use super::privacy_filter::PrivacyFilter;
use super::signaling::SignalingClient;
use super::video_encoder::{FfmpegRtpBridge, VideoEncoderBackend, VideoEncoderPreset};
use super::webrtc::derive_stream_ssrc;

/// Debug-only video logging — gate behind the `LUMIERE_VIDEO_DEBUG` flag
/// (re-exported from [`super::webrtc::video_debug_enabled`]).
macro_rules! vlog {
    ($($arg:tt)*) => {{
        if super::webrtc::video_debug_enabled() {
            tracing::info!("[video][dbg] {}", format_args!($($arg)*));
        }
    }};
}

/// Paramètres communs aux deux boucles d'encodeur logiciel et matériel.
///
/// `run_openh264_screen_sender` et `run_media_foundation_screen_sender`
/// prenaient initialement 10 arguments identiques chacun. On les regroupe ici
/// pour :
///   - rendre les appels lisibles côté `webrtc.rs` (4 sites d'appel),
///   - éviter une divergence accidentelle de signature lors des évolutions,
///   - clone() est explicite (les fallbacks recréent les receveurs `watch`).
///
/// Note : `signaling`, `peer` et `track` restent en référence pour éviter de
/// gonfler les compteurs Arc à chaque appel — ils sont déjà partagés via
/// `Arc` au niveau supérieur.
pub(super) struct ScreenSenderArgs<'a> {
    pub signaling: &'a Arc<SignalingClient>,
    pub peer: &'a Arc<RTCPeerConnection>,
    pub track: &'a Arc<TrackLocalStaticRTP>,
    pub preset: VideoEncoderPreset,
    pub rtcp_feedback: Arc<RtcpFeedbackState>,
    pub stream_profile_rx: watch::Receiver<StreamQualityProfile>,
    pub bitrate_override_rx: watch::Receiver<Option<u32>>,
    pub fps_tier_override_rx: watch::Receiver<Option<FpsTier>>,
    pub activity_state: Arc<StreamingActivityState>,
    pub frame_emission_paused: Arc<AtomicBool>,
    /// Shared privacy filter. When `Some`, every captured BGRA frame is
    /// passed through `PrivacyFilter::process_frame` *before* H.264
    /// encoding, so blurred regions never leave the agent over the wire.
    /// `None` disables the feature entirely (no allocation, no overhead).
    pub privacy_filter: Option<Arc<std::sync::Mutex<PrivacyFilter>>>,
}

/// Mutable RTP emission context — borrowed by [`emit_nal_as_rtp`] across the
/// hot loops of both encoders so we don't drag a half-dozen identical
/// arguments through every call site.
struct RtpEmitCtx<'a> {
    payloader: &'a mut H264Payloader,
    track: &'a Arc<TrackLocalStaticRTP>,
    seq: &'a mut u16,
    payload_type: u8,
    frame_ts: u32,
    stream_ssrc: u32,
}

/// Result of emitting one NAL unit:
///  - `fragments`  : nombre de paquets RTP générés (utile pour les stats)
///  - `send_error` : `true` si `track.write_rtp` a échoué (track fermé)
///
/// Le caller incrémente lui-même `total_payload_bytes` AVANT l'appel — la
/// fonction ne touche pas aux compteurs côté frame pour rester réutilisable
/// entre les deux encoders OpenH264 / Media Foundation.
struct EmitOutcome {
    fragments: usize,
    send_error: bool,
}

/// Cœur dupliqué entre `run_openh264_screen_sender` et
/// `run_media_foundation_screen_sender` : payload split + envoi RTP packet par
/// packet. `is_last_in_frame` contrôle le bit `marker` du dernier fragment du
/// dernier NAL — convention RTP H264 pour signaler la fin d'une trame.
async fn emit_nal_as_rtp(
    nal_bytes: &Bytes,
    is_last_in_frame: bool,
    ctx: &mut RtpEmitCtx<'_>,
    log_label: &str,
) -> EmitOutcome {
    let payloads = match ctx.payloader.payload(1200, nal_bytes) {
        Ok(chunks) => chunks,
        Err(err) => {
            tracing::warn!("H264 payload split failed ({log_label}): {err}");
            return EmitOutcome { fragments: 0, send_error: false };
        }
    };
    let total = payloads.len();
    for (index, fragment) in payloads.iter().enumerate() {
        let marker = is_last_in_frame && (index + 1 == total);
        let packet = Packet {
            header: rtp::header::Header {
                version: 2,
                padding: false,
                extension: false,
                marker,
                payload_type: ctx.payload_type,
                sequence_number: *ctx.seq,
                timestamp: ctx.frame_ts,
                ssrc: ctx.stream_ssrc,
                csrc: vec![],
                extension_profile: 0,
                extensions: vec![],
                extensions_padding: 0,
            },
            payload: fragment.clone(),
        };
        if ctx.track.write_rtp(&packet).await.is_err() {
            return EmitOutcome { fragments: total, send_error: true };
        }
        *ctx.seq = ctx.seq.wrapping_add(1);
    }
    EmitOutcome { fragments: total, send_error: false }
}

#[derive(Clone)]
struct CapturedScreenFrame {
    width: usize,
    height: usize,
    bgra_frame: Arc<Vec<u8>>,
    reused_last_frame: bool,
    capture_ms: f64,
    frame_counter: u64,
}

#[derive(Clone)]
struct EncodedScreenFrame {
    width: usize,
    height: usize,
    reused_last_frame: bool,
    capture_ms: f64,
    encode_ms: f64,
    frame_counter: u64,
    dropped_before_encode: u64,
    encoded_units: Vec<Vec<u8>>,
}

fn frame_interval_for_target_fps(target_fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / target_fps.max(1) as f64)
}

#[cfg(windows)]
struct WindowsTimerResolutionGuard {
    period_ms: u32,
    enabled: bool,
}

#[cfg(windows)]
impl WindowsTimerResolutionGuard {
    fn new(period_ms: u32) -> Self {
        let enabled = unsafe { timeBeginPeriod(period_ms) == TIMERR_NOERROR };
        if !enabled {
            tracing::warn!("timeBeginPeriod({period_ms}) failed; frame pacing may jitter under load");
        }
        Self { period_ms, enabled }
    }
}

#[cfg(windows)]
impl Drop for WindowsTimerResolutionGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = unsafe { timeEndPeriod(self.period_ms) };
        }
    }
}

async fn sleep_until_deadline_precise(deadline: Instant) {
    let now = Instant::now();
    if now >= deadline {
        return;
    }

    #[cfg(windows)]
    {
        let coarse_sleep_threshold = Duration::from_millis(2);
        let spin_window = Duration::from_millis(1);
        let remaining = deadline.saturating_duration_since(now);
        if remaining > coarse_sleep_threshold {
            tokio::time::sleep(remaining.saturating_sub(spin_window)).await;
        }
        while Instant::now() < deadline {
            std::hint::spin_loop();
        }
    }

    #[cfg(not(windows))]
    {
        tokio::time::sleep(deadline.saturating_duration_since(now)).await;
    }
}

async fn pace_capture_loop(next_deadline: &mut Instant, target_fps: u32) {
    let interval = frame_interval_for_target_fps(target_fps);
    let now = Instant::now();

    if *next_deadline <= now {
        *next_deadline = now + interval;
        return;
    }

    sleep_until_deadline_precise(*next_deadline).await;
    *next_deadline += interval;
}

fn create_openh264_encoder(config: AdaptiveVideoConfig) -> Result<Encoder, String> {
    let api = OpenH264API::from_source();
    let encoder_config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .set_bitrate_bps(config.bitrate_bps)
        .max_frame_rate(config.target_fps as f32)
        .enable_skip_frame(true)
        .set_multiple_thread_idc(0);

    Encoder::with_api_config(api, encoder_config)
        .map_err(|err| format!("OpenH264 encoder init failed: {err}"))
}

async fn recv_latest_broadcast<T: Clone>(
    rx: &mut broadcast::Receiver<T>,
    dropped_counter: &mut u64,
) -> Option<T> {
    let mut latest = loop {
        match rx.recv().await {
            Ok(value) => break value,
            Err(broadcast::error::RecvError::Closed) => return None,
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                *dropped_counter = dropped_counter.saturating_add(skipped);
            }
        }
    };

    loop {
        match rx.try_recv() {
            Ok(newer) => {
                *dropped_counter = dropped_counter.saturating_add(1);
                latest = newer;
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => break,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                *dropped_counter = dropped_counter.saturating_add(skipped);
            }
        }
    }

    Some(latest)
}

// ─────────────────────────────────────────────────────────────────────────────

/// Initialise les ressources partagées par les deux boucles d'encoder
/// (OpenH264 et Media Foundation) :
///   * Le watch channel `adaptive_tx/rx` qui propage les changements de
///     bitrate/FPS du contrôleur adaptatif vers les tâches de capture/encode.
///   * `force_idr_hint` : signalement IDR forcée (sur reception PLI/FIR).
///   * Les broadcast channels capture↔encode (cap 2 pour drop des frames
///     en retard si l'encoder lag).
///   * `StreamStatsWindow` pour les remontées 1Hz au signaling.
struct SenderInitState {
    initial_config: AdaptiveVideoConfig,
    adaptive_tx: watch::Sender<AdaptiveVideoConfig>,
    adaptive_rx: watch::Receiver<AdaptiveVideoConfig>,
    force_idr_hint: Arc<AtomicBool>,
    capture_tx: broadcast::Sender<CapturedScreenFrame>,
    capture_rx: broadcast::Receiver<CapturedScreenFrame>,
}

fn init_sender_state(
    stream_profile_rx: &watch::Receiver<StreamQualityProfile>,
    preset: VideoEncoderPreset,
) -> SenderInitState {
    let initial_profile = *stream_profile_rx.borrow();
    let initial_config = profile_target_for_preset(initial_profile, preset);
    let (adaptive_tx, adaptive_rx) = watch::channel(initial_config);
    let (capture_tx, capture_rx) = broadcast::channel::<CapturedScreenFrame>(2);
    SenderInitState {
        initial_config,
        adaptive_tx,
        adaptive_rx,
        force_idr_hint: Arc::new(AtomicBool::new(false)),
        capture_tx,
        capture_rx,
    }
}

pub(super) async fn run_openh264_screen_sender(
    args: ScreenSenderArgs<'_>,
) -> Result<(), String> {
    let ScreenSenderArgs {
        signaling,
        peer,
        track,
        preset,
        rtcp_feedback,
        mut stream_profile_rx,
        mut bitrate_override_rx,
        mut fps_tier_override_rx,
        activity_state,
        frame_emission_paused,
        privacy_filter,
    } = args;
    let SenderInitState {
        initial_config,
        adaptive_tx,
        adaptive_rx,
        force_idr_hint,
        capture_tx,
        mut capture_rx,
    } = init_sender_state(&stream_profile_rx, preset);
    let (encoded_tx, mut encoded_rx) = broadcast::channel::<EncodedScreenFrame>(2);

    let mut payloader = H264Payloader::default();
    let mut stream_ssrc: u32 = 0;
    let mut negotiated_ssrc: Option<u32> = None;
    let mut negotiated_payload_type: Option<u8> = None;
    let mut cached_sps: Option<Vec<u8>> = None;
    let mut cached_pps: Option<Vec<u8>> = None;
    let mut seq: u16 = 1;
    let stream_clock_start = Instant::now();
    let mut last_rtp_ts: u32 = 0;
    let mut stats = StreamStatsWindow::new();

    let capture_peer = Arc::clone(peer);
    let capture_track = Arc::clone(track);
    let capture_cfg_rx = adaptive_rx.clone();
    let capture_paused = Arc::clone(&frame_emission_paused);
    let capture_privacy_filter = privacy_filter.clone();
    let capture_task = tokio::spawn(async move {
        #[cfg(windows)]
        let _timer_resolution_guard = WindowsTimerResolutionGuard::new(1);

        let mut capturer = match create_default_capturer() {
            Ok(capturer) => capturer,
            Err(err) => {
                tracing::warn!("DXGI capturer init failed: {err}");
                return;
            }
        };

        let scale_target = resolve_scale_request();
        let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;
        let mut frame_counter: u64 = 0;
        let mut next_capture_deadline = Instant::now() + frame_interval_for_target_fps(initial_config.target_fps);

        loop {
            let capture_config = *capture_cfg_rx.borrow();
            match capture_peer.connection_state() {
                RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
                _ => {}
            }

            // Pause demandée par le viewer (transfert de fichier en cours).
            // On dort sans capturer ni encoder ni envoyer → bande passante
            // vidéo strictement nulle, le DataChannel a la pipe pour lui seul.
            if capture_paused.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(150)).await;
                next_capture_deadline = Instant::now() + frame_interval_for_target_fps(capture_config.target_fps);
                continue;
            }

            if !stream_is_ready(&capture_peer, &capture_track).await {
                tokio::time::sleep(Duration::from_millis(120)).await;
                continue;
            }

            let capture_start = Instant::now();
            let mut reused_last_frame = false;
            let (width, height, bgra_frame) = match capture_primary_screen_even_bgra(
                &mut *capturer,
                scale_target,
            ) {
                Ok(Some((w, h, mut frame))) => {
                    // Privacy blur — applied *before* H.264 encoding so
                    // password regions never reach the wire. No-op when
                    // the filter is disabled (technician toggled OFF).
                    if let Some(ref filter) = capture_privacy_filter {
                        if let Ok(mut guard) = filter.lock() {
                            guard.process_frame(&mut frame, w as u32, h as u32);
                        }
                    }
                    let arc = Arc::new(frame);
                    last_capture = Some((w, h, Arc::clone(&arc)));
                    (w, h, arc)
                }
                Ok(None) => {
                    if let Some((w, h, ref arc)) = last_capture {
                        reused_last_frame = true;
                        (w, h, Arc::clone(arc))
                    } else {
                        tokio::time::sleep(Duration::from_millis(4)).await;
                        continue;
                    }
                }
                Err(err) => {
                    tracing::warn!("Screen capture failed: {err}");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            };

            frame_counter = frame_counter.saturating_add(1);
            let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;

            let frame = CapturedScreenFrame {
                width,
                height,
                bgra_frame,
                reused_last_frame,
                capture_ms,
                frame_counter,
            };
            let _ = capture_tx.send(frame);

            pace_capture_loop(&mut next_capture_deadline, capture_config.target_fps).await;
        }
    });

    let mut encode_cfg_rx = adaptive_rx.clone();
    let force_idr_hint_for_encode = Arc::clone(&force_idr_hint);
    let encode_task = tokio::spawn(async move {
        let mut active_config = *encode_cfg_rx.borrow();
        let mut encoder = match create_openh264_encoder(active_config) {
            Ok(encoder) => encoder,
            Err(err) => {
                tracing::warn!("OpenH264 encoder init failed: {err}");
                return;
            }
        };

        let mut dropped_capture_frames: u64 = 0;
        loop {
            let Some(frame) = recv_latest_broadcast(&mut capture_rx, &mut dropped_capture_frames).await else {
                break;
            };

            let dropped_before_encode = std::mem::take(&mut dropped_capture_frames);

            let latest_config = *encode_cfg_rx.borrow_and_update();
            if latest_config != active_config {
                match create_openh264_encoder(latest_config) {
                    Ok(new_encoder) => {
                        encoder = new_encoder;
                        active_config = latest_config;
                        tracing::info!(
                            "OpenH264 adaptive reconfigure: {} FPS, {:.2} Mbps",
                            active_config.target_fps,
                            active_config.bitrate_bps as f64 / 1_000_000.0
                        );
                    }
                    Err(err) => {
                        tracing::warn!("OpenH264 reconfigure failed: {err}");
                    }
                }
            }

            if frame.frame_counter % active_config.target_fps.max(1) as u64 == 0 {
                tracing::info!(
                    "Captured {} frames via DXGI at {}x{} for software H264 pipeline",
                    frame.frame_counter, frame.width, frame.height
                );
            }

            // Production low-latency target: force an IDR approximately every 2 seconds.
            let keyframe_interval = (active_config.target_fps.max(1) as u64).saturating_mul(2);
            let force_keyframe = frame.frame_counter == 1
                || (keyframe_interval > 0 && frame.frame_counter % keyframe_interval == 0)
                || force_idr_hint_for_encode.swap(false, Ordering::Relaxed);
            let width = frame.width;
            let height = frame.height;
            let bgra_frame = Arc::clone(&frame.bgra_frame);
            let encode_start = Instant::now();

            let join_result = tokio::task::spawn_blocking(move || {
                let bgra = BgraSliceU8::new(&bgra_frame, (width, height));
                let yuv = YUVBuffer::from_rgb_source(bgra);

                if force_keyframe {
                    encoder.force_intra_frame();
                }

                let result = encoder.encode(&yuv).map(|bitstream| bitstream.to_vec());
                (result, encoder)
            })
            .await;

            let (encoded_result, active_encoder) = match join_result {
                Ok(value) => value,
                Err(e) => {
                    tracing::warn!("spawn_blocking failed: {e}");
                    break;
                }
            };
            encoder = active_encoder;

            let encoded = match encoded_result {
                Ok(data) => data,
                Err(err) => {
                    tracing::warn!("H264 encode failed: {err}");
                    continue;
                }
            };
            if encoded.is_empty() {
                continue;
            }

            let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;
            let packet = EncodedScreenFrame {
                width: frame.width,
                height: frame.height,
                reused_last_frame: frame.reused_last_frame,
                capture_ms: frame.capture_ms,
                encode_ms,
                frame_counter: frame.frame_counter,
                dropped_before_encode,
                encoded_units: vec![encoded],
            };
            let _ = encoded_tx.send(packet);
        }
    });

    let mut dropped_encoded_frames: u64 = 0;
    let mut controller = AdaptiveRateController::new(initial_config);
    let mut last_rtcp_snapshot = rtcp_feedback.snapshot();
    let mut cpu_sampler = CpuUsageSampler::new();
    let mut network_pressure_until = Instant::now();
    let mut current_fps_tier = FpsTier::Normal;
    loop {
        let cpu_usage = cpu_sampler.sample();
        let user_active = activity_state.is_active_recent(1500);
        let network_constrained = Instant::now() <= network_pressure_until;
        let auto_fps_tier = choose_auto_fps_tier(cpu_usage, user_active, network_constrained);
        let requested_fps_tier = fps_tier_override_rx
            .borrow_and_update()
            .as_ref()
            .copied()
            .unwrap_or(auto_fps_tier);
        if requested_fps_tier != current_fps_tier {
            tracing::info!(
                "OpenH264 FPS tier -> {:?} (cpu={:.1}%, user_active={}, network_constrained={})",
                requested_fps_tier,
                cpu_usage,
                user_active,
                network_constrained
            );
            current_fps_tier = requested_fps_tier;
        }

        let profile_limit = profile_target_for_preset(*stream_profile_rx.borrow_and_update(), preset);
        let bitrate_override = *bitrate_override_rx.borrow_and_update();
        let effective_limit = apply_external_limits(profile_limit, current_fps_tier, bitrate_override);
        if let Some(updated) = controller.apply_profile_limit(effective_limit) {
            let _ = adaptive_tx.send(updated);
            tracing::info!(
                "OpenH264 profile target: {} FPS, {:.2} Mbps",
                updated.target_fps,
                updated.bitrate_bps as f64 / 1_000_000.0
            );
        }

        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

        let Some(frame) = recv_latest_broadcast(&mut encoded_rx, &mut dropped_encoded_frames).await else {
            break;
        };

        let dropped_before_send = std::mem::take(&mut dropped_encoded_frames);

        if !stream_is_ready(peer, track).await {
            tokio::time::sleep(Duration::from_millis(120)).await;
            continue;
        }

        if negotiated_payload_type.is_none() {
            negotiated_payload_type = resolve_h264_payload_type(peer).await;
            if let Some(pt) = negotiated_payload_type {
                tracing::info!("Negotiated H264 RTP payload type: {pt}");
            } else {
                tracing::warn!(
                    "Could not resolve H264 RTP payload type from SDP yet; defaulting to 96 until available"
                );
            }
        }

        let payload_type = negotiated_payload_type.unwrap_or(96);

        if negotiated_ssrc.is_none() {
            negotiated_ssrc = resolve_video_ssrc(peer).await;
            stream_ssrc = negotiated_ssrc.unwrap_or_else(derive_stream_ssrc);
            tracing::info!("Using video SSRC: {stream_ssrc}");
        }

        let frame_start = Instant::now();
        let mut frame_ts = rtp_timestamp_90khz_from_instant(&stream_clock_start);
        if frame_ts <= last_rtp_ts {
            frame_ts = last_rtp_ts.wrapping_add(1);
        }
        last_rtp_ts = frame_ts;
        let mut frame_sent = false;
        let mut send_error = false;
        let mut total_fragments = 0usize;
        let mut total_payload_bytes = 0usize;

        for encoded in frame.encoded_units {
            let payload_start = Instant::now();
            let raw_nalus = split_annexb_nalus(&encoded);
            let (nalus, has_idr) =
                reorder_and_cache_sps_pps(raw_nalus, &mut cached_sps, &mut cached_pps);
            let nal_summary = summarize_nalus(&nalus);
            let payload_ms = payload_start.elapsed().as_secs_f64() * 1000.0;

            // At most 2 prefix NALs: SPS + PPS. Avoids reallocation in hot path.
            let mut prefix: Vec<Bytes> = Vec::with_capacity(2);
            if has_idr {
                if !nal_summary.has_sps {
                    if let Some(sps) = cached_sps.as_deref() {
                        prefix.push(Bytes::copy_from_slice(sps));
                    }
                }
                if !nal_summary.has_pps {
                    if let Some(pps) = cached_pps.as_deref() {
                        prefix.push(Bytes::copy_from_slice(pps));
                    }
                }
            }

            let total_nals_to_send = prefix.len() + nalus.len();
            let mut nal_cursor = 0usize;

            let mut ctx = RtpEmitCtx {
                payloader: &mut payloader,
                track: &track,
                seq: &mut seq,
                payload_type,
                frame_ts,
                stream_ssrc,
            };

            for bytes in &prefix {
                nal_cursor += 1;
                total_payload_bytes = total_payload_bytes.saturating_add(bytes.len());
                let last_nal = nal_cursor == total_nals_to_send;
                let out = emit_nal_as_rtp(bytes, last_nal, &mut ctx, "openh264").await;
                total_fragments = total_fragments.saturating_add(out.fragments);
                if out.fragments > 0 { frame_sent = true; }
                if out.send_error { send_error = true; break; }
            }

            if !send_error {
                for nal in nalus.iter() {
                    if nal.is_empty() {
                        continue;
                    }
                    total_payload_bytes = total_payload_bytes.saturating_add(nal.len());
                    let nal_bytes = Bytes::copy_from_slice(nal);
                    nal_cursor += 1;
                    let last_nal = nal_cursor == total_nals_to_send;
                    let out = emit_nal_as_rtp(&nal_bytes, last_nal, &mut ctx, "openh264").await;
                    total_fragments = total_fragments.saturating_add(out.fragments);
                    if out.fragments > 0 { frame_sent = true; }
                    if out.send_error { send_error = true; break; }
                }
            }

            let send_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
            if frame_sent {
                stats.record_frame(total_payload_bytes.max(1));
                if frame.frame_counter % preset.target_fps.max(1) as u64 == 0 {
                    tracing::info!(
                        "Sent software H264 frame {} ({} bytes payload)",
                        frame.frame_counter, total_payload_bytes
                    );
                    vlog!(
                        "sw pipeline: {}x{} reuse_last={} capture={:.2}ms encode={:.2}ms payload={:.2}ms send={:.2}ms total={:.2}ms nalus={} sps={} pps={} idr={} frags={} bytes={} drop_cap={} drop_enc={}",
                        frame.width,
                        frame.height,
                        frame.reused_last_frame,
                        frame.capture_ms,
                        frame.encode_ms,
                        payload_ms,
                        send_ms,
                        frame_start.elapsed().as_secs_f64() * 1000.0,
                        nal_summary.nalus,
                        nal_summary.has_sps,
                        nal_summary.has_pps,
                        nal_summary.has_idr,
                        total_fragments,
                        total_payload_bytes,
                        frame.dropped_before_encode,
                        dropped_before_send,
                    );
                }
            }
        }

        let rtcp_delta = collect_rtcp_delta(&rtcp_feedback, &mut last_rtcp_snapshot);
        stats.record_pipeline_sample(
            frame.capture_ms,
            frame.encode_ms,
            frame.dropped_before_encode,
            dropped_before_send,
            rtcp_delta,
        );
        if send_error
            || dropped_before_send > 0
            || rtcp_delta.nack >= 3
            || rtcp_delta.pli > 0
            || rtcp_delta.fir > 0
        {
            network_pressure_until = Instant::now() + Duration::from_secs(2);
        }
        if let Some(next) = controller.on_feedback(AdaptiveFeedback {
            dropped_before_encode: frame.dropped_before_encode,
            dropped_before_send,
            send_error,
            rtcp_nack_delta: rtcp_delta.nack,
            rtcp_pli_delta: rtcp_delta.pli,
            rtcp_fir_delta: rtcp_delta.fir,
            rtcp_feedback_stale: rtcp_delta.total > 0 && rtcp_delta.feedback_stale,
        }) {
            let _ = adaptive_tx.send(next);
            tracing::info!(
                "OpenH264 adaptive target: {} FPS, {:.2} Mbps",
                next.target_fps,
                next.bitrate_bps as f64 / 1_000_000.0
            );
        }

        if rtcp_delta.pli > 0 || rtcp_delta.fir > 0 {
            // Recover quickly from decoder corruption after packet loss by forcing
            // an IDR on the next encoded frame.
            force_idr_hint.store(true, Ordering::Relaxed);
        }

        stats.flush_if_due(signaling).await;
    }

    capture_task.abort();
    encode_task.abort();

    Ok(())
}

pub(super) async fn run_media_foundation_screen_sender(
    args: ScreenSenderArgs<'_>,
) -> Result<(), String> {
    let ScreenSenderArgs {
        signaling,
        peer,
        track,
        preset,
        rtcp_feedback,
        mut stream_profile_rx,
        mut bitrate_override_rx,
        mut fps_tier_override_rx,
        activity_state,
        frame_emission_paused,
        privacy_filter,
    } = args;
    let SenderInitState {
        initial_config,
        adaptive_tx,
        adaptive_rx,
        force_idr_hint,
        capture_tx,
        mut capture_rx,
    } = init_sender_state(&stream_profile_rx, preset);
    let mut payloader = H264Payloader::default();
    let mut stream_ssrc: u32 = 0;
    let mut negotiated_ssrc: Option<u32> = None;
    let mut negotiated_payload_type: Option<u8> = None;
    let mut cached_sps: Option<Vec<u8>> = None;
    let mut cached_pps: Option<Vec<u8>> = None;
    let mut seq: u16 = 1;
    let stream_clock_start = Instant::now();
    let mut last_rtp_ts: u32 = 0;
    let mut stats = StreamStatsWindow::new();
    let (encoded_tx, mut encoded_rx) = broadcast::channel::<EncodedScreenFrame>(2);

    let capture_peer = Arc::clone(peer);
    let capture_track = Arc::clone(track);
    let capture_cfg_rx = adaptive_rx.clone();
    let capture_paused = Arc::clone(&frame_emission_paused);
    let capture_privacy_filter = privacy_filter.clone();
    let capture_task = tokio::spawn(async move {
        #[cfg(windows)]
        let _timer_resolution_guard = WindowsTimerResolutionGuard::new(1);

        let mut capturer = match create_default_capturer() {
            Ok(capturer) => capturer,
            Err(err) => {
                tracing::warn!("DXGI capturer init failed: {err}");
                return;
            }
        };

        let scale_target = resolve_scale_request();
        let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;
        let mut frame_counter: u64 = 0;
        let mut next_capture_deadline = Instant::now() + frame_interval_for_target_fps(initial_config.target_fps);

        loop {
            let capture_config = *capture_cfg_rx.borrow();
            match capture_peer.connection_state() {
                RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
                _ => {}
            }

            // Pause demandée par le viewer (transfert en cours).
            if capture_paused.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(150)).await;
                next_capture_deadline = Instant::now() + frame_interval_for_target_fps(capture_config.target_fps);
                continue;
            }

            if !stream_is_ready(&capture_peer, &capture_track).await {
                tokio::time::sleep(Duration::from_millis(80)).await;
                continue;
            }

            let capture_start = Instant::now();
            let mut reused_last_frame = false;

            let (width, height, bgra_frame) = match capture_primary_screen_even_bgra(
                &mut *capturer,
                scale_target,
            ) {
                Ok(Some((w, h, mut frame))) => {
                    // Privacy blur — applied before NV12 conversion / MF encode.
                    if let Some(ref filter) = capture_privacy_filter {
                        if let Ok(mut guard) = filter.lock() {
                            guard.process_frame(&mut frame, w as u32, h as u32);
                        }
                    }
                    let arc = Arc::new(frame);
                    last_capture = Some((w, h, Arc::clone(&arc)));
                    (w, h, arc)
                }
                Ok(None) => {
                    if let Some((w, h, ref arc)) = last_capture {
                        reused_last_frame = true;
                        (w, h, Arc::clone(arc))
                    } else {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        continue;
                    }
                }
                Err(err) => {
                    tracing::warn!("Screen capture failed: {err}");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            };

            frame_counter = frame_counter.saturating_add(1);
            let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;

            let frame = CapturedScreenFrame {
                width,
                height,
                bgra_frame,
                reused_last_frame,
                capture_ms,
                frame_counter,
            };

            let _ = capture_tx.send(frame);

            pace_capture_loop(&mut next_capture_deadline, capture_config.target_fps).await;
        }
    });

    let mut encode_cfg_rx = adaptive_rx.clone();
    let force_idr_hint_for_encode = Arc::clone(&force_idr_hint);
    let encode_task = tokio::spawn(async move {
        let mut active_config = *encode_cfg_rx.borrow();
        let mut worker = match MediaFoundationEncoderWorker::new(
            0,
            0,
            active_config.target_fps.max(1),
            active_config.bitrate_bps,
        ) {
            Ok(worker) => worker,
            Err(err) => {
                tracing::warn!("Media Foundation worker init failed: {err}");
                return;
            }
        };

        let mut last_dimensions: Option<(usize, usize)> = None;
        let mut dropped_capture_frames: u64 = 0;

        loop {
            let Some(frame) = recv_latest_broadcast(&mut capture_rx, &mut dropped_capture_frames).await else {
                break;
            };

            let dropped_before_encode = std::mem::take(&mut dropped_capture_frames);

            let latest_config = *encode_cfg_rx.borrow_and_update();
            if latest_config != active_config {
                // R4: Only recreate the encoder on significant bitrate changes (>30%).
                // Small adaptive adjustments skip the expensive MFT teardown/rebuild
                // which causes visible 50-200ms hiccups.
                let bitrate_ratio = latest_config.bitrate_bps as f64
                    / active_config.bitrate_bps.max(1) as f64;
                let fps_delta = latest_config
                    .target_fps
                    .abs_diff(active_config.target_fps);
                let significant_change = bitrate_ratio < 0.7
                    || bitrate_ratio > 1.43
                    || fps_delta >= 8;

                if significant_change {
                    match MediaFoundationEncoderWorker::new(
                        0,
                        0,
                        latest_config.target_fps.max(1),
                        latest_config.bitrate_bps,
                    ) {
                        Ok(new_worker) => {
                            worker = new_worker;
                            tracing::info!(
                                "Media Foundation adaptive reconfigure: {} FPS, {:.2} Mbps",
                                latest_config.target_fps,
                                latest_config.bitrate_bps as f64 / 1_000_000.0
                            );
                        }
                        Err(err) => {
                            tracing::warn!("Media Foundation reconfigure failed: {err}");
                        }
                    }
                }
                active_config = latest_config;
            }

            if last_dimensions != Some((frame.width, frame.height)) {
                last_dimensions = Some((frame.width, frame.height));
                tracing::info!(
                    "Media Foundation H264 encoder configured at {}x{}",
                    frame.width, frame.height
                );
            }

            // Keep decoder recovery fast: IDR at startup, every ~2 seconds,
            // and immediately when RTCP PLI/FIR asks for a refresh.
            let keyframe_interval = (active_config.target_fps.max(1) as u64).saturating_mul(2);
            let force_keyframe = frame.frame_counter == 1
                || (keyframe_interval > 0 && frame.frame_counter % keyframe_interval == 0)
                || force_idr_hint_for_encode.swap(false, Ordering::Relaxed);

            let width = frame.width;
            let height = frame.height;
            let bgra_frame = Arc::clone(&frame.bgra_frame);
            let encode_start = Instant::now();

            let join_result = tokio::task::spawn_blocking(move || {
                let result = worker.encode_bgra(width, height, bgra_frame, force_keyframe);
                (result, worker)
            })
            .await;

            let (encoded_units_result, active_worker) = match join_result {
                Ok(value) => value,
                Err(e) => {
                    tracing::warn!("spawn_blocking failed: {e}");
                    break;
                }
            };
            worker = active_worker;

            let encoded_units = match encoded_units_result {
                Ok(units) => units,
                Err(err) => {
                    tracing::warn!("Media Foundation encode failed: {err}");
                    continue;
                }
            };

            if encoded_units.is_empty() || encoded_units.iter().all(|u| u.data.is_empty()) {
                continue;
            }

            let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;
            let packet = EncodedScreenFrame {
                width: frame.width,
                height: frame.height,
                reused_last_frame: frame.reused_last_frame,
                capture_ms: frame.capture_ms,
                encode_ms,
                frame_counter: frame.frame_counter,
                dropped_before_encode,
                encoded_units: encoded_units
                    .into_iter()
                    .filter_map(|unit| if unit.data.is_empty() { None } else { Some(unit.data) })
                    .collect(),
            };

            let _ = encoded_tx.send(packet);
        }
    });

    let mut dropped_encoded_frames: u64 = 0;
    let mut controller = AdaptiveRateController::new(initial_config);
    let mut last_rtcp_snapshot = rtcp_feedback.snapshot();
    let mut cpu_sampler = CpuUsageSampler::new();
    let mut network_pressure_until = Instant::now();
    let mut current_fps_tier = FpsTier::Normal;
    let mut no_output_since: Option<Instant> = None;
    let first_frame_deadline = Instant::now() + Duration::from_secs(2);
    let mut has_sent_video_frame = false;
    loop {
        let cpu_usage = cpu_sampler.sample();
        let user_active = activity_state.is_active_recent(1500);
        let network_constrained = Instant::now() <= network_pressure_until;
        let auto_fps_tier = choose_auto_fps_tier(cpu_usage, user_active, network_constrained);
        let requested_fps_tier = fps_tier_override_rx
            .borrow_and_update()
            .as_ref()
            .copied()
            .unwrap_or(auto_fps_tier);
        if requested_fps_tier != current_fps_tier {
            tracing::info!(
                "Media Foundation FPS tier -> {:?} (cpu={:.1}%, user_active={}, network_constrained={})",
                requested_fps_tier,
                cpu_usage,
                user_active,
                network_constrained
            );
            current_fps_tier = requested_fps_tier;
        }

        let profile_limit = profile_target_for_preset(*stream_profile_rx.borrow_and_update(), preset);
        let bitrate_override = *bitrate_override_rx.borrow_and_update();
        let effective_limit = apply_external_limits(profile_limit, current_fps_tier, bitrate_override);
        if let Some(updated) = controller.apply_profile_limit(effective_limit) {
            let _ = adaptive_tx.send(updated);
            tracing::info!(
                "Media Foundation profile target: {} FPS, {:.2} Mbps",
                updated.target_fps,
                updated.bitrate_bps as f64 / 1_000_000.0
            );
        }

        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

        let frame = match tokio::time::timeout(
            Duration::from_millis(500),
            recv_latest_broadcast(&mut encoded_rx, &mut dropped_encoded_frames),
        )
        .await
        {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) => {
                if !has_sent_video_frame && Instant::now() >= first_frame_deadline {
                    capture_task.abort();
                    encode_task.abort();
                    return Err(
                        "Media Foundation timed out waiting for first encoded frame; triggering software fallback"
                            .to_string(),
                    );
                }
                continue;
            }
        };

        let dropped_before_send = std::mem::take(&mut dropped_encoded_frames);

        if !stream_is_ready(peer, track).await {
            tokio::time::sleep(Duration::from_millis(60)).await;
            continue;
        }

        if negotiated_payload_type.is_none() {
            negotiated_payload_type = resolve_h264_payload_type(peer).await;
            if let Some(pt) = negotiated_payload_type {
                tracing::info!("Negotiated H264 RTP payload type: {pt}");
            } else {
                tracing::warn!(
                    "Could not resolve H264 RTP payload type from SDP yet; defaulting to 96 until available"
                );
            }
        }

        let payload_type = negotiated_payload_type.unwrap_or(96);

        if negotiated_ssrc.is_none() {
            negotiated_ssrc = resolve_video_ssrc(peer).await;
            stream_ssrc = negotiated_ssrc.unwrap_or_else(derive_stream_ssrc);
            tracing::info!("Using video SSRC: {stream_ssrc}");
        }

        let loop_start = Instant::now();
        let mut frame_ts = rtp_timestamp_90khz_from_instant(&stream_clock_start);
        if frame_ts <= last_rtp_ts {
            frame_ts = last_rtp_ts.wrapping_add(1);
        }
        last_rtp_ts = frame_ts;
        let mut frame_sent = false;
        let mut send_error = false;
        let mut total_payload_bytes = 0usize;
        let mut total_fragments = 0usize;
        let mut nal_summary = NalSummary::default();

        let units_len = frame.encoded_units.len();
        for (unit_index, unit_data) in frame.encoded_units.into_iter().enumerate() {
            let raw_nalus = split_annexb_nalus(&unit_data);
            let (nalus, has_idr) =
                reorder_and_cache_sps_pps(raw_nalus, &mut cached_sps, &mut cached_pps);
            let unit_summary = summarize_nalus(&nalus);
            nal_summary.nalus = nal_summary.nalus.saturating_add(unit_summary.nalus);
            nal_summary.has_sps |= unit_summary.has_sps;
            nal_summary.has_pps |= unit_summary.has_pps;
            nal_summary.has_idr |= unit_summary.has_idr;

            let last_unit = unit_index + 1 == units_len;

            // At most 2 prefix NALs: SPS + PPS. Avoids reallocation in hot path.
            let mut prefix: Vec<Bytes> = Vec::with_capacity(2);
            if has_idr {
                if !unit_summary.has_sps {
                    if let Some(sps) = cached_sps.as_deref() {
                        prefix.push(Bytes::copy_from_slice(sps));
                    }
                }
                if !unit_summary.has_pps {
                    if let Some(pps) = cached_pps.as_deref() {
                        prefix.push(Bytes::copy_from_slice(pps));
                    }
                }
            }

            let total_nals_to_send = prefix.len() + nalus.len();
            let mut nal_cursor = 0usize;

            let mut ctx = RtpEmitCtx {
                payloader: &mut payloader,
                track: &track,
                seq: &mut seq,
                payload_type,
                frame_ts,
                stream_ssrc,
            };

            for bytes in &prefix {
                nal_cursor += 1;
                total_payload_bytes = total_payload_bytes.saturating_add(bytes.len());
                let last_nal = last_unit && (nal_cursor == total_nals_to_send);
                let out = emit_nal_as_rtp(bytes, last_nal, &mut ctx, "media_foundation").await;
                total_fragments = total_fragments.saturating_add(out.fragments);
                if out.fragments > 0 { frame_sent = true; }
                if out.send_error { send_error = true; break; }
            }

            if !send_error {
                for nal in nalus.iter() {
                    if nal.is_empty() {
                        continue;
                    }
                    total_payload_bytes = total_payload_bytes.saturating_add(nal.len());
                    let nal_bytes = Bytes::copy_from_slice(nal);
                    nal_cursor += 1;
                    let last_nal = last_unit && (nal_cursor == total_nals_to_send);
                    let out = emit_nal_as_rtp(&nal_bytes, last_nal, &mut ctx, "media_foundation").await;
                    total_fragments = total_fragments.saturating_add(out.fragments);
                    if out.fragments > 0 { frame_sent = true; }
                    if out.send_error { send_error = true; break; }
                }
            }
        }

        if frame_sent {
            has_sent_video_frame = true;
            no_output_since = None;
            stats.record_frame(total_payload_bytes.max(1));
            if frame.frame_counter % preset.target_fps.max(1) as u64 == 0 {
                tracing::info!(
                    "Sent native MF H264 frame {} ({} bytes payload)",
                    frame.frame_counter, total_payload_bytes
                );
                vlog!(
                    "mf pipeline: {}x{} reuse_last={} capture={:.2}ms encode={:.2}ms total={:.2}ms nalus={} sps={} pps={} idr={} frags={} bytes={} drop_cap={} drop_enc={}",
                    frame.width,
                    frame.height,
                    frame.reused_last_frame,
                    frame.capture_ms,
                    frame.encode_ms,
                    loop_start.elapsed().as_secs_f64() * 1000.0,
                    nal_summary.nalus,
                    nal_summary.has_sps,
                    nal_summary.has_pps,
                    nal_summary.has_idr,
                    total_fragments,
                    total_payload_bytes,
                    frame.dropped_before_encode,
                    dropped_before_send,
                );
            }
        } else {
            let now = Instant::now();
            let started = no_output_since.get_or_insert(now);
            if now.duration_since(*started) >= Duration::from_secs(3) {
                capture_task.abort();
                encode_task.abort();
                return Err(
                    "Media Foundation produced no RTP output for >3s; triggering software fallback"
                        .to_string(),
                );
            }
        }

        let rtcp_delta = collect_rtcp_delta(&rtcp_feedback, &mut last_rtcp_snapshot);
        stats.record_pipeline_sample(
            frame.capture_ms,
            frame.encode_ms,
            frame.dropped_before_encode,
            dropped_before_send,
            rtcp_delta,
        );
        if send_error
            || dropped_before_send > 0
            || rtcp_delta.nack >= 3
            || rtcp_delta.pli > 0
            || rtcp_delta.fir > 0
        {
            network_pressure_until = Instant::now() + Duration::from_secs(2);
        }
        if let Some(next) = controller.on_feedback(AdaptiveFeedback {
            dropped_before_encode: frame.dropped_before_encode,
            dropped_before_send,
            send_error,
            rtcp_nack_delta: rtcp_delta.nack,
            rtcp_pli_delta: rtcp_delta.pli,
            rtcp_fir_delta: rtcp_delta.fir,
            rtcp_feedback_stale: rtcp_delta.total > 0 && rtcp_delta.feedback_stale,
        }) {
            let _ = adaptive_tx.send(next);
            tracing::info!(
                "Media Foundation adaptive target: {} FPS, {:.2} Mbps",
                next.target_fps,
                next.bitrate_bps as f64 / 1_000_000.0
            );
        }

        if rtcp_delta.pli > 0 || rtcp_delta.fir > 0 {
            // Viewer requested a decoder refresh; force an IDR on next encode.
            force_idr_hint.store(true, Ordering::Relaxed);
        }

        stats.flush_if_due(signaling).await;
    }

    capture_task.abort();
    encode_task.abort();

    Ok(())
}

fn rtp_timestamp_90khz_from_instant(start: &Instant) -> u32 {
    // RTP video timestamps are based on a 90kHz clock.
    // Using a monotonic Instant keeps timestamps aligned with real time even when
    // capture/encode time varies (prevents jittery playback).
    let nanos = start.elapsed().as_nanos();
    let ticks = (nanos.saturating_mul(90_000)) / 1_000_000_000;
    ticks as u32
}

pub(super) async fn run_ffmpeg_rtp_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    backend: VideoEncoderBackend,
    preset: VideoEncoderPreset,
    frame_emission_paused: Arc<AtomicBool>,
) -> Result<(), String> {
    let frame_interval = frame_interval_for(preset);
    #[cfg(windows)]
    let _timer_resolution_guard = WindowsTimerResolutionGuard::new(1);

    let mut bridge: Option<FfmpegRtpBridge> = None;
    let mut active_dimensions: Option<(usize, usize)> = None;
    let mut stats = StreamStatsWindow::new();
    let mut negotiated_payload_type: Option<u8> = None;
    let mut capturer = create_default_capturer()?;
    let scale_target = resolve_scale_request();
    let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;
    let mut frame_counter: u64 = 0;
    let mut next_capture_deadline = Instant::now() + frame_interval;

    loop {
        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

        // Pause demandée par le viewer (transfert en cours).
        if frame_emission_paused.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(150)).await;
            next_capture_deadline = Instant::now() + frame_interval;
            continue;
        }

        if !stream_is_ready(peer, track).await {
            tokio::time::sleep(Duration::from_millis(80)).await;
            continue;
        }

        if negotiated_payload_type.is_none() {
            negotiated_payload_type = resolve_h264_payload_type(peer).await;
            if let Some(pt) = negotiated_payload_type {
                tracing::info!("Negotiated H264 RTP payload type: {pt}");
            }
        }

        let payload_type = negotiated_payload_type.unwrap_or(96);

        let capture_start = Instant::now();
        let mut reused_last_frame = false;
        let (width, height, bgra_frame) =
            match capture_primary_screen_even_bgra(&mut *capturer, scale_target)? {
                Some((w, h, frame)) => {
                    let arc = Arc::new(frame);
                    last_capture = Some((w, h, Arc::clone(&arc)));
                    (w, h, arc)
                }
                None => {
                    if let Some((w, h, ref arc)) = last_capture {
                        reused_last_frame = true;
                        (w, h, Arc::clone(arc))
                    } else {
                        tokio::time::sleep(Duration::from_millis(4)).await;
                        continue;
                    }
                }
            };
        let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;
        frame_counter += 1;
        if active_dimensions != Some((width, height)) {
            if let Some(existing) = bridge.as_mut() {
                existing.shutdown().await;
            }

            bridge = Some(
                FfmpegRtpBridge::spawn(backend, width, height, preset, payload_type).await?,
            );
            active_dimensions = Some((width, height));
            tracing::info!(
                "Reconfigured FFmpeg RTP bridge for {}x{} with backend {}",
                width,
                height,
                backend.label()
            );
        }

        let active_bridge = bridge
            .as_mut()
            .ok_or_else(|| "FFmpeg bridge unavailable after spawn".to_string())?;

        let bridge_write_start = Instant::now();
        active_bridge.write_frame(&bgra_frame).await?;
        let bridge_write_ms = bridge_write_start.elapsed().as_secs_f64() * 1000.0;
        if frame_counter % preset.target_fps.max(1) as u64 == 0 {
            tracing::info!(
                "Captured {} frames via DXGI for native->FFmpeg bridge at {}x{}",
                frame_counter, width, height
            );
            vlog!(
                "ffmpeg pipeline: {}x{} reuse_last={} capture={:.2}ms bridge_write={:.2}ms backend={}",
                width,
                height,
                reused_last_frame,
                capture_ms,
                bridge_write_ms,
                backend.label(),
            );
        }

        let drain_start = Instant::now();
        drain_ffmpeg_packets(track, active_bridge, &mut stats).await?;
        vlog!(
            "ffmpeg drain: elapsed={:.2}ms",
            drain_start.elapsed().as_secs_f64() * 1000.0
        );
        stats.flush_if_due(signaling).await;
        pace_capture_loop(&mut next_capture_deadline, preset.target_fps.max(1)).await;
    }

    if let Some(existing) = bridge.as_mut() {
        existing.shutdown().await;
    }

    Ok(())
}

async fn drain_ffmpeg_packets(
    track: &Arc<TrackLocalStaticRTP>,
    bridge: &mut FfmpegRtpBridge,
    stats: &mut StreamStatsWindow,
) -> Result<(), String> {
    let mut packet_buffer = vec![0u8; 64 * 1024];
    let mut idle_polls = 0;

    loop {
        match bridge.try_read_packet(&mut packet_buffer).await? {
            Some(size) => {
                idle_polls = 0;

                let mut raw = &packet_buffer[..size];
                let packet =
                    Packet::unmarshal(&mut raw).map_err(|err| format!("rtp parse failed: {err}"))?;

                if track.write_rtp(&packet).await.is_err() {
                    break;
                }

                stats.record_rtp_packet(&packet);
            }
            None => {
                idle_polls += 1;
                if idle_polls >= 2 {
                    break;
                }
            }
        }
    }

    Ok(())
}

async fn stream_is_ready(peer: &Arc<RTCPeerConnection>, track: &Arc<TrackLocalStaticRTP>) -> bool {
    let _ = track;
    // Some systems report binding paused for too long even after negotiation succeeds,
    // which can stall the sender loop and leave only the startup preview visible.
    peer.connection_state() == RTCPeerConnectionState::Connected
}

pub(super) fn frame_interval_for(preset: VideoEncoderPreset) -> Duration {
    Duration::from_secs_f64(1.0 / preset.target_fps.max(1) as f64)
}
