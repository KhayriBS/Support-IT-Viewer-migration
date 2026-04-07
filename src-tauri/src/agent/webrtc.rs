use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f"
                        .to_owned(),
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
    let mut seq: u16 = 1;
    let mut timestamp: u32 = 0;
    let ts_step: u32 = 90000 / preset.target_fps.max(1);
    let mut frame_index: u64 = 0;
    let mut stats = StreamStatsWindow::new();

    loop {
        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

        if !stream_is_ready(peer, track).await {
            tokio::time::sleep(Duration::from_millis(120)).await;
            continue;
        }

        let (width, height, bgra_frame) = match capture_primary_screen_even_bgra() {
            Ok(frame) => frame,
            Err(err) => {
                eprintln!("Screen capture failed: {err}");
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        };

        let bgra = BgraSliceU8::new(&bgra_frame, (width, height));
        let yuv = YUVBuffer::from_rgb_source(bgra);

        frame_index += 1;
        if keyframe_interval > 0 && frame_index % keyframe_interval == 0 {
            encoder.force_intra_frame();
        }

        let encoded = match encoder.encode(&yuv).map(|bitstream| bitstream.to_vec()) {
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

        let payload_bytes = Bytes::from(encoded);
        let payloads = match payloader.payload(1200, &payload_bytes) {
            Ok(chunks) => chunks,
            Err(err) => {
                eprintln!("H264 payload split failed: {err}");
                tokio::time::sleep(frame_interval).await;
                continue;
            }
        };

        let frame_ts = timestamp;
        timestamp = timestamp.wrapping_add(ts_step);
        let mut frame_sent = false;

        for (index, fragment) in payloads.iter().enumerate() {
            if track.any_binding_paused().await {
                break;
            }

            let packet = Packet {
                header: rtp::header::Header {
                    version: 2,
                    padding: false,
                    extension: false,
                    marker: index + 1 == payloads.len(),
                    payload_type: 0,
                    sequence_number: seq,
                    timestamp: frame_ts,
                    ssrc: 0,
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

        if frame_sent {
            stats.record_frame(payload_bytes.len());
        }
        stats.flush_if_due(signaling).await;
        tokio::time::sleep(frame_interval).await;
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

    loop {
        match peer.connection_state() {
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
            _ => {}
        }

        if !stream_is_ready(peer, track).await {
            tokio::time::sleep(Duration::from_millis(80)).await;
            continue;
        }

        let (width, height, bgra_frame) = capture_primary_screen_even_bgra()?;
        if active_dimensions != Some((width, height)) {
            if let Some(existing) = bridge.as_mut() {
                existing.shutdown().await;
            }

            bridge = Some(FfmpegRtpBridge::spawn(backend, width, height, preset).await?);
            active_dimensions = Some((width, height));
        }

        let active_bridge = bridge
            .as_mut()
            .ok_or_else(|| "FFmpeg bridge unavailable after spawn".to_string())?;
        active_bridge.write_frame(&bgra_frame).await?;
        drain_ffmpeg_packets(track, active_bridge, &mut stats).await?;
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

fn capture_primary_screen_even_bgra() -> Result<(usize, usize, Vec<u8>), String> {
    let frame = screenshots::Screen::all()
        .map_err(|err| err.to_string())
        .and_then(|screens| {
            screens
                .first()
                .ok_or_else(|| "No screen detected".to_string())
                .and_then(|screen| screen.capture().map_err(|err| err.to_string()))
        })?;

    let width = frame.width() as usize;
    let height = frame.height() as usize;
    let raw = frame.into_raw();
    let even_width = width & !1;
    let even_height = height & !1;
    if even_width < 2 || even_height < 2 {
        return Err("Captured frame is too small".to_string());
    }

    let bgra_even = if even_width == width && even_height == height {
        raw
    } else {
        let mut out = vec![0u8; even_width * even_height * 4];
        for y in 0..even_height {
            let src_row = &raw[(y * width * 4)..(y * width * 4 + even_width * 4)];
            let dst_row = &mut out[(y * even_width * 4)..(y * even_width * 4 + even_width * 4)];
            dst_row.copy_from_slice(src_row);
        }
        out
    };

    Ok((even_width, even_height, bgra_even))
}
