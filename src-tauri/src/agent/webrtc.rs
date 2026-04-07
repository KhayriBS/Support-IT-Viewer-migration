use serde_json::Value;
use std::sync::Arc;

use super::input_handler::InputHandler;
use super::signaling::SignalingClient;
use bytes::Bytes;
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;
use rtp::codecs::h264::H264Payloader;
use rtp::packet::Packet;
use rtp::packetizer::Payloader;
use webrtc::api::media_engine::{MIME_TYPE_H264};
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::TrackLocalWriter;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::interceptor::registry::Registry;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

pub struct AgentWebRtc {
    peer: Arc<RTCPeerConnection>,
    video_track: Arc<TrackLocalStaticRTP>,
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
                            eprintln!("⚠️ Failed to send local ICE candidate: {err}");
                        }
                    }
                    Err(err) => {
                        eprintln!("⚠️ Failed to serialize local ICE candidate: {err}");
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

        // Add a real outgoing H.264 video track (screen share).
        // The viewer already creates a recvonly transceiver for video.
        let video_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f".to_owned(),
                rtcp_feedback: vec![],
            },
            "video".to_owned(),
            "screen".to_owned(),
        ));

        let rtp_sender = peer
            .add_track(Arc::clone(&video_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
            .await
            .map_err(|e| format!("add_track failed: {e}"))?;

        // Drain RTCP packets so the sender doesn't stall.
        tokio::spawn(async move {
            while rtp_sender.read_rtcp().await.is_ok() {
                // ignore
            }
        });

        // Keep parity with legacy logs for easier debugging.
        let peer_for_state = Arc::clone(&peer);
        peer.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            println!("🔗 WebRTC connection state: {state:?}");
            let _ = &peer_for_state;
            Box::pin(async {})
        }));

        Ok(Self { peer, video_track })
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
        let peer = Arc::clone(&self.peer);
        let track = Arc::clone(&self.video_track);

        tokio::spawn(async move {
            // Encoder tuned for real-time screen content.
            let api = OpenH264API::from_source();
            let config = EncoderConfig::new()
                .usage_type(UsageType::ScreenContentRealTime)
                .rate_control_mode(RateControlMode::Bitrate)
                .set_bitrate_bps(2_000_000)
                .max_frame_rate(12.0)
                .enable_skip_frame(true)
                .set_multiple_thread_idc(0);

            let mut encoder = match Encoder::with_api_config(api, config) {
                Ok(enc) => enc,
                Err(err) => {
                    eprintln!("❌ OpenH264 encoder init failed: {err}");
                    return;
                }
            };

            let mut payloader = H264Payloader::default();
            let mtu: usize = 1200;
            let mut seq: u16 = 1;
            let mut timestamp: u32 = 0;
            let ts_step: u32 = 90000 / 12;
            let mut frame_index: u64 = 0;

            loop {
                // Stop when the PeerConnection is gone/closing.
                match peer.connection_state() {
                    RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed => break,
                    _ => {}
                }

                // Wait for negotiation/bindings. Treat "no bindings" as paused.
                if track.all_binding_paused().await || peer.connection_state() != RTCPeerConnectionState::Connected {
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                    continue;
                }

                // Capture screen (BGRA) on Windows.
                let frame = match screenshots::Screen::all()
                    .map_err(|e| e.to_string())
                    .and_then(|screens| {
                        screens
                            .first()
                            .ok_or_else(|| "No screen detected".to_string())
                            .and_then(|screen| screen.capture().map_err(|e| e.to_string()))
                    }) {
                    Ok(frame) => frame,
                    Err(err) => {
                        eprintln!("⚠️ Screen capture failed: {err}");
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        continue;
                    }
                };

                let width = frame.width() as usize;
                let height = frame.height() as usize;
                let raw = frame.into_raw();

                // Ensure even dimensions for I420.
                let even_w = width & !1;
                let even_h = height & !1;
                if even_w < 2 || even_h < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }

                let bgra_even: Vec<u8> = if even_w == width && even_h == height {
                    raw
                } else {
                    let mut out = vec![0u8; even_w * even_h * 4];
                    for y in 0..even_h {
                        let src_row = &raw[(y * width * 4)..(y * width * 4 + even_w * 4)];
                        let dst_row = &mut out[(y * even_w * 4)..(y * even_w * 4 + even_w * 4)];
                        dst_row.copy_from_slice(src_row);
                    }
                    out
                };

                // Convert BGRA -> I420 using openh264's converter.
                let bgra = BgraSliceU8::new(&bgra_even, (even_w, even_h));
                let yuv = YUVBuffer::from_rgb_source(bgra);

                // Force a keyframe periodically.
                frame_index += 1;
                if frame_index % 90 == 0 {
                    encoder.force_intra_frame();
                }

                // IMPORTANT: OpenH264's `EncodedBitStream` borrows internal encoder state and is
                // not `Send`; materialize it into a `Vec<u8>` immediately so we don't hold it
                // across any `.await` points.
                let encoded = match encoder.encode(&yuv).map(|bs| bs.to_vec()) {
                    Ok(data) => data,
                    Err(err) => {
                        eprintln!("⚠️ H264 encode failed: {err}");
                        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                        continue;
                    }
                };
                if encoded.is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                    continue;
                }

                let payload_bytes = Bytes::from(encoded);
                let payloads = match payloader.payload(mtu, &payload_bytes) {
                    Ok(v) => v,
                    Err(err) => {
                        eprintln!("⚠️ H264 payload split failed: {err}");
                        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                        continue;
                    }
                };

                // Same timestamp for all packets of a frame.
                let frame_ts = timestamp;
                timestamp = timestamp.wrapping_add(ts_step);

                for (i, frag) in payloads.iter().enumerate() {
                    if track.any_binding_paused().await {
                        // Don't advance seq if sending is blocked internally.
                        break;
                    }

                    let is_last = i + 1 == payloads.len();
                    let pkt = Packet {
                        header: rtp::header::Header {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker: is_last,
                            payload_type: 0,
                            sequence_number: seq,
                            timestamp: frame_ts,
                            ssrc: 0,
                            csrc: vec![],
                            extension_profile: 0,
                            extensions: vec![],
                            extensions_padding: 0,
                        },
                        payload: frag.clone(),
                    };

                    if track.write_rtp(&pkt).await.is_err() {
                        break;
                    }

                    seq = seq.wrapping_add(1);
                }

                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
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
