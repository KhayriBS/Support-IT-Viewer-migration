//! Session orchestrator.
//!
//! Port of `Program.cs` (main loop) + `SessionManager.cs` (session lifecycle).
//!
//! Runs entirely on a background Tokio task so the Tauri UI stays responsive.
//! Exposed to the frontend via Tauri commands in `lib.rs`.
//!
//! Lifecycle:
//!   start_agent() → register → login → [heartbeat | metrics | session-poll] loop
//!   join_session() → connect signaling → dispatch signals → WebRTC / file transfer
//!   stop_agent()  → graceful shutdown

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::{interval, Duration};

use super::auth::{AgentAuthService, PendingSession};
use super::file_transfer::{FileListResponse, FileTransferService};
use super::input_handler::InputHandler;
use super::metrics::MetricsCollector;
use super::screen_capture::capture_primary_jpeg_base64;
use super::signaling::{SignalEvent, SignalType, SignalingClient};
use super::webrtc::AgentWebRtc;

// ─── Agent state (shared across Tauri commands) ───────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub running: bool,
    pub authenticated: bool,
    pub in_session: bool,
    pub machine_id: String,
    pub server_url: String,
    pub session_id: Option<i64>,
    pub technician: Option<String>,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self {
            running: false,
            authenticated: false,
            in_session: false,
            machine_id: String::new(),
            server_url: String::new(),
            session_id: None,
            technician: None,
        }
    }
}

// ─── Shared state (Arc<Mutex<…>>) ─────────────────────────────────────────────

pub struct SharedState {
    pub status: Mutex<AgentStatus>,
    pub jwt_token: Mutex<Option<String>>,
    pub signaling: Mutex<Option<Arc<SignalingClient>>>,
    pub stop_notify: Notify,
    /// Channel to push inbound chat messages to the frontend via Tauri events
    pub chat_tx: Mutex<Option<mpsc::UnboundedSender<(String, String)>>>,
}

impl SharedState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(AgentStatus::default()),
            jwt_token: Mutex::new(None),
            signaling: Mutex::new(None),
            stop_notify: Notify::new(),
            chat_tx: Mutex::new(None),
        })
    }
}

// ─── start_agent ──────────────────────────────────────────────────────────────
/// Equivalent of `static async Task Main()` in `Program.cs`.
///
/// Spawns the main agent loop in the background.
pub async fn start_agent(
    state: Arc<SharedState>,
    server_url: String,
) -> Result<(), String> {
    // Guard: already running?
    {
        let s = state.status.lock().await;
        if s.running {
            return Err("Agent already running".into());
        }
    }

    let machine_id = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let os = std::env::consts::OS.to_string();

    println!("MachineId : {machine_id}");
    println!("OS        : {os}");

    {
        let mut s = state.status.lock().await;
        s.running = true;
        s.machine_id = machine_id.clone();
        s.server_url = server_url.clone();
    }

    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) = agent_loop(state_clone, server_url, machine_id, os).await {
            eprintln!("❌ Agent loop error: {e}");
        }
    });

    Ok(())
}

// ─── stop_agent ───────────────────────────────────────────────────────────────
/// Signals the agent loop to stop gracefully.
pub async fn stop_agent(state: Arc<SharedState>) {
    state.stop_notify.notify_one();
    let mut s = state.status.lock().await;
    s.running = false;
    s.authenticated = false;
    s.in_session = false;
}

