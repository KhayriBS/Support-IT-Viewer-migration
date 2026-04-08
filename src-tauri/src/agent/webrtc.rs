use serde_json::Value;
use std::env;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant};

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
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
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
}

struct StreamStatsWindow {
    started_at: Instant,
    sent_bytes: usize,
    sent_frames: usize,
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
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
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

        println!("WebRTC video track created: screen/H264");

        tokio::spawn(async move {
            while rtp_sender.read_rtcp().await.is_ok() {}
        });

        let peer_for_state = Arc::clone(&peer);
        peer.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            println!("WebRTC connection state: {state:?}");
            let _ = &peer_for_state;
            Box::pin(async {})
        }));

        Ok(Self {
            signaling,
            peer,
            video_track,
        })
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

        let mut gather_complete = self.peer.gathering_complete_promise().await;

        let answer = self
            .peer
            .create_answer(None)
            .await
            .map_err(|e| format!("create_answer failed: {e}"))?;

        self.peer
            .set_local_description(answer)
            .await
            .map_err(|e| format!("set_local_description failed: {e}"))?;

        let _ = gather_complete.recv().await;

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
                    run_openh264_screen_sender(&signaling, &peer, &track, selection.preset).await
                }
                VideoEncoderBackend::MediaFoundationH264 => {
                    match run_media_foundation_screen_sender(
                        &signaling,
                        &peer,
                        &track,
                        selection.preset,
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

        self.peer
            .add_ice_candidate(init)
            .await
            .map_err(|e| format!("add_ice_candidate failed: {e}"))
    }
}

