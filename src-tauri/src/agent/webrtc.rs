use serde_json::Value;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch, Mutex};

#[cfg(windows)]
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod, TIMERR_NOERROR};

use super::desktop_duplication::{DesktopFrame, DxgiDesktopDuplicator};
use super::media_foundation_encoder::MediaFoundationEncoderWorker;
use super::input_handler::InputHandler;
use super::signaling::SignalingClient;
use super::video_encoder::{
    FfmpegRtpBridge, VideoEncoderBackend, VideoEncoderPreset, VideoEncoderSelection,
};
use bytes::Bytes;
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;
use rtp::codecs::h264::H264Payloader;
use rtp::packet::Packet;
use rtp::packetizer::Payloader;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_credential_type::RTCIceCredentialType;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IceServerConfig {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
}

fn default_ice_servers() -> Vec<IceServerConfig> {
    vec![
        IceServerConfig {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            username: None,
            credential: None,
        },
        IceServerConfig {
            urls: vec![
                "turn:openrelay.metered.ca:80".to_owned(),
                "turn:openrelay.metered.ca:443".to_owned(),
                "turns:openrelay.metered.ca:443".to_owned(),
            ],
            username: Some("openrelayproject".to_owned()),
            credential: Some("openrelayproject".to_owned()),
        },
    ]
}

fn read_env_or_local(key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    // Standard command (`npm run tauri dev/build`) may not inject process env vars.
    // Fall back to reading local .env files from common working directories.
    let candidates = [Path::new(".env.local"), Path::new("../.env.local")];

    for path in candidates {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((name, value)) = line.split_once('=') else {
                continue;
            };

            if name.trim() != key {
                continue;
            }

            let mut parsed = value.trim().to_string();
            if parsed.len() >= 2 {
                let quoted_with_single = parsed.starts_with('\'') && parsed.ends_with('\'');
                let quoted_with_double = parsed.starts_with('"') && parsed.ends_with('"');
                if quoted_with_single || quoted_with_double {
                    parsed = parsed[1..parsed.len() - 1].to_string();
                }
            }

            if !parsed.is_empty() {
                return Some(parsed);
            }
        }
    }

    None
}

fn to_rtc_ice_servers(input: &[IceServerConfig]) -> Vec<RTCIceServer> {
    input
        .iter()
        .filter_map(|server| {
            // `webrtc-rs` may fail peer initialization with `turns:` URLs on some builds.
            // Keep STUN/TURN entries and drop only unsupported TURNS entries on the agent side
            // so SDP answer generation is never blocked.
            let urls: Vec<String> = server
                .urls
                .iter()
                .filter_map(|url| {
                    let trimmed = url.trim();
                    if trimmed.is_empty() || trimmed.starts_with("turns:") {
                        return None;
                    }
                    Some(trimmed.to_string())
                })
                .collect();

            if urls.is_empty() {
                return None;
            }

            let has_turn = urls
                .iter()
                .any(|url| url.starts_with("turn:"));
            let username = server
                .username
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let credential = server
                .credential
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();

            if has_turn && (username.is_empty() || credential.is_empty()) {
                eprintln!(
                    "Skipping TURN ICE server without credentials: {:?}",
                    urls
                );
                return None;
            }

            let mut rtc_ice = RTCIceServer {
                urls,
                username,
                credential,
                ..Default::default()
            };

            if has_turn {
                rtc_ice.credential_type = RTCIceCredentialType::Password;
            }

            Some(rtc_ice)
        })
        .collect()
}

fn parse_ice_servers_from_json(raw: &str) -> Option<Vec<IceServerConfig>> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let array = if let Some(items) = value.as_array() {
        items.clone()
    } else {
        value
            .get("iceServers")
            .and_then(serde_json::Value::as_array)
            .cloned()?
    };

    let mut servers = Vec::new();
    for item in array {
        let Some(obj) = item.as_object() else {
            continue;
        };

        let urls = if let Some(urls_array) = obj.get("urls").and_then(serde_json::Value::as_array) {
            urls_array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        } else if let Some(single) = obj.get("urls").and_then(serde_json::Value::as_str) {
            vec![single.to_string()]
        } else if let Some(single) = obj.get("url").and_then(serde_json::Value::as_str) {
            vec![single.to_string()]
        } else {
            Vec::new()
        };

        if urls.is_empty() {
            continue;
        }

        let username = obj
            .get("username")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string());
        let credential = obj
            .get("credential")
            .or_else(|| obj.get("password"))
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string());

        servers.push(IceServerConfig {
            urls,
            username,
            credential,
        });
    }

    if servers.is_empty() {
        None
    } else {
        Some(servers)
    }
}

fn load_ice_servers_from_env() -> Option<Vec<IceServerConfig>> {
    // Supports either:
    // - LUMIERE_ICE_SERVERS='[{"urls":["stun:..." ]},{"urls":["turn:..."],"username":"u","credential":"p"}]'
    // - LUMIERE_ICE_SERVERS='stun:stun.l.google.com:19302,turn:turn.example.com:3478'
    let raw = read_env_or_local("LUMIERE_ICE_SERVERS").map(|s| s.trim().to_string());
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return None;
    };

    if raw.starts_with('[') {
        return parse_ice_servers_from_json(&raw);
    }

    let urls: Vec<String> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if urls.is_empty() {
        return None;
    }

    Some(vec![IceServerConfig {
        urls,
        username: None,
        credential: None,
    }])
}

