use serde_json::Value;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use std::time::Duration;
use tokio::sync::{watch, Mutex};


use super::input_handler::InputHandler;
use super::signaling::SignalingClient;
use super::video_encoder::{
    VideoEncoderBackend, VideoEncoderSelection,
};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

use super::ice_servers::{read_env_or_local, resolve_ice_servers_for_peer};
pub use super::ice_servers::{resolve_ice_servers_for_frontend, IceServerConfig};
use super::adaptive_streaming::{
    RtcpFeedbackState, StreamingActivityState,
};
pub use super::adaptive_streaming::{FpsTier, StreamQualityProfile};
use super::file_channel_handler::{handle_screenshot_request, setup_file_channel};
use super::stream_senders::{
    run_ffmpeg_rtp_screen_sender, run_media_foundation_screen_sender,
    run_openh264_screen_sender,
};

pub(super) fn env_flag_true(key: &str) -> bool {
    // 1) Variable d'environnement directe (process env, posée par le shell ou
    //    Cargo) — chemin canonique pour CI / production.
    if let Ok(value) = env::var(key) {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
    // 2) Fallback : .env.local au repo root (utilisé en dev Tauri où le binaire
    //    tourne depuis src-tauri/target/debug/, hors du process env).
    if let Some(value) = read_env_or_local(key) {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
    false
}

fn agent_ice_gather_timeout_secs() -> u64 {
    // Lit env var, sinon .env.local (cas dev Tauri où le binaire tourne hors
    // du process env du shell). Default réduit à 3 s parce que les passerelles
    // freemium (Render) ferment la WebSocket en 5-8 s d'idle — il vaut mieux
    // envoyer l'ANSWER avec des candidats partiels que pas du tout.
    let raw = std::env::var("LUMIERE_AGENT_ICE_GATHER_TIMEOUT_SECS")
        .ok()
        .or_else(|| read_env_or_local("LUMIERE_AGENT_ICE_GATHER_TIMEOUT_SECS"));
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(1, 15))
        .unwrap_or(3)
}

pub(super) fn video_debug_enabled() -> bool {
    env_flag_true("LUMIERE_VIDEO_DEBUG")
}

pub(super) fn parse_candidate_type(candidate: &str) -> &'static str {
    let needle = " typ ";
    let Some(idx) = candidate.find(needle) else {
        return "unknown";
    };
    let rest = &candidate[idx + needle.len()..];
    let end = rest.find(' ').unwrap_or(rest.len());
    match rest[..end].trim() {
        "host" => "host",
        "srflx" => "srflx",
        "relay" => "relay",
        "prflx" => "prflx",
        _ => "unknown",
    }
}

pub(super) fn parse_candidate_address(candidate: &str) -> Option<(String, u16)> {
    let mut parts = candidate.split_whitespace();
    let _foundation = parts.next()?;
    let _component = parts.next()?;
    let _protocol = parts.next()?;
    let _priority = parts.next()?;
    let ip = parts.next()?.to_string();
    let port = parts.next()?.parse::<u16>().ok()?;
    Some((ip, port))
}

pub(super) fn derive_stream_ssrc() -> u32 {
    let pid = std::process::id() as u64;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    let mixed = now.as_nanos() as u64 ^ (pid.rotate_left(13));
    let ssrc = (mixed as u32) | 1;
    if ssrc == 0 { 1 } else { ssrc }
}

pub struct AgentWebRtc {
    signaling: Arc<SignalingClient>,
    peer: Arc<RTCPeerConnection>,
    video_track: Arc<TrackLocalStaticRTP>,
    rtcp_feedback: Arc<RtcpFeedbackState>,
    stream_profile_tx: watch::Sender<StreamQualityProfile>,
    bitrate_override_tx: watch::Sender<Option<u32>>,
    fps_tier_override_tx: watch::Sender<Option<FpsTier>>,
    activity_state: Arc<StreamingActivityState>,
    /// Flag basculé via STREAM_PROFILE payload `{ paused: true }` pour
    /// suspendre complètement la capture+encodage+envoi RTP. Quand `true`,
    /// les boucles de capture des trois encodeurs (OpenH264 / Media Foundation /
    /// FFmpeg) court-circuitent toute production de frame — bande passante
    /// vidéo nulle, le DataChannel SCTP a la pipe pour lui seul.
    frame_emission_paused: Arc<AtomicBool>,
    pending_remote_ice: Mutex<Vec<RTCIceCandidateInit>>,
}