async fn run_openh264_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    preset: VideoEncoderPreset,
) -> Result<(), String> {
    let api = OpenH264API::from_source();
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .set_bitrate_bps(preset.bitrate_bps)
        .max_frame_rate(preset.target_fps as f32)
        .enable_skip_frame(true)
        .set_multiple_thread_idc(0);

    let mut encoder = Encoder::with_api_config(api, config)
        .map_err(|err| format!("OpenH264 encoder init failed: {err}"))?;
    let mut payloader = H264Payloader::default();
    let frame_interval = frame_interval_for(preset);
    let keyframe_interval = (preset.target_fps.max(1) as u64).saturating_mul(5);
    let mut stream_ssrc: u32 = 0;
    let mut negotiated_ssrc: Option<u32> = None;
    let mut negotiated_payload_type: Option<u8> = None;
    let mut cached_sps: Option<Vec<u8>> = None;
    let mut cached_pps: Option<Vec<u8>> = None;
    let mut seq: u16 = 1;
    let mut timestamp: u32 = 0;
    let ts_step: u32 = 90000 / preset.target_fps.max(1);
    let mut frame_index: u64 = 0;
    let mut stats = StreamStatsWindow::new();
    let mut capturer = DxgiDesktopDuplicator::new()?;
    let scale_target = resolve_scale_request();
    let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;

    loop {
        let frame_start = Instant::now();
        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

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
        let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;

        frame_index += 1;
        if frame_index % preset.target_fps.max(1) as u64 == 0 {
            println!(
                "Captured {} frames via DXGI at {}x{} for software H264 pipeline",
                frame_index, width, height
            );
        }

        let force_keyframe = frame_index == 1
            || (keyframe_interval > 0 && frame_index % keyframe_interval == 0);
        let encode_start = Instant::now();
        let (encoded_result, returned_encoder) = tokio::task::spawn_blocking(move || {
            let bgra = BgraSliceU8::new(&bgra_frame, (width, height));
            let yuv = YUVBuffer::from_rgb_source(bgra);

            if force_keyframe {
                encoder.force_intra_frame();
            }

            let result = encoder.encode(&yuv).map(|bitstream| bitstream.to_vec());
            (result, encoder)
        })
        .await
        .map_err(|e| format!("spawn_blocking failed: {e}"))?;

        let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;

        encoder = returned_encoder;

        let encoded = match encoded_result {
            Ok(data) => data,
            Err(err) => {
                eprintln!("H264 encode failed: {err}");
                tokio::time::sleep(frame_interval).await;
                continue;
            }
        };
        if encoded.is_empty() {
            tokio::time::sleep(frame_interval).await;
            continue;
        }

        let payload_start = Instant::now();
        let raw_nalus = split_annexb_nalus(&encoded);
        let (nalus, has_idr) = reorder_and_cache_sps_pps(raw_nalus, &mut cached_sps, &mut cached_pps);
        let nal_summary = summarize_nalus(&nalus);
        let payload_ms = payload_start.elapsed().as_secs_f64() * 1000.0;

        let frame_ts = timestamp;
        timestamp = timestamp.wrapping_add(ts_step);
        let mut frame_sent = false;

        let send_start = Instant::now();
        let mut total_fragments = 0usize;
        // If IDR is present but SPS/PPS missing in this access unit, prepend cached SPS/PPS (if any)
        // as copied Bytes so lifetimes are correct.
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

        // Send optional prefix first (SPS/PPS), then the ordered NALs.
        let total_nals_to_send = prefix.len() + nalus.len();
        let mut nal_cursor = 0usize;

        for bytes in prefix {
            nal_cursor += 1;
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
                if track.any_binding_paused().await {
                    break;
                }
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
                if track.any_binding_paused().await {
                    break;
                }

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
                    break;
                }

                frame_sent = true;
                seq = seq.wrapping_add(1);
            }
        }
        let send_ms = send_start.elapsed().as_secs_f64() * 1000.0;

        if frame_sent {
            stats.record_frame(encoded.len());
            if frame_index % preset.target_fps.max(1) as u64 == 0 {
                println!(
                    "Sent software H264 frame {} ({} bytes payload)",
                    frame_index,
                    encoded.len()
                );
                vlog!(
                    "sw pipeline: {}x{} reuse_last={} capture={:.2}ms encode={:.2}ms payload={:.2}ms send={:.2}ms total={:.2}ms nalus={} sps={} pps={} idr={} frags={} bytes={}",
                    width,
                    height,
                    reused_last_frame,
                    capture_ms,
                    encode_ms,
                    payload_ms,
                    send_ms,
                    frame_start.elapsed().as_secs_f64() * 1000.0,
                    nal_summary.nalus,
                    nal_summary.has_sps,
                    nal_summary.has_pps,
                    nal_summary.has_idr,
                    total_fragments,
                    encoded.len(),
                );
            }
        }
        stats.flush_if_due(signaling).await;
        tokio::time::sleep(frame_interval).await;
    }

    Ok(())
}