async fn load_ice_servers_from_metered() -> Option<Vec<IceServerConfig>> {
    let domain = read_env_or_local("LUMIERE_METERED_DOMAIN")?
        .trim()
        .to_string();
    let api_key = read_env_or_local("LUMIERE_METERED_API_KEY")?
        .trim()
        .to_string();
    if domain.is_empty() || api_key.is_empty() {
        return None;
    }

    let endpoint = format!(
        "https://{domain}/api/v1/turn/credentials?apiKey={api_key}",
        domain = domain,
        api_key = api_key
    );

    let response = reqwest::Client::new()
        .get(endpoint)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .ok()?;
    let body = response.text().await.ok()?;
    parse_ice_servers_from_json(&body)
}

pub async fn resolve_ice_servers_for_frontend() -> Vec<IceServerConfig> {
    if let Some(env_servers) = load_ice_servers_from_env() {
        return env_servers;
    }
    if let Some(metered_servers) = load_ice_servers_from_metered().await {
        return metered_servers;
    }
    default_ice_servers()
}

async fn resolve_ice_servers_for_peer() -> Vec<RTCIceServer> {
    let servers = resolve_ice_servers_for_frontend().await;
    to_rtc_ice_servers(&servers)
}
use webrtc::track::track_local::TrackLocalWriter;
use webrtc_util::marshal::Unmarshal;

fn env_flag_true(key: &str) -> bool {
    let Ok(value) = env::var(key) else {
        return false;
    };
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn video_debug_enabled() -> bool {
    env_flag_true("LUMIERE_VIDEO_DEBUG")
}

fn derive_stream_ssrc() -> u32 {
    let pid = std::process::id() as u64;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    let mixed = now.as_nanos() as u64 ^ (pid.rotate_left(13));
    let ssrc = (mixed as u32) | 1;
    if ssrc == 0 { 1 } else { ssrc }
}

fn parse_h264_payload_type_from_sdp(sdp: &str) -> Option<u8> {
    for raw in sdp.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("a=rtpmap:") else {
            continue;
        };

        // Examples:
        // a=rtpmap:102 H264/90000
        // a=rtpmap:96 H264/90000
        let mut parts = rest.split_whitespace();
        let pt_str = parts.next()?;
        let codec_str = parts.next()?;

        let Some((codec, clock)) = codec_str.split_once('/') else {
            continue;
        };

        if codec.eq_ignore_ascii_case("H264") && clock == "90000" {
            if let Ok(pt) = pt_str.parse::<u8>() {
                return Some(pt);
            }
        }
    }

    None
}

fn parse_first_ssrc_from_sdp(sdp: &str) -> Option<u32> {
    for raw in sdp.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("a=ssrc:") else {
            continue;
        };
        // Example: a=ssrc:123456789 cname:...
        let mut chars = rest.chars();
        let mut num = String::new();
        while let Some(ch) = chars.next() {
            if ch.is_ascii_digit() {
                num.push(ch);
            } else {
                break;
            }
        }
        if num.is_empty() {
            continue;
        }
        if let Ok(value) = num.parse::<u32>() {
            if value != 0 {
                return Some(value);
            }
        }
    }
    None
}

async fn resolve_h264_payload_type(peer: &Arc<RTCPeerConnection>) -> Option<u8> {
    let local = peer.local_description().await?;
    parse_h264_payload_type_from_sdp(&local.sdp)
}

async fn resolve_video_ssrc(peer: &Arc<RTCPeerConnection>) -> Option<u32> {
    let local = peer.local_description().await?;
    parse_first_ssrc_from_sdp(&local.sdp)
}

fn reorder_and_cache_sps_pps<'a>(
    nalus: Vec<&'a [u8]>,
    cached_sps: &mut Option<Vec<u8>>,
    cached_pps: &mut Option<Vec<u8>>,
) -> (Vec<&'a [u8]>, bool) {
    let mut sps: Vec<&'a [u8]> = Vec::new();
    let mut pps: Vec<&'a [u8]> = Vec::new();
    let mut others: Vec<&'a [u8]> = Vec::new();
    let mut has_idr = false;

    for nal in nalus {
        let Some(&first) = nal.first() else {
            continue;
        };
        let nal_type = first & 0x1f;
        match nal_type {
            5 => {
                has_idr = true;
                others.push(nal);
            }
            7 => {
                *cached_sps = Some(nal.to_vec());
                sps.push(nal);
            }
            8 => {
                *cached_pps = Some(nal.to_vec());
                pps.push(nal);
            }
            _ => others.push(nal),
        }
    }

    let mut ordered = Vec::with_capacity(sps.len() + pps.len() + others.len() + 2);

    ordered.extend_from_slice(&sps);
    ordered.extend_from_slice(&pps);
    ordered.extend_from_slice(&others);
    (ordered, has_idr)
}

macro_rules! vlog {
    ($($arg:tt)*) => {{
        if video_debug_enabled() {
            println!("[video][dbg] {}", format_args!($($arg)*));
        }
    }};
}

#[derive(Default, Clone, Copy)]
struct NalSummary {
    nalus: usize,
    has_sps: bool,
    has_pps: bool,
    has_idr: bool,
}