// ─── agent_loop ───────────────────────────────────────────────────────────────
/// Main loop — mirrors the `while (!cts.IsCancellationRequested)` block in C#.
async fn agent_loop(
    state: Arc<SharedState>,
    server_url: String,
    machine_id: String,
    os: String,
) -> Result<(), String> {
    let auth = AgentAuthService::new(&server_url);
    let metrics_collector = MetricsCollector::new();

    // ── Register ──────────────────────────────────────────────────────────────
    let agent = auth.register_or_update(&machine_id, &machine_id, &os).await?;
    println!("🟢 Registered: {} ({})", agent.machine_id, agent.status);

    // ── Login → JWT ───────────────────────────────────────────────────────────
    let token = auth.login(&machine_id, &os).await?;
    println!("✅ Agent authenticated (JWT received)");

    {
        *state.jwt_token.lock().await = Some(token.clone());
        let mut s = state.status.lock().await;
        s.authenticated = true;
    }

    // ── Periods (same as C#) ──────────────────────────────────────────────────
    let mut heartbeat_tick = interval(Duration::from_secs(10));
    let mut metrics_tick   = interval(Duration::from_secs(10));
    let mut session_tick   = interval(Duration::from_secs(1));

    println!("\n🔄 Agent en attente de sessions de contrôle à distance…\n");

    loop {
        tokio::select! {
            // ── Stop signal ───────────────────────────────────────────────────
            _ = state.stop_notify.notified() => {
                println!("🛑 Stop signal reçu");
                let _ = auth.mark_offline(&machine_id, &token).await;
                println!("🔴 Agent marked OFFLINE");

                // Leave active session if any
                if state.status.lock().await.in_session {
                    leave_session(Arc::clone(&state)).await;
                }
                break;
            }

            // ── Heartbeat ─────────────────────────────────────────────────────
            _ = heartbeat_tick.tick() => {
                match auth.send_heartbeat(&machine_id, &token).await {
                    Ok(_)  => println!("💓 Heartbeat @ {}", chrono::Local::now().format("%H:%M:%S")),
                    Err(e) => eprintln!("⚠️ Heartbeat error: {e}"),
                }
            }

            // ── Metrics ───────────────────────────────────────────────────────
            _ = metrics_tick.tick() => {
                let m = metrics_collector.collect();
                println!("📊 CPU={:.1}% RAM={:.1}% DISK={:.1}%",
                    m.cpu_usage, m.ram_usage, m.disk_usage);
                if let Err(e) = auth.send_metrics(&m, &token).await {
                    eprintln!("⚠️ Metrics error: {e}");
                }
            }

            // ── Session poll ──────────────────────────────────────────────────
            _ = session_tick.tick() => {
                let in_session = state.status.lock().await.in_session;
                if !in_session {
                    match auth.get_pending_session(&machine_id, &token).await {
                        Ok(Some(pending)) => {
                            println!("\n🔔 Nouvelle session! Technicien: {}", pending.technician_username);
                            if let Err(e) = join_session(Arc::clone(&state), &server_url, &pending).await {
                                eprintln!("❌ join_session error: {e}");
                            }
                        }
                        Ok(None) => {} // no pending session, normal
                        Err(e)   => eprintln!("⚠️ Session poll error: {e}"),
                    }
                }
            }
        }
    }

    Ok(())
}

// ─── join_session ─────────────────────────────────────────────────────────────
/// Equivalent of `JoinSessionAsync()` in `SessionManager.cs`.
pub async fn join_session(
    state: Arc<SharedState>,
    server_url: &str,
    pending: &PendingSession,
) -> Result<(), String> {
    // Mark in session
    {
        let mut s = state.status.lock().await;
        if s.in_session { return Ok(()); } // already in session
        s.in_session = true;
        s.session_id = Some(pending.id);
        s.technician = Some(pending.technician_username.clone());
    }

    let client = Arc::new(SignalingClient::new(server_url));
    client.set_session_id(pending.id.to_string()).await;
    *state.signaling.lock().await = Some(Arc::clone(&client));

    println!("🎯 Session démarrée (token: {}…)", &pending.signaling_token[..8.min(pending.signaling_token.len())]);

    let allow_input       = pending.allow_remote_input;
    let allow_file_xfer   = pending.allow_file_transfer;
    let input_handler     = Arc::new(InputHandler::new());
    let file_service      = FileTransferService::new();
    let state_for_signals = Arc::clone(&state);
    let token_clone       = pending.signaling_token.clone();
    let _server_url_clone  = server_url.to_string();
    let _pending_id        = pending.id;

    // ── Signal dispatch loop with auto-reconnect ───────────────────────────────
    // Equivalent of the event handlers in SessionManager / SignalingClient
    tokio::spawn(async move {
        let mut reconnect_delay = Duration::from_secs(1);
        let max_reconnect_delay = Duration::from_secs(30);

        loop {
            // Try to connect
            let (event_tx, event_rx) = mpsc::unbounded_channel::<SignalEvent>();
            
            match client.connect(&token_clone, event_tx).await {
                Ok(_) => {
                    println!("⏳ En attente de l'OFFER du viewer…");
                    reconnect_delay = Duration::from_secs(1); // reset backoff on successful connect
                    
                    // Run the signal dispatch loop
                    dispatch_signals(
                        event_rx,
                        Arc::clone(&state_for_signals),
                        allow_input,
                        allow_file_xfer,
                        Arc::clone(&input_handler),
                        &file_service,
                    ).await;
                    
                    // Connection closed — check if we should reconnect
                    {
                        let in_session = state_for_signals.status.lock().await.in_session;
                        if !in_session {
                            // Session ended intentionally via leave_session()
                            break;
                        }
                    }
                    
                    println!("🔄 Tentative de reconnexion et attente {:.1}s…", reconnect_delay.as_secs_f64());
                    tokio::time::sleep(reconnect_delay).await;
                    
                    // Exponential backoff: 1s → 2s → 4s → 8s → 16s → 30s → 30s…
                    if reconnect_delay.as_secs() < max_reconnect_delay.as_secs() {
                        reconnect_delay = Duration::from_secs(
                            (reconnect_delay.as_secs() * 2).min(max_reconnect_delay.as_secs())
                        );
                    }
                }
                Err(e) => {
                    eprintln!("❌ Reconnexion échouée: {e}");
                    println!("🔄 Nouvelle tentative en {:.1}s…", reconnect_delay.as_secs_f64());
                    tokio::time::sleep(reconnect_delay).await;
                    
                    // Exponential backoff
                    if reconnect_delay.as_secs() < max_reconnect_delay.as_secs() {
                        reconnect_delay = Duration::from_secs(
                            (reconnect_delay.as_secs() * 2).min(max_reconnect_delay.as_secs())
                        );
                    }
                }
            }
        }
    });

    Ok(())
}