// ─── AgentWebRtc impl ─────────────────────────────────────────────────────────

impl AgentWebRtc {
    pub async fn new(
        signaling: Arc<SignalingClient>,
        input_handler: Arc<InputHandler>,
        allow_remote_input: bool,
        allow_file_transfer: bool,
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

        let ice_servers = resolve_ice_servers_for_peer().await;
        let policy = if std::env::var("LUMIERE_FORCE_RELAY")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
            .unwrap_or(false)
        {
            RTCIceTransportPolicy::Relay
        } else {
            RTCIceTransportPolicy::All
        };
        let has_stun = ice_servers
            .iter()
            .any(|s| s.urls.iter().any(|u| u.starts_with("stun:")));
        let has_turn = ice_servers
            .iter()
            .any(|s| s.urls.iter().any(|u| u.starts_with("turn:") || u.starts_with("turns:")));
        tracing::info!(
            "🧊 [ICE] Agent transport policy: {:?} | servers count={} stun={} turn={}",
            policy,
            ice_servers.len(),
            has_stun,
            has_turn
        );
        if !has_stun {
            tracing::warn!(
                "🧊 [ICE] Aucun serveur STUN configuré — les candidats srflx ne seront pas découverts. \
                 Le LAN direct (host) peut quand même fonctionner mais le P2P inter-NAT échouera."
            );
        }
        if policy == RTCIceTransportPolicy::Relay {
            tracing::warn!(
                "🧊 [ICE] LUMIERE_FORCE_RELAY=1 → seuls les candidats relay TURN seront tentés. \
                 Le LAN direct est désactivé."
            );
        }
        let config = RTCConfiguration {
            ice_servers,
            ice_transport_policy: policy,
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
                        let cand_type = parse_candidate_type(&init.candidate);
                        let addr = parse_candidate_address(&init.candidate)
                            .map(|(ip, port)| format!("{ip}:{port}"))
                            .unwrap_or_else(|| "?".to_string());
                        tracing::info!(
                            "🧊 [ICE] agent → viewer  type={cand_type:<5} addr={addr}"
                        );
                        let sdp_mid = init.sdp_mid.filter(|mid| !mid.is_empty());
                        let payload = serde_json::json!({
                            "candidate": init.candidate,
                            "sdpMid": sdp_mid,
                            "sdpMLineIndex": init.sdp_mline_index,
                        });

                        if let Err(err) = signaling.send_ice_candidate(payload).await {
                            tracing::warn!("Failed to send local ICE candidate: {err}");
                        }
                    }
                    Err(err) => {
                        tracing::warn!("Failed to serialize local ICE candidate: {err}");
                    }
                }
            })
        }));

        let activity_state = Arc::new(StreamingActivityState::default());
        let activity_for_channel = Arc::clone(&activity_state);
        // Démarre PAUSÉ (true) : aucune frame n'est émise tant que le viewer
        // n'a pas explicitement cliqué la carte "Écran" (envoi VIDEO_RESUME
        // via DataChannel input). À l'activation de session, le viewer
        // affiche un menu Écran/Fichier/Chat — on ne veut pas gâcher de
        // bande passante avant qu'il choisisse.
        // Le viewer renvoie automatiquement VIDEO_RESUME à chaque fois que
        // le DataChannel input s'ouvre, ce qui élimine le risque d'agent
        // muet permanent même si le premier message se perd.
        let frame_emission_paused = Arc::new(AtomicBool::new(true));
        let frame_emission_paused_for_channel = Arc::clone(&frame_emission_paused);
        peer.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
            let input_handler = Arc::clone(&input_handler);
            let activity_for_channel = Arc::clone(&activity_for_channel);
            let frame_emission_paused = Arc::clone(&frame_emission_paused_for_channel);
            Box::pin(async move {
                let label = channel.label().to_string();
                tracing::info!("DataChannel recu: {label}");

                let open_label = label.clone();
                channel.on_open(Box::new(move || {
                    Box::pin(async move {
                        tracing::info!("DataChannel ouvert: {open_label}");
                    })
                }));

                let close_label = label.clone();
                channel.on_close(Box::new(move || {
                    let close_label = close_label.clone();
                    Box::pin(async move {
                        tracing::info!("DataChannel ferme: {close_label}");
                    })
                }));

                if label == "input" {
                    // Fingerprint de version : si tu ne vois PAS cette ligne dans la
                    // console agent quand le viewer se connecte, c'est que le binaire
                    // qui tourne est l'ANCIEN (sans le handler request_screenshot).
                    tracing::info!(
                        "✅ DataChannel 'input' configure (AI build = v2 with request_screenshot handler)"
                    );

                    let message_label = label.clone();
                    let frame_emission_paused = Arc::clone(&frame_emission_paused);
                    // Clone du channel pour que l'executor IA puisse renvoyer
                    // les AI_ACTION_RESULT sur le meme tuyau.
                    let channel_for_ai = Arc::clone(&channel);
                    channel.on_message(Box::new(move |msg: DataChannelMessage| {
                        // ═══════════════════════════════════════════════════════════════
                        // [WIRE TRACE] Tout premier point de contact — AVANT meme la
                        // verification is_string, AVANT le parsing UTF-8, AVANT le JSON.
                        // Si tu fais /ai cote viewer et tu ne vois RIEN ici cote agent,
                        // c'est definitif : le message n'arrive physiquement pas au
                        // DataChannel input de CE peer-connection-ci. → probleme reseau
                        // ou double-instance d'agent qui se partage le code.
                        //
                        // Filtre anti-spam : on skippe les mousemove qui inondent a 60Hz.
                        // ═══════════════════════════════════════════════════════════════
                        {
                            let is_str = msg.is_string;
                            let n = msg.data.len();
                            // Detection rapide "mousemove" sans parser le JSON complet :
                            // {"type":"mousemove"...  → contains b"\"type\":\"mousemove\""
                            let is_mousemove = is_str
                                && (msg.data.windows(20).any(|w| w == b"\"type\":\"mousemove\"")
                                    || msg.data.windows(21).any(|w| w == b"\"type\":\"mouse-move\""));
                            if !is_mousemove {
                                if is_str {
                                    let preview: String = String::from_utf8_lossy(&msg.data)
                                        .chars().take(80).collect();
                                    tracing::info!("[wire] ENTRY len={n} str='{preview}'");
                                } else {
                                    tracing::info!("[wire] ENTRY len={n} <binary>");
                                }
                            }
                        }

                        let input_handler = Arc::clone(&input_handler);
                        let message_label = message_label.clone();
                        let activity_for_channel = Arc::clone(&activity_for_channel);
                        let frame_emission_paused = Arc::clone(&frame_emission_paused);
                        let channel_for_ai = Arc::clone(&channel_for_ai);
                        Box::pin(async move {
                            if !msg.is_string {
                                return;
                            }

                            let Ok(message) = String::from_utf8(msg.data.to_vec()) else {
                                tracing::warn!("Message DataChannel invalide sur {message_label}");
                                return;
                            };

                            // Diagnostic IA : trace tous les messages JSON qui contiennent
                            // un type (= messages de controle, pas les mousemove). Cible :
                            // confirmer si request_screenshot arrive ou pas. A retirer
                            // une fois le pipeline IA stabilise.
                            if message.starts_with('{') {
                                let preview = if message.len() > 160 {
                                    format!("{}…", &message[..160])
                                } else {
                                    message.clone()
                                };
                                tracing::info!("🔎 input-msg: {preview}");
                            }

                            // Messages de contrôle vidéo : court-circuitent
                            // l'input handler. Format JSON :
                            //   { "type": "VIDEO_PAUSE" }   → suspend l'émission
                            //   { "type": "VIDEO_RESUME" }  → reprend l'émission
                            // Passent par le DataChannel input (P2P) plutôt que
                            // par le signaling WebSocket qui est souvent fermé
                            // (1003) après l'OFFER/ANSWER sur Render free-tier.
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message) {
                                if let Some(t) = value.get("type").and_then(|v| v.as_str()) {
                                    match t {
                                        "VIDEO_PAUSE" => {
                                            let was = frame_emission_paused.swap(true, Ordering::AcqRel);
                                            if !was {
                                                tracing::info!("🎬 Frame emission PAUSED (viewer request)");
                                            }
                                            return;
                                        }
                                        "VIDEO_RESUME" => {
                                            let was = frame_emission_paused.swap(false, Ordering::AcqRel);
                                            if was {
                                                tracing::info!("🎬 Frame emission RESUMED (viewer request)");
                                            }
                                            return;
                                        }
                                        // Agent IA : actions envoyees par le viewer apres reponse Gemini.
                                        // On les execute hors du chemin input_handler car la semantique
                                        // est differente (clic absolu denormalise, shell, screenshot
                                        // reply, etc.) et on respecte la meme regle allow_remote_input.
                                        "AI_ACTION" | "AI_PLAN" => {
                                            activity_for_channel.mark_input_activity();
                                            if !allow_remote_input {
                                                tracing::info!("AI action ignoree (lecture seule)");
                                                return;
                                            }
                                            super::ai_executor::dispatch(
                                                Arc::clone(&channel_for_ai),
                                                &message,
                                            )
                                            .await;
                                            return;
                                        }
                                        // Pre-Gemini : le viewer veut un screenshot REEL de notre
                                        // ecran (et pas la frame WebRTC decodee, qui peut etre noire
                                        // si emission_paused=true). On capture cote agent avec
                                        // screenshots::Screen et on renvoie en base64 par le meme
                                        // DataChannel, indexe par commandId pour matcher la response
                                        // cote viewer en cas de requetes concurrentes.
                                        "request_screenshot" => {
                                            let command_id = value
                                                .get("commandId")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            tracing::info!("📸 request_screenshot recu (commandId={command_id})");
                                            let channel_reply = Arc::clone(&channel_for_ai);
                                            tokio::spawn(async move {
                                                handle_screenshot_request(channel_reply, command_id).await;
                                            });
                                            return;
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            activity_for_channel.mark_input_activity();

                            if !allow_remote_input {
                                tracing::info!("Input distant ignore (lecture seule)");
                                return;
                            }

                            input_handler.handle_input(&message);
                        })
                    }));
                } else if label == "file" {
                    setup_file_channel(channel, allow_file_transfer).await;
                }
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
    let (bitrate_override_tx, _) = watch::channel(None::<u32>);
    let (fps_tier_override_tx, _) = watch::channel(None::<FpsTier>);

        tracing::info!("WebRTC video track created: screen/H264");

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
            tracing::info!("WebRTC connection state: {state:?}");
            if state == RTCPeerConnectionState::Failed {
                let peer_for_recovery = Arc::clone(&peer_for_state);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if peer_for_recovery.connection_state() == RTCPeerConnectionState::Failed {
                        tracing::warn!(
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
            bitrate_override_tx,
            fps_tier_override_tx,
            activity_state,
            frame_emission_paused,
            pending_remote_ice: Mutex::new(Vec::new()),
        })
    }

    pub fn set_stream_profile(&self, profile: StreamQualityProfile) {
        let _ = self.stream_profile_tx.send(profile);
    }

    /// Suspend ou reprend l'émission de frames vidéo. Utilisé par le viewer
    /// pendant un transfert de fichier pour libérer toute la bande passante
    /// du canal P2P au profit du DataChannel SCTP.
    pub fn set_frame_emission_paused(&self, paused: bool) {
        let was = self.frame_emission_paused.swap(paused, Ordering::AcqRel);
        if was != paused {
            tracing::info!(
                "🎬 Frame emission {} (was {})",
                if paused { "PAUSED" } else { "RESUMED" },
                if was { "paused" } else { "running" }
            );
        }
    }

    pub fn adjust_bitrate(&self, bitrate: u32) {
        let _ = self.bitrate_override_tx.send(Some(bitrate.max(800_000)));
    }

    pub fn clear_bitrate_override(&self) {
        let _ = self.bitrate_override_tx.send(None);
    }

    pub fn set_fps_tier(&self, tier: FpsTier) {
        let _ = self.fps_tier_override_tx.send(Some(tier));
    }

    pub fn clear_fps_tier_override(&self) {
        let _ = self.fps_tier_override_tx.send(None);
    }

    pub async fn close(&self) {
        if let Err(err) = self.peer.close().await {
            tracing::warn!("Failed to close WebRTC peer: {err}");
        }
    }

    /// True when the underlying peer connection is in `Connected` state.
    /// Used by the session layer to ignore signaling-socket failures while
    /// the actual WebRTC pipe is healthy (signaling is only required for
    /// negotiation; once `Connected`, video/data flow peer-to-peer).
    pub fn is_peer_connected(&self) -> bool {
        self.peer.connection_state() == RTCPeerConnectionState::Connected
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
                tracing::warn!("Failed to apply queued remote ICE candidate: {err}");
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

        // Half-trickle ICE is OPT-IN. By default we return the ANSWER
        // immediately and rely on `on_ice_candidate` to trickle each
        // candidate as a small ICE signaling message. Embedding all
        // candidates (host+srflx+relay) in the SDP makes the ANSWER frame
        // 8–30 KB which the Render free-tier WebSocket gateway rejects
        // with close code 1003 ("message too big" → "unsupported data"),
        // tearing the signaling socket down within seconds.
        //
        // Set LUMIERE_HALF_TRICKLE_ICE=1 to re-enable embedding (only safe
        // on signaling servers that accept large text frames).
        if env_flag_true("LUMIERE_HALF_TRICKLE_ICE") {
            let gather_timeout_secs = agent_ice_gather_timeout_secs();
            tracing::info!(
                "⏳ [ICE] Half-trickle ON — waiting for ICE gathering (max {}s)...",
                gather_timeout_secs
            );
            let mut gather_complete = self.peer.gathering_complete_promise().await;
            let gather_timeout = tokio::time::timeout(
                Duration::from_secs(gather_timeout_secs),
                gather_complete.recv(),
            )
            .await;

            match gather_timeout {
                Ok(_) => tracing::info!("✅ [ICE] Local ICE gathering complete"),
                Err(_) => tracing::info!("⚠️ [ICE] Gathering timeout — sending ANSWER with partial candidates"),
            }
        } else {
            tracing::info!("📤 [ICE] Trickle mode — sending ANSWER immediately, candidates will trickle separately");
        }

        let local = self
            .peer
            .local_description()
            .await
            .ok_or("local_description unavailable")?;

        // Count candidates embedded for visibility.
        let cand_lines: Vec<&str> = local.sdp.lines()
            .filter(|l| l.starts_with("a=candidate"))
            .collect();
        let relay_count = cand_lines.iter().filter(|l| l.contains(" typ relay")).count();
        let srflx_count = cand_lines.iter().filter(|l| l.contains(" typ srflx")).count();
        let host_count = cand_lines.iter().filter(|l| l.contains(" typ host")).count();
        tracing::info!(
            "📤 [ICE] ANSWER embedding {} candidates (host={}, srflx={}, relay={})",
            cand_lines.len(), host_count, srflx_count, relay_count
        );

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
        let bitrate_override_rx = self.bitrate_override_tx.subscribe();
        let fps_tier_override_rx = self.fps_tier_override_tx.subscribe();
        let activity_state = Arc::clone(&self.activity_state);
        let frame_emission_paused = Arc::clone(&self.frame_emission_paused);

        tokio::spawn(async move {
            let selection = VideoEncoderSelection::resolve();
            tracing::info!(
                "Video encoder selected: {} (target={} FPS, bitrate={} Mbps)",
                selection.backend.label(),
                selection.preset.target_fps,
                selection.preset.bitrate_bps as f64 / 1_000_000.0
            );

            // Construit une seule fois les 10 paramètres communs aux 2
            // encoders SW/HW. Les fallbacks utilisent `clone_for_fallback()`
            // pour dupliquer les receveurs `watch` sans risquer un mismatch.
            let sender_args = || super::stream_senders::ScreenSenderArgs {
                signaling: &signaling,
                peer: &peer,
                track: &track,
                preset: selection.preset,
                rtcp_feedback: Arc::clone(&rtcp_feedback),
                stream_profile_rx: stream_profile_rx.clone(),
                bitrate_override_rx: bitrate_override_rx.clone(),
                fps_tier_override_rx: fps_tier_override_rx.clone(),
                activity_state: Arc::clone(&activity_state),
                frame_emission_paused: Arc::clone(&frame_emission_paused),
            };

            let result = match selection.backend {
                VideoEncoderBackend::OpenH264Software => {
                    run_openh264_screen_sender(sender_args()).await
                }
                VideoEncoderBackend::MediaFoundationH264 => {
                    match run_media_foundation_screen_sender(sender_args()).await {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            tracing::warn!(
                                "Native Media Foundation H264 encoder failed: {err}. Falling back to software OpenH264."
                            );
                            run_openh264_screen_sender(sender_args()).await
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
                        Arc::clone(&frame_emission_paused),
                    )
                    .await
                    {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            tracing::warn!(
                                "Hardware encoder {} failed: {err}. Falling back to software OpenH264.",
                                backend.label()
                            );
                            run_openh264_screen_sender(sender_args()).await
                        }
                    }
                }
            };

            if let Err(err) = result {
                tracing::warn!("Video sender stopped with error: {err}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_candidate() {
        let line = "candidate:1 1 udp 2113937151 192.168.1.42 51234 typ host generation 0";
        assert_eq!(parse_candidate_type(line), "host");
        assert_eq!(
            parse_candidate_address(line),
            Some(("192.168.1.42".to_string(), 51234))
        );
    }

    #[test]
    fn parse_srflx_candidate() {
        let line = "candidate:2 1 udp 1677729535 80.50.10.5 41320 typ srflx raddr 192.168.1.42 rport 51234";
        assert_eq!(parse_candidate_type(line), "srflx");
        assert_eq!(parse_candidate_address(line).map(|p| p.0), Some("80.50.10.5".to_string()));
    }

    #[test]
    fn parse_relay_candidate() {
        let line = "candidate:3 1 udp 41819647 51.158.40.10 32000 typ relay raddr 0.0.0.0 rport 0";
        assert_eq!(parse_candidate_type(line), "relay");
    }

    #[test]
    fn parse_prflx_candidate() {
        let line = "candidate:4 1 udp 1845501695 1.2.3.4 5678 typ prflx";
        assert_eq!(parse_candidate_type(line), "prflx");
    }

    #[test]
    fn parse_unknown_when_missing_typ() {
        assert_eq!(parse_candidate_type("garbage"), "unknown");
        assert_eq!(parse_candidate_type(""), "unknown");
    }

    #[test]
    fn parse_ipv6_host_candidate() {
        let line = "candidate:1 1 udp 2113937151 fe80::1 51234 typ host generation 0";
        assert_eq!(parse_candidate_type(line), "host");
        assert_eq!(parse_candidate_address(line).map(|p| p.0), Some("fe80::1".to_string()));
    }

    #[test]
    fn parse_address_returns_none_on_malformed() {
        assert!(parse_candidate_address("only one field").is_none());
        assert!(parse_candidate_address("a b c d e not-a-port").is_none());
    }
}