fn split_annexb_nalus(data: &[u8]) -> Vec<&[u8]> {
    // Extract NAL units from Annex-B formatted stream (start codes 0x000001 or 0x00000001).
    // Returned slices exclude the start code.
    let mut nalus = Vec::new();
    let mut i = 0usize;
    let len = data.len();

    let find_start_code = |from: usize| -> Option<(usize, usize)> {
        let mut j = from;
        while j + 3 < len {
            if data[j] == 0 && data[j + 1] == 0 {
                if data[j + 2] == 1 {
                    return Some((j, 3));
                }
                if j + 3 < len && data[j + 2] == 0 && data[j + 3] == 1 {
                    return Some((j, 4));
                }
            }
            j += 1;
        }
        None
    };

    while let Some((sc_pos, sc_len)) = find_start_code(i) {
        let nal_start = sc_pos + sc_len;
        if let Some((next_sc_pos, _)) = find_start_code(nal_start) {
            if next_sc_pos > nal_start {
                nalus.push(&data[nal_start..next_sc_pos]);
            }
            i = next_sc_pos;
        } else {
            if nal_start < len {
                nalus.push(&data[nal_start..len]);
            }
            break;
        }
    }

    if nalus.is_empty() && !data.is_empty() {
        // Fallback: try AVCC / length-prefixed NAL units (4-byte big-endian lengths).
        let mut offset = 0usize;
        while offset + 4 <= len {
            let size = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;

            if size == 0 || offset + size > len {
                nalus.clear();
                break;
            }

            nalus.push(&data[offset..offset + size]);
            offset += size;
        }

        // If parsing failed or produced nothing, assume the input is a single NAL unit.
        if nalus.is_empty() {
            nalus.push(data);
        }
    }

    nalus
}

fn summarize_nalus(nalus: &[&[u8]]) -> NalSummary {
    let mut summary = NalSummary::default();
    summary.nalus = nalus.len();
    for nal in nalus {
        let Some(&first) = nal.first() else {
            continue;
        };
        let nal_type = first & 0x1f;
        match nal_type {
            5 => summary.has_idr = true,
            7 => summary.has_sps = true,
            8 => summary.has_pps = true,
            _ => {}
        }
    }
    summary
}

pub struct AgentWebRtc {
    signaling: Arc<SignalingClient>,
    peer: Arc<RTCPeerConnection>,
    video_track: Arc<TrackLocalStaticRTP>,
    rtcp_feedback: Arc<RtcpFeedbackState>,
    stream_profile_tx: watch::Sender<StreamQualityProfile>,
    pending_remote_ice: Mutex<Vec<RTCIceCandidateInit>>,
}