// ─── dispatch_signals ──────────────────────────────────────────────────────────
/// Processes inbound signaling messages in a loop.
/// Returns when the connection is closed or session is terminated.
async fn dispatch_signals(
    mut event_rx: mpsc::UnboundedReceiver<SignalEvent>,
    state: Arc<SharedState>,
    allow_input: bool,
    allow_file_xfer: bool,
    input_handler: Arc<InputHandler>,
    file_service: &FileTransferService,
) {
    // Upload state (mirrors _uploadingFilePath/_uploadingFileAppend in C#)
    let mut uploading_path: Option<String> = None;
    let mut uploading_append = false;
    let mut webrtc: Option<AgentWebRtc> = None;
    let mut h264_sender_started = false;
    let mut startup_preview_started = false;

    while let Some(msg) = event_rx.recv().await {
        let sig_client = {
            state.signaling.lock().await.clone()
        };
        let Some(sig) = sig_client else { break };

        match msg.signal_type {
            SignalType::Join => {
                println!("👋 Viewer rejoint la session — attente de l'OFFER SDP");
            }

            // ── SDP Offer → create Answer ──────────────────────────────
            SignalType::Offer => {
                println!("📥 Offer SDP reçu du viewer");

                if webrtc.is_none() {
                    match AgentWebRtc::new(
                        Arc::clone(&sig),
                        Arc::clone(&input_handler),
                        allow_input,
                    ).await {
                        Ok(pc) => {
                            println!("🔧 WebRTC initialisé");
                            webrtc = Some(pc);
                        }
                        Err(e) => {
                            eprintln!("❌ Init WebRTC échouée: {e}");
                            continue;
                        }
                    }
                }

                if let (Some(pc), Some(payload)) = (webrtc.as_ref(), msg.payload.as_ref()) {
                    match pc.handle_offer(payload).await {
                        Ok(answer_payload) => {
                            if let Err(e) = sig.send_answer(answer_payload).await {
                                eprintln!("❌ Envoi ANSWER échoué: {e}");
                            } else {
                                println!("📤 Answer SDP envoyé");

                                if !startup_preview_started {
                                    let preview_sig = Arc::clone(&sig);
                                    tokio::spawn(async move {
                                        for _ in 0..6 {
                                            match capture_primary_jpeg_base64(55) {
                                                Ok(frame) => {
                                                    let payload = serde_json::json!({
                                                        "kind": "screen-frame",
                                                        "mime": "image/jpeg",
                                                        "data": frame,
                                                        "timestamp": chrono::Utc::now().to_rfc3339(),
                                                    });

                                                    if let Err(err) = preview_sig.send_screen_frame(payload).await {
                                                        eprintln!("⚠️ Envoi preview écran échoué: {err}");
                                                    }
                                                }
                                                Err(err) => {
                                                    eprintln!("⚠️ Capture preview écran échouée: {err}");
                                                }
                                            }

                                            tokio::time::sleep(Duration::from_millis(900)).await;
                                        }
                                    });
                                    startup_preview_started = true;
                                }

                                if !h264_sender_started {
                                    if let Some(pc) = webrtc.as_ref() {
                                        println!("🎥 Démarrage stream WebRTC H.264 (screen)");
                                        pc.start_h264_screen_sender();
                                        h264_sender_started = true;
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!("❌ Erreur WebRTC OFFER->ANSWER: {e}"),
                    }
                } else {
                    eprintln!("❌ Offer sans payload");
                }
            }

            // ── ICE candidate ─────────────────────────────────────────
            SignalType::Ice => {
                println!("🧊 ICE candidate reçu");
                if let (Some(pc), Some(payload)) = (webrtc.as_ref(), msg.payload.as_ref()) {
                    if let Err(e) = pc.add_ice_candidate(payload).await {
                        eprintln!("⚠️ ICE candidate rejeté: {e}");
                    }
                }
            }

            // ── Chat ──────────────────────────────────────────────────
            SignalType::Chat => {
                if let Some(payload) = &msg.payload {
                    let content     = payload["content"].as_str().unwrap_or("").to_string();
                    let sender_name = payload["senderName"].as_str().unwrap_or("?").to_string();
                    println!("💬 [{sender_name}]: {content}");

                    // Forward to frontend via chat channel
                    if let Some(tx) = state.chat_tx.lock().await.as_ref() {
                        let _ = tx.send((sender_name, content));
                    }
                }
            }

            // ── LEAVE ─────────────────────────────────────────────────
            SignalType::Leave => {
                println!("🚪 Signal LEAVE reçu — fermeture de la session");
                leave_session(Arc::clone(&state)).await;
                break;
            }

            // ── File: list request ────────────────────────────────────
            SignalType::FileListRequest => {
                if !allow_file_xfer {
                    let _ = sig.send_file_error("File transfer refused by remote user").await;
                    continue;
                }
                let path = msg.payload
                    .as_ref()
                    .and_then(|p| p["path"].as_str())
                    .unwrap_or("");

                println!("📂 Demande liste fichiers: {path}");
                let listing = file_service.get_directory_listing(path);
                let json = serde_json::to_value(&listing).unwrap_or_default();
                let _ = sig.send_file_list(json).await;
            }

            // ── File: download request ────────────────────────────────
            SignalType::FileDownloadRequest => {
                if !allow_file_xfer {
                    let _ = sig.send_file_error("File transfer refused by remote user").await;
                    continue;
                }
                if let Some(path) = msg.payload.as_ref().and_then(|p| p["path"].as_str()) {
                    println!("📥 Téléchargement demandé: {path}");
                    let chunks = file_service.read_file_chunks(path);
                    if chunks.is_empty() {
                        let _ = sig.send_file_error("File not found or unreadable").await;
                    } else {
                        let file_name = chunks[0].file_name.clone();
                        for chunk in chunks {
                            let json = serde_json::to_value(&chunk).unwrap_or_default();
                            let _ = sig.send_file_data(json).await;
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        let _ = sig.send_file_complete(&file_name).await;
                        println!("✅ Fichier envoyé: {path}");
                    }
                }
            }

            // ── File: upload start ────────────────────────────────────
            SignalType::FileUploadRequest => {
                if !allow_file_xfer {
                    let _ = sig.send_file_error("File transfer refused by remote user").await;
                    continue;
                }
                if let Some(file_name) = msg.payload.as_ref()
                    .and_then(|p| p["fileName"].as_str())
                {
                    let safe_name = std::path::Path::new(file_name)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| file_name.to_string());

                    let downloads = FileTransferService::get_downloads_path();
                    uploading_path   = Some(downloads.join(safe_name).to_string_lossy().to_string());
                    uploading_append = false;
                    println!("📤 Upload démarré: {:?}", uploading_path);
                }
            }

            // ── File: data chunk ──────────────────────────────────────
            SignalType::FileData => {
                if let (Some(dest), Some(payload)) = (&uploading_path, &msg.payload) {
                    let data         = payload["data"].as_str().unwrap_or("");
                    let chunk_index  = payload["chunkIndex"].as_u64().unwrap_or(0);
                    let total_chunks = payload["totalChunks"].as_u64().unwrap_or(1);

                    match file_service.save_file_async(dest, data, uploading_append).await {
                        Ok(_) => {
                            uploading_append = true;
                            println!("📦 Chunk {}/{} reçu", chunk_index + 1, total_chunks);
                            if chunk_index + 1 >= total_chunks {
                                println!("✅ Fichier reçu: {dest}");
                                uploading_path   = None;
                                uploading_append = false;
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Erreur sauvegarde: {e}");
                            let _ = sig.send_file_error(&e).await;
                        }
                    }
                }
            }

            // ── Input from DataChannel (sent via WebRTC, not signaling) ──
            // Handled in the WebRTC layer; here for completeness / future use
            _ => {
                println!("📨 Signal ignoré: {:?}", msg.signal_type);
            }
        }

    }
}

// ─── leave_session ────────────────────────────────────────────────────────────
/// Equivalent of `LeaveSessionAsync()` in `SessionManager.cs`.
pub async fn leave_session(state: Arc<SharedState>) {
    if let Some(sig) = state.signaling.lock().await.take() {
        sig.disconnect().await;
    }

    let mut s = state.status.lock().await;
    s.in_session = false;
    s.session_id = None;
    s.technician = None;

    println!("🚪 Session terminée");
}

// ─── send_chat ────────────────────────────────────────────────────────────────
pub async fn send_chat_message(
    state: Arc<SharedState>,
    content: String,
    sender_name: String,
) -> Result<(), String> {
    let sig_opt = state.signaling.lock().await.clone();
    match sig_opt {
        Some(sig) => sig.send_chat(&content, &sender_name).await,
        None => Err("Pas en session".into()),
    }
}

// ─── get_file_list ────────────────────────────────────────────────────────────
pub fn get_file_list(path: &str) -> FileListResponse {
    FileTransferService::new().get_directory_listing(path)
}