async fn run_media_foundation_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    preset: VideoEncoderPreset,
) -> Result<(), String> {
    let frame_period = frame_interval_for(preset);
    let mut payloader = H264Payloader::default();
    let mut stream_ssrc: u32 = 0;
    let mut negotiated_ssrc: Option<u32> = None;
    let mut negotiated_payload_type: Option<u8> = None;
    let mut cached_sps: Option<Vec<u8>> = None;
    let mut cached_pps: Option<Vec<u8>> = None;
    let mut seq: u16 = 1;
    let mut timestamp: u32 = 0;
    let ts_step: u32 = 90000 / preset.target_fps.max(1);
    let mut frame_counter: u64 = 0;
    let mut stats = StreamStatsWindow::new();
    let mut capturer = DxgiDesktopDuplicator::new()?;
    let scale_target = resolve_scale_request();
    let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;
    let mut last_dimensions: Option<(usize, usize)> = None;
    let mut worker = MediaFoundationEncoderWorker::new(
        0,
        0,
        preset.target_fps.max(1),
        preset.bitrate_bps,
    )?;

    loop {
        let loop_start = Instant::now();
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
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        continue;
                    }
                }
            };
        let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;
        frame_counter += 1;

        if last_dimensions != Some((width, height)) {
            last_dimensions = Some((width, height));
            println!(
                "Media Foundation H264 encoder configured at {}x{}",
                width, height
            );
        }

        let encode_start = Instant::now();
        let (encoded_units_result, returned_worker) = tokio::task::spawn_blocking(move || {
            let nv12 = super::desktop_duplication::bgra_to_nv12(width, height, width * 4, &bgra_frame);
            let result = worker.encode_nv12(width, height, nv12.as_bytes());
            (result, worker)
        })
        .await
        .map_err(|e| format!("spawn_blocking failed: {e}"))?;
        worker = returned_worker;

        let encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;

        let encoded_units = encoded_units_result?;
        if encoded_units.is_empty() || encoded_units.iter().all(|u| u.data.is_empty()) {
            tokio::time::sleep(Duration::from_millis(1)).await;
            continue;
        }

        let frame_ts = timestamp;
        timestamp = timestamp.wrapping_add(ts_step);
        let mut frame_sent = false;
        let mut total_payload_bytes = 0usize;
        let mut total_fragments = 0usize;
        let mut nal_summary = NalSummary::default();

        let units_len = encoded_units.len();
        for (unit_index, unit) in encoded_units.into_iter().enumerate() {
            if unit.data.is_empty() {
                continue;
            }

            let raw_nalus = split_annexb_nalus(&unit.data);
            let (nalus, has_idr) = reorder_and_cache_sps_pps(raw_nalus, &mut cached_sps, &mut cached_pps);
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
                    if track.any_binding_paused().await {
                        break;
                    }
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
                    if track.any_binding_paused().await {
                        break;
                    }

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
                        break;
                    }

                    frame_sent = true;
                    seq = seq.wrapping_add(1);
                }
            }
        }

        if frame_sent {
            stats.record_frame(total_payload_bytes.max(1));
            if frame_counter % preset.target_fps.max(1) as u64 == 0 {
                println!(
                    "Sent native MF H264 frame {} ({} bytes payload)",
                    frame_counter, total_payload_bytes
                );
                vlog!(
                    "mf pipeline: {}x{} reuse_last={} capture={:.2}ms encode={:.2}ms total={:.2}ms nalus={} sps={} pps={} idr={} frags={} bytes={}",
                    width,
                    height,
                    reused_last_frame,
                    capture_ms,
                    encode_ms,
                    loop_start.elapsed().as_secs_f64() * 1000.0,
                    nal_summary.nalus,
                    nal_summary.has_sps,
                    nal_summary.has_pps,
                    nal_summary.has_idr,
                    total_fragments,
                    total_payload_bytes,
                );
            }
        }
        stats.flush_if_due(signaling).await;

        let elapsed = loop_start.elapsed();
        if elapsed < frame_period {
            tokio::time::sleep(frame_period - elapsed).await;
        }
    }

    Ok(())
}

async fn run_ffmpeg_rtp_screen_sender(
    signaling: &Arc<SignalingClient>,
    peer: &Arc<RTCPeerConnection>,
    track: &Arc<TrackLocalStaticRTP>,
    backend: VideoEncoderBackend,
    preset: VideoEncoderPreset,
) -> Result<(), String> {
    let frame_interval = frame_interval_for(preset);
    let mut bridge: Option<FfmpegRtpBridge> = None;
    let mut active_dimensions: Option<(usize, usize)> = None;
    let mut stats = StreamStatsWindow::new();
    let mut negotiated_payload_type: Option<u8> = None;
    let mut capturer = DxgiDesktopDuplicator::new()?;
    let scale_target = resolve_scale_request();
    let mut last_capture: Option<(usize, usize, Arc<Vec<u8>>)> = None;
    let mut frame_counter: u64 = 0;

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
        tokio::time::sleep(frame_interval).await;
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
    !track.all_binding_paused().await
        && peer.connection_state() == RTCPeerConnectionState::Connected
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
    } else {
        frame
    };

    let (width, height, bgra) = frame.into_even_bgra();
    if width < 2 || height < 2 {
        return Err("Captured frame is too small".to_string());
    }

    Ok((width, height, bgra))
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