struct StreamStatsWindow {
    started_at: Instant,
    sent_bytes: usize,
    sent_frames: usize,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdaptiveVideoConfig {
    target_fps: u32,
    bitrate_bps: u32,
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

fn profile_target_for_preset(
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

struct AdaptiveRateController {
    current: AdaptiveVideoConfig,
    ceiling: AdaptiveVideoConfig,
    min_fps: u32,
    min_bitrate_bps: u32,
    last_adjust_at: Instant,
    stress_ema: f64,
}

#[derive(Clone, Copy, Default)]
struct AdaptiveFeedback {
    dropped_before_encode: u64,
    dropped_before_send: u64,
    send_error: bool,
    rtcp_nack_delta: u64,
    rtcp_pli_delta: u64,
    rtcp_fir_delta: u64,
    rtcp_feedback_stale: bool,
}

#[derive(Clone, Copy, Default)]
struct RtcpDelta {
    total: u64,
    nack: u64,
    pli: u64,
    fir: u64,
    feedback_stale: bool,
}

#[derive(Default)]
struct RtcpFeedbackState {
    total_reports: AtomicU64,
    nack_reports: AtomicU64,
    pli_reports: AtomicU64,
    fir_reports: AtomicU64,
    last_feedback_unix_ms: AtomicU64,
}

#[derive(Clone, Copy, Default)]
struct RtcpFeedbackSnapshot {
    total_reports: u64,
    nack_reports: u64,
    pli_reports: u64,
    fir_reports: u64,
    last_feedback_unix_ms: u64,
}

impl RtcpFeedbackState {
    fn mark_feedback_from_packet_text(&self, packet_text: &str) {
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

    fn snapshot(&self) -> RtcpFeedbackSnapshot {
        RtcpFeedbackSnapshot {
            total_reports: self.total_reports.load(Ordering::Relaxed),
            nack_reports: self.nack_reports.load(Ordering::Relaxed),
            pli_reports: self.pli_reports.load(Ordering::Relaxed),
            fir_reports: self.fir_reports.load(Ordering::Relaxed),
            last_feedback_unix_ms: self.last_feedback_unix_ms.load(Ordering::Relaxed),
        }
    }
}

fn collect_rtcp_delta(
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

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

impl AdaptiveRateController {
    fn new(initial: AdaptiveVideoConfig) -> Self {
        Self {
            current: initial,
            ceiling: initial,
            min_fps: 10,
            min_bitrate_bps: 800_000,
            last_adjust_at: Instant::now(),
            stress_ema: 0.0,
        }
    }

    fn on_feedback(&mut self, feedback: AdaptiveFeedback) -> Option<AdaptiveVideoConfig> {
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
            let next_fps = (self.current.target_fps + 2).min(self.ceiling.target_fps.max(self.min_fps));
            let next_bitrate = (self.current.bitrate_bps + 500_000).min(self.ceiling.bitrate_bps);
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

    fn apply_profile_limit(&mut self, limit: AdaptiveVideoConfig) -> Option<AdaptiveVideoConfig> {
        self.ceiling = AdaptiveVideoConfig {
            target_fps: limit.target_fps.max(self.min_fps),
            bitrate_bps: limit.bitrate_bps.max(self.min_bitrate_bps),
        };

        let clamped_fps = self.current.target_fps.min(self.ceiling.target_fps);
        let clamped_bitrate = self.current.bitrate_bps.min(self.ceiling.bitrate_bps);
        self.set_if_changed(clamped_fps, clamped_bitrate)
    }
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
            eprintln!("timeBeginPeriod({period_ms}) failed; frame pacing may jitter under load");
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

impl StreamStatsWindow {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            sent_bytes: 0,
            sent_frames: 0,
        }
    }

    fn record_frame(&mut self, frame_bytes: usize) {
        self.sent_bytes += frame_bytes;
        self.sent_frames += 1;
    }

    fn record_rtp_packet(&mut self, packet: &Packet) {
        self.sent_bytes += packet.payload.len();
        if packet.header.marker {
            self.sent_frames += 1;
        }
    }

    async fn flush_if_due(&mut self, signaling: &Arc<SignalingClient>) {
        let elapsed = self.started_at.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }

        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let mbps = (self.sent_bytes as f64 * 8.0) / (elapsed_sec * 1_000_000.0);
        let fps = self.sent_frames as f64 / elapsed_sec;
        let bytes_per_second = (self.sent_bytes as f64 / elapsed_sec).round() as i64;

        if let Err(err) = signaling
            .send_stream_stats(mbps, fps, bytes_per_second)
            .await
        {
            eprintln!("Failed to send stream stats: {err}");
        }

        self.started_at = Instant::now();
        self.sent_bytes = 0;
        self.sent_frames = 0;
    }
}

impl AgentWebRtc {
    pub async fn new(
        signaling: Arc<SignalingClient>,
        input_handler: Arc<InputHandler>,
        allow_remote_input: bool,
    ) -> Result<Self, String> {
        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .map_err(|e| format!("register_default_codecs failed: {e}"))?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .map_err(|e| format!("register_default_interceptors failed: {e}"))?;

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers: resolve_ice_servers_for_peer().await,
            ..Default::default()
        };

        let peer = Arc::new(
            api.new_peer_connection(config)
                .await
                .map_err(|e| format!("new_peer_connection failed: {e}"))?,
        );

        let signaling_for_ice = Arc::clone(&signaling);
        peer.on_ice_candidate(Box::new(move |candidate| {
            let signaling = Arc::clone(&signaling_for_ice);
            Box::pin(async move {
                let Some(candidate) = candidate else {
                    return;
                };

                match candidate.to_json() {
                    Ok(init) => {
                        let sdp_mid = init.sdp_mid.filter(|mid| !mid.is_empty());
                        let payload = serde_json::json!({
                            "candidate": init.candidate,
                            "sdpMid": sdp_mid,
                            "sdpMLineIndex": init.sdp_mline_index,
                        });

                        if let Err(err) = signaling.send_ice_candidate(payload).await {
                            eprintln!("Failed to send local ICE candidate: {err}");
                        }
                    }
                    Err(err) => {
                        eprintln!("Failed to serialize local ICE candidate: {err}");
                    }
                }
            })
        }));

        peer.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
            let input_handler = Arc::clone(&input_handler);
            Box::pin(async move {
                let label = channel.label().to_string();
                println!("DataChannel recu: {label}");

                let open_label = label.clone();
                channel.on_open(Box::new(move || {
                    Box::pin(async move {
                        println!("DataChannel ouvert: {open_label}");
                    })
                }));

                let close_label = label.clone();
                channel.on_close(Box::new(move || {
                    let close_label = close_label.clone();
                    Box::pin(async move {
                        println!("DataChannel ferme: {close_label}");
                    })
                }));

                if label != "input" {
                    return;
                }

                let message_label = label.clone();
                channel.on_message(Box::new(move |msg: DataChannelMessage| {
                    let input_handler = Arc::clone(&input_handler);
                    let message_label = message_label.clone();
                    Box::pin(async move {
                        if !msg.is_string {
                            return;
                        }

                        let Ok(message) = String::from_utf8(msg.data.to_vec()) else {
                            eprintln!("Message DataChannel invalide sur {message_label}");
                            return;
                        };

                        if !allow_remote_input {
                            println!("Input distant ignore (lecture seule)");
                            return;
                        }

                        input_handler.handle_input(&message);
                    })
                }));
            })
        }));

        let video_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90000,
                channels: 0,
                // Avoid over-constraining the negotiated H264 profile/level.
                // Some hardware encoders may output Main/High; SPS/PPS will carry the true profile.
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1".to_owned(),
                rtcp_feedback: vec![],
            },
            "video".to_owned(),
            "screen".to_owned(),
        ));

        let rtp_sender = peer
            .add_track(
                Arc::clone(&video_track)
                    as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>,
            )
            .await
            .map_err(|e| format!("add_track failed: {e}"))?;

        let rtcp_feedback = Arc::new(RtcpFeedbackState::default());
        let (stream_profile_tx, _) = watch::channel(StreamQualityProfile::Quality);

        println!("WebRTC video track created: screen/H264");

        let rtcp_feedback_for_task = Arc::clone(&rtcp_feedback);
        tokio::spawn(async move {
            while let Ok((packets, _)) = rtp_sender.read_rtcp().await {
                for packet in packets {
                    let packet_text = format!("{:?}", packet);
                    rtcp_feedback_for_task.mark_feedback_from_packet_text(&packet_text);
                }
            }
        });

        let peer_for_state = Arc::clone(&peer);
        peer.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            println!("WebRTC connection state: {state:?}");
            if state == RTCPeerConnectionState::Failed {
                let peer_for_recovery = Arc::clone(&peer_for_state);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if peer_for_recovery.connection_state() == RTCPeerConnectionState::Failed {
                        eprintln!(
                            "WebRTC still failed after delay; waiting for signaling renegotiation"
                        );
                    }
                });
            }
            Box::pin(async {})
        }));

        Ok(Self {
            signaling,
            peer,
            video_track,
            rtcp_feedback,
            stream_profile_tx,
            pending_remote_ice: Mutex::new(Vec::new()),
        })
    }

    pub fn set_stream_profile(&self, profile: StreamQualityProfile) {
        let _ = self.stream_profile_tx.send(profile);
    }

    pub async fn close(&self) {
        if let Err(err) = self.peer.close().await {
            eprintln!("Failed to close WebRTC peer: {err}");
        }
    }

    pub async fn handle_offer(&self, payload: &Value) -> Result<Value, String> {
        let sdp_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if sdp_type != "offer" {
            return Err(format!("Unexpected SDP type: {sdp_type}"));
        }

        let sdp = payload
            .get("sdp")
            .and_then(Value::as_str)
            .ok_or("Missing offer.sdp")?
            .to_string();

        let remote = RTCSessionDescription::offer(sdp)
            .map_err(|e| format!("RTCSessionDescription::offer failed: {e}"))?;

        self.peer
            .set_remote_description(remote)
            .await
            .map_err(|e| format!("set_remote_description failed: {e}"))?;

        // Apply any ICE candidates that arrived before remote description was set.
        let pending_candidates = {
            let mut queue = self.pending_remote_ice.lock().await;
            std::mem::take(&mut *queue)
        };
        for candidate in pending_candidates {
            if let Err(err) = self.peer.add_ice_candidate(candidate).await {
                eprintln!("Failed to apply queued remote ICE candidate: {err}");
            }
        }

        let answer = self
            .peer
            .create_answer(None)
            .await
            .map_err(|e| format!("create_answer failed: {e}"))?;

        self.peer
            .set_local_description(answer)
            .await
            .map_err(|e| format!("set_local_description failed: {e}"))?;

        let local = self
            .peer
            .local_description()
            .await
            .ok_or("local_description unavailable")?;

        Ok(serde_json::json!({
            "type": "answer",
            "sdp": local.sdp,
        }))
    }

    pub fn start_h264_screen_sender(&self) {
        let signaling = Arc::clone(&self.signaling);
        let peer = Arc::clone(&self.peer);
        let track = Arc::clone(&self.video_track);
        let rtcp_feedback = Arc::clone(&self.rtcp_feedback);
        let stream_profile_rx = self.stream_profile_tx.subscribe();

        tokio::spawn(async move {
            let selection = VideoEncoderSelection::resolve();
            println!(
                "Video encoder selected: {} (target={} FPS, bitrate={} Mbps)",
                selection.backend.label(),
                selection.preset.target_fps,
                selection.preset.bitrate_bps as f64 / 1_000_000.0
            );

            let result = match selection.backend {
                VideoEncoderBackend::OpenH264Software => {
                    run_openh264_screen_sender(
                        &signaling,
                        &peer,
                        &track,
                        selection.preset,
                        Arc::clone(&rtcp_feedback),
                        stream_profile_rx.clone(),
                    )
                    .await
                }
                VideoEncoderBackend::MediaFoundationH264 => {
                    match run_media_foundation_screen_sender(
                        &signaling,
                        &peer,
                        &track,
                        selection.preset,
                        Arc::clone(&rtcp_feedback),
                        stream_profile_rx.clone(),
                    )
                    .await
                    {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            eprintln!(
                                "Native Media Foundation H264 encoder failed: {err}. Falling back to software OpenH264."
                            );
                            run_openh264_screen_sender(
                                &signaling,
                                &peer,
                                &track,
                                selection.preset,
                                Arc::clone(&rtcp_feedback),
                                stream_profile_rx.clone(),
                            )
                            .await
                        }
                    }
                }
                backend => {
                    match run_ffmpeg_rtp_screen_sender(
                        &signaling,
                        &peer,
                        &track,
                        backend,
                        selection.preset,
                    )
                    .await
                    {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            eprintln!(
                                "Hardware encoder {} failed: {err}. Falling back to software OpenH264.",
                                backend.label()
                            );
                            run_openh264_screen_sender(
                                &signaling,
                                &peer,
                                &track,
                                selection.preset,
                                Arc::clone(&rtcp_feedback),
                                stream_profile_rx.clone(),
                            )
                            .await
                        }
                    }
                }
            };

            if let Err(err) = result {
                eprintln!("Video sender stopped with error: {err}");
            }
        });
    }

    pub async fn add_ice_candidate(&self, payload: &Value) -> Result<(), String> {
        let candidate = payload
            .get("candidate")
            .and_then(Value::as_str)
            .ok_or("Missing ICE candidate")?
            .to_string();

        let mut init = RTCIceCandidateInit {
            candidate,
            ..Default::default()
        };

        if let Some(mid) = payload.get("sdpMid").and_then(Value::as_str) {
            init.sdp_mid = Some(mid.to_string());
        }

        if let Some(index) = payload.get("sdpMLineIndex").and_then(Value::as_u64) {
            init.sdp_mline_index = Some(index as u16);
        }

        match self.peer.add_ice_candidate(init.clone()).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let msg = err.to_string();
                if msg.to_ascii_lowercase().contains("remote description") {
                    let mut queue = self.pending_remote_ice.lock().await;
                    queue.push(init);
                    return Ok(());
                }
                Err(format!("add_ice_candidate failed: {err}"))
            }
        }
    }
}

async fn run_openh264_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    preset: VideoEncoderPreset,
    rtcp_feedback: Arc<RtcpFeedbackState>,
    mut stream_profile_rx: watch::Receiver<StreamQualityProfile>,
) -> Result<(), String> {
    let initial_profile = *stream_profile_rx.borrow();
    let initial_config = profile_target_for_preset(initial_profile, preset);
    let (adaptive_tx, adaptive_rx) = watch::channel(initial_config);
    let force_idr_hint = Arc::new(AtomicBool::new(false));
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

    let (capture_tx, mut capture_rx) = broadcast::channel::<CapturedScreenFrame>(2);
    let (encoded_tx, mut encoded_rx) = broadcast::channel::<EncodedScreenFrame>(2);

    let capture_peer = Arc::clone(peer);
    let capture_track = Arc::clone(track);
    let capture_cfg_rx = adaptive_rx.clone();
    let capture_task = tokio::spawn(async move {
        #[cfg(windows)]
        let _timer_resolution_guard = WindowsTimerResolutionGuard::new(1);

        let mut capturer = match DxgiDesktopDuplicator::new() {
            Ok(capturer) => capturer,
            Err(err) => {
                eprintln!("DXGI capturer init failed: {err}");
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

            if !stream_is_ready(&capture_peer, &capture_track).await {
                tokio::time::sleep(Duration::from_millis(120)).await;
                continue;
            }

            let capture_start = Instant::now();
            let mut reused_last_frame = false;
            let (width, height, bgra_frame) = match capture_primary_screen_even_bgra(
                &mut capturer,
                scale_target,
            ) {
                Ok(Some((w, h, frame))) => {
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
                    eprintln!("Screen capture failed: {err}");
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
                eprintln!("OpenH264 encoder init failed: {err}");
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
                        println!(
                            "OpenH264 adaptive reconfigure: {} FPS, {:.2} Mbps",
                            active_config.target_fps,
                            active_config.bitrate_bps as f64 / 1_000_000.0
                        );
                    }
                    Err(err) => {
                        eprintln!("OpenH264 reconfigure failed: {err}");
                    }
                }
            }

            if frame.frame_counter % active_config.target_fps.max(1) as u64 == 0 {
                println!(
                    "Captured {} frames via DXGI at {}x{} for software H264 pipeline",
                    frame.frame_counter, frame.width, frame.height
                );
            }

            let keyframe_interval = (active_config.target_fps.max(1) as u64).saturating_mul(5);
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
                    eprintln!("spawn_blocking failed: {e}");
                    break;
                }
            };
            encoder = active_encoder;

            let encoded = match encoded_result {
                Ok(data) => data,
                Err(err) => {
                    eprintln!("H264 encode failed: {err}");
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
    loop {
        let profile_limit = profile_target_for_preset(*stream_profile_rx.borrow_and_update(), preset);
        if let Some(updated) = controller.apply_profile_limit(profile_limit) {
            let _ = adaptive_tx.send(updated);
            println!(
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
                println!("Negotiated H264 RTP payload type: {pt}");
            } else {
                eprintln!(
                    "Could not resolve H264 RTP payload type from SDP yet; defaulting to 96 until available"
                );
            }
        }

        let payload_type = negotiated_payload_type.unwrap_or(96);

        if negotiated_ssrc.is_none() {
            negotiated_ssrc = resolve_video_ssrc(peer).await;
            stream_ssrc = negotiated_ssrc.unwrap_or_else(derive_stream_ssrc);
            println!("Using video SSRC: {stream_ssrc}");
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

            let mut prefix: Vec<Bytes> = Vec::new();
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

            for bytes in prefix {
                nal_cursor += 1;
                total_payload_bytes = total_payload_bytes.saturating_add(bytes.len());
                let payloads = match payloader.payload(1200, &bytes) {
                    Ok(chunks) => chunks,
                    Err(err) => {
                        eprintln!("H264 payload split failed: {err}");
                        continue;
                    }
                };
                total_fragments = total_fragments.saturating_add(payloads.len());
                let last_nal = nal_cursor == total_nals_to_send;
                for (index, fragment) in payloads.iter().enumerate() {
                    let marker = last_nal && (index + 1 == payloads.len());
                    let packet = Packet {
                        header: rtp::header::Header {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker,
                            payload_type,
                            sequence_number: seq,
                            timestamp: frame_ts,
                            ssrc: stream_ssrc,
                            csrc: vec![],
                            extension_profile: 0,
                            extensions: vec![],
                            extensions_padding: 0,
                        },
                        payload: fragment.clone(),
                    };
                    if track.write_rtp(&packet).await.is_err() {
                        send_error = true;
                        break;
                    }
                    frame_sent = true;
                    seq = seq.wrapping_add(1);
                }
            }

            for nal in nalus.iter() {
                if nal.is_empty() {
                    continue;
                }

                total_payload_bytes = total_payload_bytes.saturating_add(nal.len());
                let nal_bytes = Bytes::copy_from_slice(nal);
                let payloads = match payloader.payload(1200, &nal_bytes) {
                    Ok(chunks) => chunks,
                    Err(err) => {
                        eprintln!("H264 payload split failed: {err}");
                        continue;
                    }
                };
                total_fragments = total_fragments.saturating_add(payloads.len());

                nal_cursor += 1;
                let last_nal = nal_cursor == total_nals_to_send;
                for (index, fragment) in payloads.iter().enumerate() {
                    let marker = last_nal && (index + 1 == payloads.len());
                    let packet = Packet {
                        header: rtp::header::Header {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker,
                            payload_type,
                            sequence_number: seq,
                            timestamp: frame_ts,
                            ssrc: stream_ssrc,
                            csrc: vec![],
                            extension_profile: 0,
                            extensions: vec![],
                            extensions_padding: 0,
                        },
                        payload: fragment.clone(),
                    };

                    if track.write_rtp(&packet).await.is_err() {
                        send_error = true;
                        break;
                    }

                    frame_sent = true;
                    seq = seq.wrapping_add(1);
                }
            }

            let send_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
            if frame_sent {
                stats.record_frame(total_payload_bytes.max(1));
                if frame.frame_counter % preset.target_fps.max(1) as u64 == 0 {
                    println!(
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
            println!(
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

async fn run_media_foundation_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    preset: VideoEncoderPreset,
    rtcp_feedback: Arc<RtcpFeedbackState>,
    mut stream_profile_rx: watch::Receiver<StreamQualityProfile>,
) -> Result<(), String> {
    let initial_profile = *stream_profile_rx.borrow();
    let initial_config = profile_target_for_preset(initial_profile, preset);
    let (adaptive_tx, adaptive_rx) = watch::channel(initial_config);
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

    let (capture_tx, mut capture_rx) = broadcast::channel::<CapturedScreenFrame>(2);
    let (encoded_tx, mut encoded_rx) = broadcast::channel::<EncodedScreenFrame>(2);

    let capture_peer = Arc::clone(peer);
    let capture_track = Arc::clone(track);
    let capture_cfg_rx = adaptive_rx.clone();
    let capture_task = tokio::spawn(async move {
        #[cfg(windows)]
        let _timer_resolution_guard = WindowsTimerResolutionGuard::new(1);

        let mut capturer = match DxgiDesktopDuplicator::new() {
            Ok(capturer) => capturer,
            Err(err) => {
                eprintln!("DXGI capturer init failed: {err}");
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

            if !stream_is_ready(&capture_peer, &capture_track).await {
                tokio::time::sleep(Duration::from_millis(80)).await;
                continue;
            }

            let capture_start = Instant::now();
            let mut reused_last_frame = false;

            let (width, height, bgra_frame) = match capture_primary_screen_even_bgra(
                &mut capturer,
                scale_target,
            ) {
                Ok(Some((w, h, frame))) => {
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
                    eprintln!("Screen capture failed: {err}");
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
                eprintln!("Media Foundation worker init failed: {err}");
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
                            println!(
                                "Media Foundation adaptive reconfigure: {} FPS, {:.2} Mbps",
                                latest_config.target_fps,
                                latest_config.bitrate_bps as f64 / 1_000_000.0
                            );
                        }
                        Err(err) => {
                            eprintln!("Media Foundation reconfigure failed: {err}");
                        }
                    }
                }
                active_config = latest_config;
            }

            if last_dimensions != Some((frame.width, frame.height)) {
                last_dimensions = Some((frame.width, frame.height));
                println!(
                    "Media Foundation H264 encoder configured at {}x{}",
                    frame.width, frame.height
                );
            }

            // Some Intel Media Foundation stacks can stall after repeated drain/restart
            // keyframe forcing. Keep forced keyframe only at stream startup.
            let force_keyframe = frame.frame_counter == 1;

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
                    eprintln!("spawn_blocking failed: {e}");
                    break;
                }
            };
            worker = active_worker;

            let encoded_units = match encoded_units_result {
                Ok(units) => units,
                Err(err) => {
                    eprintln!("Media Foundation encode failed: {err}");
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
    loop {
        let profile_limit = profile_target_for_preset(*stream_profile_rx.borrow_and_update(), preset);
        if let Some(updated) = controller.apply_profile_limit(profile_limit) {
            let _ = adaptive_tx.send(updated);
            println!(
                "Media Foundation profile target: {} FPS, {:.2} Mbps",
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
            tokio::time::sleep(Duration::from_millis(60)).await;
            continue;
        }

        if negotiated_payload_type.is_none() {
            negotiated_payload_type = resolve_h264_payload_type(peer).await;
            if let Some(pt) = negotiated_payload_type {
                println!("Negotiated H264 RTP payload type: {pt}");
            } else {
                eprintln!(
                    "Could not resolve H264 RTP payload type from SDP yet; defaulting to 96 until available"
                );
            }
        }

        let payload_type = negotiated_payload_type.unwrap_or(96);

        if negotiated_ssrc.is_none() {
            negotiated_ssrc = resolve_video_ssrc(peer).await;
            stream_ssrc = negotiated_ssrc.unwrap_or_else(derive_stream_ssrc);
            println!("Using video SSRC: {stream_ssrc}");
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

            let mut prefix: Vec<Bytes> = Vec::new();
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
            for bytes in prefix {
                nal_cursor += 1;
                total_payload_bytes = total_payload_bytes.saturating_add(bytes.len());
                let payloads = match payloader.payload(1200, &bytes) {
                    Ok(chunks) => chunks,
                    Err(err) => {
                        eprintln!("H264 payload split failed (Media Foundation): {err}");
                        continue;
                    }
                };
                total_fragments = total_fragments.saturating_add(payloads.len());
                let last_nal = last_unit && (nal_cursor == total_nals_to_send);
                for (index, fragment) in payloads.iter().enumerate() {
                    let marker = last_nal && (index + 1 == payloads.len());
                    let packet = Packet {
                        header: rtp::header::Header {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker,
                            payload_type,
                            sequence_number: seq,
                            timestamp: frame_ts,
                            ssrc: stream_ssrc,
                            csrc: vec![],
                            extension_profile: 0,
                            extensions: vec![],
                            extensions_padding: 0,
                        },
                        payload: fragment.clone(),
                    };
                    if track.write_rtp(&packet).await.is_err() {
                        send_error = true;
                        break;
                    }
                    frame_sent = true;
                    seq = seq.wrapping_add(1);
                }
            }

            for nal in nalus.iter() {
                if nal.is_empty() {
                    continue;
                }

                total_payload_bytes = total_payload_bytes.saturating_add(nal.len());
                let nal_bytes = Bytes::copy_from_slice(nal);
                let payloads = match payloader.payload(1200, &nal_bytes) {
                    Ok(chunks) => chunks,
                    Err(err) => {
                        eprintln!("H264 payload split failed (Media Foundation): {err}");
                        continue;
                    }
                };
                total_fragments = total_fragments.saturating_add(payloads.len());

                nal_cursor += 1;
                let last_nal = last_unit && (nal_cursor == total_nals_to_send);
                for (index, fragment) in payloads.iter().enumerate() {
                    let marker = last_nal && (index + 1 == payloads.len());
                    let packet = Packet {
                        header: rtp::header::Header {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker,
                            payload_type,
                            sequence_number: seq,
                            timestamp: frame_ts,
                            ssrc: stream_ssrc,
                            csrc: vec![],
                            extension_profile: 0,
                            extensions: vec![],
                            extensions_padding: 0,
                        },
                        payload: fragment.clone(),
                    };

                    if track.write_rtp(&packet).await.is_err() {
                        send_error = true;
                        break;
                    }

                    frame_sent = true;
                    seq = seq.wrapping_add(1);
                }
            }
        }

        if frame_sent {
            stats.record_frame(total_payload_bytes.max(1));
            if frame.frame_counter % preset.target_fps.max(1) as u64 == 0 {
                println!(
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
        }

        let rtcp_delta = collect_rtcp_delta(&rtcp_feedback, &mut last_rtcp_snapshot);
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
            println!(
                "Media Foundation adaptive target: {} FPS, {:.2} Mbps",
                next.target_fps,
                next.bitrate_bps as f64 / 1_000_000.0
            );
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

async fn run_ffmpeg_rtp_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    backend: VideoEncoderBackend,
    preset: VideoEncoderPreset,
) -> Result<(), String> {
    let frame_interval = frame_interval_for(preset);
    #[cfg(windows)]
    let _timer_resolution_guard = WindowsTimerResolutionGuard::new(1);

    let mut bridge: Option<FfmpegRtpBridge> = None;
    let mut active_dimensions: Option<(usize, usize)> = None;
    let mut stats = StreamStatsWindow::new();
    let mut negotiated_payload_type: Option<u8> = None;
    let mut capturer = DxgiDesktopDuplicator::new()?;
    let scale_target = resolve_scale_request();
    let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;
    let mut frame_counter: u64 = 0;
    let mut next_capture_deadline = Instant::now() + frame_interval;

    loop {
        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

        if !stream_is_ready(peer, track).await {
            tokio::time::sleep(Duration::from_millis(80)).await;
            continue;
        }

        if negotiated_payload_type.is_none() {
            negotiated_payload_type = resolve_h264_payload_type(peer).await;
            if let Some(pt) = negotiated_payload_type {
                println!("Negotiated H264 RTP payload type: {pt}");
            }
        }

        let payload_type = negotiated_payload_type.unwrap_or(96);

        let capture_start = Instant::now();
        let mut reused_last_frame = false;
        let (width, height, bgra_frame) =
            match capture_primary_screen_even_bgra(&mut capturer, scale_target)? {
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
            println!(
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
            println!(
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

fn frame_interval_for(preset: VideoEncoderPreset) -> Duration {
    Duration::from_secs_f64(1.0 / preset.target_fps.max(1) as f64)
}

fn capture_primary_screen_even_bgra(
    capturer: &mut DxgiDesktopDuplicator,
    scale_target: Option<(usize, usize)>,
) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
    let Some(frame) = capturer.capture_next_frame(16)? else {
        return Ok(None);
    };

    let prepared = normalize_frame_for_stream(frame, scale_target)?;
    Ok(Some(prepared))
}

fn normalize_frame_for_stream(
    frame: DesktopFrame,
    scale_target: Option<(usize, usize)>,
) -> Result<(usize, usize, Vec<u8>), String> {
    let frame = if let Some((requested_width, requested_height)) = scale_target {
        let (target_width, target_height) = resolve_scaled_dimensions(
            frame.width,
            frame.height,
            requested_width,
            requested_height,
        );
        frame.resize_bgra_nearest(target_width, target_height)
    } else if should_auto_downscale() && (frame.width > 1920 || frame.height > 1080) {
        // Auto-downscale high-resolution captures (4K, ultrawide, etc.) to 1080p
        // to guarantee good FPS on all hardware. Preserves aspect ratio.
        let aspect = frame.width as f64 / frame.height.max(1) as f64;
        let (tw, th) = if aspect >= 1.0 {
            // Landscape: cap width at 1920
            let w = 1920usize;
            let h = ((w as f64 / aspect).round() as usize).max(2);
            (w, h)
        } else {
            // Portrait: cap height at 1080
            let h = 1080usize;
            let w = ((h as f64 * aspect).round() as usize).max(2);
            (w, h)
        };
        frame.resize_bgra_nearest(tw & !1, th & !1)
    } else {
        frame
    };

    let (width, height, bgra) = frame.into_even_bgra();
    if width < 2 || height < 2 {
        return Err("Captured frame is too small".to_string());
    }

    Ok((width, height, bgra))
}

fn should_auto_downscale() -> bool {
    // Disable auto-downscale with LUMIERE_STREAM_NOSCALE=1
    !env_flag_true("LUMIERE_STREAM_NOSCALE")
}

fn resolve_scale_request() -> Option<(usize, usize)> {
    let requested_width = env::var("LUMIERE_STREAM_WIDTH")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 320);
    let requested_height = env::var("LUMIERE_STREAM_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 180);

    match (requested_width, requested_height) {
        (Some(width), Some(height)) => Some((width & !1, height & !1)),
        (Some(width), None) => Some((width & !1, 0)),
        (None, Some(height)) => Some((0, height & !1)),
        (None, None) => None,
    }
}

fn resolve_scaled_dimensions(
    source_width: usize,
    source_height: usize,
    requested_width: usize,
    requested_height: usize,
) -> (usize, usize) {
    let aspect = source_width as f64 / source_height.max(1) as f64;

    let (target_width, target_height) = match (requested_width, requested_height) {
        (width, height) if width > 0 && height > 0 => (width, height),
        (width, 0) if width > 0 => {
            let height = ((width as f64 / aspect).round() as usize).max(2);
            (width, height)
        }
        (0, height) if height > 0 => {
            let width = ((height as f64 * aspect).round() as usize).max(2);
            (width, height)
        }
        _ => (source_width, source_height),
    };

    ((target_width.max(2)) & !1, (target_height.max(2)) & !1)
}
