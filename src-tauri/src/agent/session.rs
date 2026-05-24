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
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::{interval, Duration};

use super::auth::{AgentAuthService, PendingSession};
use super::file_transfer::{FileListResponse, FileTransferService};
use super::input_handler::InputHandler;
use super::metrics::MetricsCollector;
use super::signaling::{SignalEvent, SignalType, SignalingClient};
use super::webrtc::{AgentWebRtc, FpsTier, StreamQualityProfile};

// Re-export for visibility in SharedState type
type SharedWebRtc = Arc<AgentWebRtc>;

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
    /// Persisted WebRTC peer so it survives signaling reconnects / grace
    /// windows. Cleared only on real session end (`leave_session`).
    pub webrtc: Mutex<Option<SharedWebRtc>>,
    /// Monotonic counter incremented on any sign of viewer activity
    /// (JOIN, OFFER, ICE, etc.). Used by the grace-period task to detect
    /// whether the viewer came back before the timeout expires.
    pub viewer_activity_epoch: AtomicU64,
    /// True while a grace task is in flight. Used by error paths (e.g. 1003)
    /// to defer to the grace decision instead of killing the session.
    pub grace_active: AtomicBool,
    /// Number of consecutive `1003` socket closes received during the current
    /// grace window. After a small threshold we treat the server's verdict as
    /// definitive ("session no longer recognised") and abort the grace early.
    pub consecutive_1003: AtomicU64,
    /// Notifies the in-flight grace-period task that it should cancel
    /// (viewer is back, or session is shutting down for another reason).
    pub grace_cancel: Notify,
    pub stop_notify: Notify,
    /// Channel to push inbound chat messages to the frontend via Tauri events
    pub chat_tx: Mutex<Option<mpsc::UnboundedSender<(String, String)>>>,
    /// ANSWER calculé mais pas encore livré (signaling fermée au moment de
    /// l'envoi). Re-tenté à chaque entrée fraîche dans dispatch_signals tant
    /// qu'il n'a pas été acquitté (= jusqu'à ce que send réussisse). Cleared
    /// par leave_session.
    pub pending_answer: Mutex<Option<serde_json::Value>>,
}

impl SharedState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(AgentStatus::default()),
            jwt_token: Mutex::new(None),
            signaling: Mutex::new(None),
            webrtc: Mutex::new(None),
            viewer_activity_epoch: AtomicU64::new(0),
            grace_active: AtomicBool::new(false),
            consecutive_1003: AtomicU64::new(0),
            grace_cancel: Notify::new(),
            stop_notify: Notify::new(),
            chat_tx: Mutex::new(None),
            pending_answer: Mutex::new(None),
        })
    }
}

/// Default grace period (seconds) granted when the viewer disconnects
/// non-explicitly (peer_disconnected / socket close 1000 / 1006…).
/// Configurable via `LUMIERE_VIEWER_GRACE_SECS` (env ou .env.local).
///
/// Default monté à 180 s : sur Render free-tier la WS signaling se ferme en
/// 1003/1011 dès l'OFFER/ANSWER échangé, et notre reconnect peut prendre
/// jusqu'à plusieurs dizaines de secondes (back-off + reject côté serveur).
/// Tant que le peer WebRTC est Connected, on n'a aucune raison de tuer la
/// session — la grâce est de toute façon ré-armée à chaque expiration tant
/// que `is_peer_connected()` répond true.
fn viewer_grace_period_secs() -> u64 {
    let raw = std::env::var("LUMIERE_VIEWER_GRACE_SECS")
        .ok()
        .or_else(|| {
            // Fallback .env.local pour les builds dev Tauri.
            std::fs::read_to_string("../.env.local")
                .ok()
                .and_then(|content| {
                    content.lines().find_map(|line| {
                        let line = line.trim();
                        if line.starts_with("LUMIERE_VIEWER_GRACE_SECS=") {
                            Some(line.trim_start_matches("LUMIERE_VIEWER_GRACE_SECS=").to_string())
                        } else {
                            None
                        }
                    })
                })
        });
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.clamp(5, 600))
        .unwrap_or(180)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchOutcome {
    Reconnect,
    Stop,
}

fn env_flag_true(key: &str) -> bool {
    let Ok(value) = std::env::var(key) else {
        return false;
    };

    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn session_structured_log_enabled() -> bool {
    env_flag_true("LUMIERE_SESSION_STRUCTURED_LOG")
        || env_flag_true("LUMIERE_STREAM_STRUCTURED_LOG")
}

fn session_log_interval_secs() -> u64 {
    std::env::var("LUMIERE_SESSION_LOG_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(1, 300))
        .unwrap_or(10)
}

fn is_retryable_signaling_close_code(code: u16) -> bool {
    // 1000 = normal close — many free-tier signaling gateways (Render, Fly,
    // load-balancers with idle timeouts) send a clean 1000 on connection
    // recycle even though the session is still valid; treat it as retryable.
    // 1006/1011/1012/1013 = abnormal/server-side transient closes.
    matches!(code, 1000 | 1006 | 1011 | 1012 | 1013)
}

fn log_session_event(event: &str, payload: serde_json::Value) {
    if !session_structured_log_enabled() {
        return;
    }

    let envelope = serde_json::json!({
        "event": event,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "payload": payload,
    });
    tracing::info!("{envelope}");
}

/// Spawn an async grace-period task. Keeps the session ALIVE for `grace_secs`
/// while the viewer is allowed to come back (refresh / mini network blip /
/// tab close-and-reopen).
///
/// Cancellation paths:
///   • `state.grace_cancel.notify_one()` (viewer returned, or stop_agent)
///   • viewer_activity_epoch changed since the snapshot taken at scheduling
///   • session already left
fn schedule_viewer_grace_period(
    state: Arc<SharedState>,
    reason: &'static str,
) {
    // Idempotent: if a grace task is already running, don't stack another.
    // The existing one will be cancelled by activity / shutdown anyway.
    if state.grace_active.swap(true, Ordering::AcqRel) {
        tracing::info!("ℹ️ Grâce déjà active ({reason}) — pas de nouveau timer");
        return;
    }

    let grace_secs = viewer_grace_period_secs();
    let activity_at_start = state.viewer_activity_epoch.load(Ordering::Relaxed);
    // Reset counter for this fresh grace window.
    state.consecutive_1003.store(0, Ordering::Release);

    tracing::info!(
        "⏳ Viewer parti ({reason}) — fenêtre de grâce {grace_secs}s avant fermeture définitive"
    );
    log_session_event(
        "viewer_grace_started",
        serde_json::json!({
            "reason": reason,
            "graceSeconds": grace_secs,
            "activityEpochAtStart": activity_at_start,
        }),
    );

    tokio::spawn(async move {
        let cancel_fired = tokio::select! {
            _ = state.grace_cancel.notified() => true,
            _ = tokio::time::sleep(Duration::from_secs(grace_secs)) => false,
        };

        // Always clear the active flag before returning.
        state.grace_active.store(false, Ordering::Release);

        if cancel_fired {
            // Distinguish: was this a real viewer return, or a session shutdown
            // (leave_session/stop_agent fire the same Notify)?
            let in_session = state.status.lock().await.in_session;
            if !in_session {
                tracing::info!("ℹ️ Grâce annulée — session déjà fermée (shutdown)");
                log_session_event(
                    "viewer_grace_cancelled",
                    serde_json::json!({ "reason": "shutdown" }),
                );
            } else {
                let activity_now = state.viewer_activity_epoch.load(Ordering::Relaxed);
                tracing::info!(
                    "✅ Viewer revenu avant expiration ({activity_at_start} → {activity_now}) — fermeture annulée"
                );
                log_session_event(
                    "viewer_grace_cancelled",
                    serde_json::json!({
                        "reason": "viewer_returned",
                        "activityEpochAtStart": activity_at_start,
                        "activityEpochNow": activity_now,
                    }),
                );
            }
            return;
        }

        // Sleep elapsed without cancel. Race-safe re-check before teardown.
        let in_session = state.status.lock().await.in_session;
        if !in_session {
            tracing::info!("ℹ️ Grâce expirée mais session déjà fermée — no-op");
            return;
        }

        let activity_now = state.viewer_activity_epoch.load(Ordering::Relaxed);
        if activity_now != activity_at_start {
            tracing::info!(
                "✅ Activité viewer détectée pendant la grâce ({activity_at_start} → {activity_now}) — session conservée"
            );
            log_session_event(
                "viewer_grace_cancelled",
                serde_json::json!({
                    "reason": "activity_detected",
                    "activityEpochAtStart": activity_at_start,
                    "activityEpochNow": activity_now,
                }),
            );
            return;
        }

        // Final safety net: if the WebRTC peer is still Connected, the viewer
        // is alive and well — only the signaling socket is unhappy. Tearing
        // down a healthy P2P pipe just because the (free-tier) signaling
        // server keeps closing on us would be wrong. Re-arm a fresh grace
        // window and let it expire only when the peer itself dies.
        let webrtc_alive = {
            let guard = state.webrtc.lock().await;
            guard.as_ref().map(|pc| pc.is_peer_connected()).unwrap_or(false)
        };
        if webrtc_alive {
            tracing::info!(
                "🛡️ Grâce expirée mais peer WebRTC toujours Connected — session maintenue, nouvelle fenêtre de grâce armée"
            );
            log_session_event(
                "viewer_grace_extended_peer_alive",
                serde_json::json!({
                    "reason": "peer_still_connected",
                    "graceSeconds": grace_secs,
                }),
            );
            schedule_viewer_grace_period(Arc::clone(&state), "peer_still_connected");
            return;
        }

        tracing::info!("⛔ Délai de grâce expiré sans retour viewer — fermeture session");
        log_session_event(
            "viewer_grace_expired",
            serde_json::json!({
                "reason": reason,
                "graceSeconds": grace_secs,
            }),
        );
        leave_session(state).await;
    });
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

    tracing::info!("MachineId : {machine_id}");
    tracing::info!("OS        : {os}");

    {
        let mut s = state.status.lock().await;
        s.running = true;
        s.machine_id = machine_id.clone();
        s.server_url = server_url.clone();
    }

    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) = agent_loop(state_clone, server_url, machine_id, os).await {
            tracing::warn!("❌ Agent loop error: {e}");
        }
    });

    Ok(())
}

// ─── stop_agent ───────────────────────────────────────────────────────────────
/// Signals the agent loop to stop gracefully.
pub async fn stop_agent(state: Arc<SharedState>) {
    // Cancel any in-flight grace-period task — we're shutting down anyway.
    state.grace_cancel.notify_waiters();
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
    tracing::info!("🟢 Registered: {} ({})", agent.machine_id, agent.status);

    // ── Login → JWT ───────────────────────────────────────────────────────────
    let token = auth.login(&machine_id, &os).await?;
    tracing::info!("✅ Agent authenticated (JWT received)");

    {
        *state.jwt_token.lock().await = Some(token.clone());
        let mut s = state.status.lock().await;
        s.authenticated = true;
    }

    // ── Periods (same as C#) ──────────────────────────────────────────────────
    let mut heartbeat_tick = interval(Duration::from_secs(10));
    let mut metrics_tick   = interval(Duration::from_secs(10));
    let mut session_tick   = interval(Duration::from_secs(1));

    tracing::info!("\n🔄 Agent en attente de sessions de contrôle à distance…\n");

    loop {
        tokio::select! {
            // ── Stop signal ───────────────────────────────────────────────────
            _ = state.stop_notify.notified() => {
                tracing::info!("🛑 Stop signal reçu");
                let _ = auth.mark_offline(&machine_id, &token).await;
                tracing::info!("🔴 Agent marked OFFLINE");

                // Leave active session if any
                if state.status.lock().await.in_session {
                    leave_session(Arc::clone(&state)).await;
                }
                break;
            }

            // ── Heartbeat ─────────────────────────────────────────────────────
            _ = heartbeat_tick.tick() => {
                match auth.send_heartbeat(&machine_id, &token).await {
                    Ok(_)  => tracing::info!("💓 Heartbeat @ {}", chrono::Local::now().format("%H:%M:%S")),
                    Err(e) => tracing::warn!("⚠️ Heartbeat error: {e}"),
                }
            }

            // ── Metrics ───────────────────────────────────────────────────────
            _ = metrics_tick.tick() => {
                let m = metrics_collector.collect();
                tracing::info!("📊 CPU={:.1}% RAM={:.1}% DISK={:.1}%",
                    m.cpu_usage, m.ram_usage, m.disk_usage);
                if let Err(e) = auth.send_metrics(&m, &token).await {
                    tracing::warn!("⚠️ Metrics error: {e}");
                }
            }

            // ── Session poll ──────────────────────────────────────────────────
            _ = session_tick.tick() => {
                let in_session = state.status.lock().await.in_session;
                if !in_session {
                    match auth.get_pending_session(&machine_id, &token).await {
                        Ok(Some(pending)) => {
                            tracing::info!("\n🔔 Nouvelle session! Technicien: {}", pending.technician_username);
                            if let Err(e) = join_session(Arc::clone(&state), &server_url, &pending).await {
                                tracing::warn!("❌ join_session error: {e}");
                            }
                        }
                        Ok(None) => {} // no pending session, normal
                        Err(e)   => tracing::warn!("⚠️ Session poll error: {e}"),
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

    tracing::info!("🎯 Session démarrée (token: {}…)", &pending.signaling_token[..8.min(pending.signaling_token.len())]);
    log_session_event(
        "session_start",
        serde_json::json!({
            "sessionId": pending.id,
            "technician": pending.technician_username,
            "allowRemoteInput": pending.allow_remote_input,
            "allowFileTransfer": pending.allow_file_transfer,
        }),
    );

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
        let mut reconnect_attempt: u64 = 0;
        let reconnect_log_interval = Duration::from_secs(session_log_interval_secs());
        let mut last_reconnect_log = std::time::Instant::now()
            .checked_sub(reconnect_log_interval)
            .unwrap_or_else(std::time::Instant::now);

        loop {
            if !state_for_signals.status.lock().await.in_session {
                break;
            }

            // Try to connect
            let (event_tx, event_rx) = mpsc::unbounded_channel::<SignalEvent>();

            match client.connect(&token_clone, event_tx).await {
                Ok(_) => {
                    tracing::info!("⏳ En attente de l'OFFER du viewer…");
                    let connected_at = std::time::Instant::now();

                    let dispatch_outcome = dispatch_signals(
                        event_rx,
                        Arc::clone(&state_for_signals),
                        allow_input,
                        allow_file_xfer,
                        Arc::clone(&input_handler),
                        &file_service,
                    ).await;

                    if dispatch_outcome == DispatchOutcome::Stop {
                        break;
                    }

                    if !state_for_signals.status.lock().await.in_session {
                        break;
                    }

                    if connected_at.elapsed() >= Duration::from_secs(8) {
                        reconnect_delay = Duration::from_secs(1);
                        reconnect_attempt = 0;
                    }

                    reconnect_attempt = reconnect_attempt.saturating_add(1);
                    if last_reconnect_log.elapsed() >= reconnect_log_interval {
                        log_session_event(
                            "signaling_reconnect_scheduled",
                            serde_json::json!({
                                "attempt": reconnect_attempt,
                                "delaySeconds": reconnect_delay.as_secs_f64(),
                                "reason": "dispatch_reconnect",
                            }),
                        );
                        last_reconnect_log = std::time::Instant::now();
                    }

                    tracing::info!("🔄 Tentative de reconnexion et attente {:.1}s…", reconnect_delay.as_secs_f64());
                    tokio::time::sleep(reconnect_delay).await;

                    if !state_for_signals.status.lock().await.in_session {
                        break;
                    }

                    if reconnect_delay.as_secs() < max_reconnect_delay.as_secs() {
                        reconnect_delay = Duration::from_secs(
                            (reconnect_delay.as_secs() * 2).min(max_reconnect_delay.as_secs())
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("❌ Reconnexion échouée: {e}");

                    if !state_for_signals.status.lock().await.in_session {
                        break;
                    }

                    reconnect_attempt = reconnect_attempt.saturating_add(1);
                    if last_reconnect_log.elapsed() >= reconnect_log_interval {
                        log_session_event(
                            "signaling_reconnect_scheduled",
                            serde_json::json!({
                                "attempt": reconnect_attempt,
                                "delaySeconds": reconnect_delay.as_secs_f64(),
                                "reason": "connect_error",
                                "error": e,
                            }),
                        );
                        last_reconnect_log = std::time::Instant::now();
                    }

                    tracing::info!("🔄 Nouvelle tentative en {:.1}s…", reconnect_delay.as_secs_f64());
                    tokio::time::sleep(reconnect_delay).await;

                    if !state_for_signals.status.lock().await.in_session {
                        break;
                    }

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
) -> DispatchOutcome {
    // Upload state (mirrors _uploadingFilePath/_uploadingFileAppend in C#)
    let mut uploading_path: Option<String> = None;
    let mut uploading_append = false;
    // Hydrate from SharedState so the peer survives signaling reconnects /
    // grace windows. None on first dispatch, Some(...) on subsequent entries
    // when a viewer is reconnecting.
    let mut webrtc: Option<Arc<AgentWebRtc>> = state.webrtc.lock().await.clone();
    // If we already had a peer, the H264 sender task was started in a previous
    // dispatch. Don't restart it (it would duplicate the encoder + capture).
    let mut h264_sender_started = webrtc.is_some();
    let mut last_offer_fingerprint: Option<u64> = None;
    let mut requested_stream_profile = StreamQualityProfile::Responsive;

    while let Some(msg) = event_rx.recv().await {
        let sig_client = {
            state.signaling.lock().await.clone()
        };
        let Some(sig) = sig_client else { break };

        match msg.signal_type {
            SignalType::Join => {
                state.viewer_activity_epoch.fetch_add(1, Ordering::Relaxed);
                // Real viewer activity → reset 1003 counter, cancel grace.
                state.consecutive_1003.store(0, Ordering::Release);
                state.grace_cancel.notify_waiters();
                tracing::info!("👋 Viewer rejoint la session — attente de l'OFFER SDP");

                // Si on a une ANSWER pendante (signaling était tombée juste
                // avant son envoi sur la dispatch précédente), on la renvoie
                // dès que le viewer revient en ligne — évite que le viewer
                // boucle sur "Aucune réponse SDP recue" pendant les coupures
                // signaling de Render.
                let pending = state.pending_answer.lock().await.clone();
                if let Some(answer) = pending {
                    tracing::info!("🔁 Renvoi de l'ANSWER pendante au viewer (reconnect signaling)");
                    match sig.send_answer(answer).await {
                        Ok(()) => {
                            *state.pending_answer.lock().await = None;
                            tracing::info!("📤 Answer SDP renvoyé avec succès");
                            if !h264_sender_started {
                                if let Some(pc) = webrtc.as_ref() {
                                    tracing::info!("🎥 Démarrage stream WebRTC H.264 (screen)");
                                    pc.start_h264_screen_sender();
                                    h264_sender_started = true;
                                }
                            }
                        }
                        Err(e) => tracing::warn!("⚠️ Renvoi ANSWER pendante échoué: {e}"),
                    }
                }
            }

            // ── SDP Offer → create Answer ──────────────────────────────
            SignalType::Offer => {
                state.viewer_activity_epoch.fetch_add(1, Ordering::Relaxed);
                state.consecutive_1003.store(0, Ordering::Release);
                state.grace_cancel.notify_waiters();
                tracing::info!("📥 Offer SDP reçu du viewer");

                let current_offer_fingerprint = msg
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("sdp"))
                    .and_then(|value| value.as_str())
                    .map(|sdp| {
                        let mut hasher = DefaultHasher::new();
                        sdp.hash(&mut hasher);
                        hasher.finish()
                    });

                if current_offer_fingerprint.is_some()
                    && current_offer_fingerprint == last_offer_fingerprint
                {
                    tracing::info!("⚠️ OFFER dupliqué ignoré (même SDP)");
                    continue;
                }

                if webrtc.is_none() {
                    match AgentWebRtc::new(
                        Arc::clone(&sig),
                        Arc::clone(&input_handler),
                        allow_input,
                        allow_file_xfer,
                    ).await {
                        Ok(pc) => {
                            tracing::info!("🔧 WebRTC initialisé");
                            let arc_pc = Arc::new(pc);
                            // Persist so it survives signaling reconnects / grace.
                            *state.webrtc.lock().await = Some(Arc::clone(&arc_pc));
                            webrtc = Some(arc_pc);
                        }
                        Err(e) => {
                            tracing::warn!("❌ Init WebRTC échouée: {e}");
                            continue;
                        }
                    }
                }

                if let (Some(pc), Some(payload)) = (webrtc.as_ref(), msg.payload.as_ref()) {
                    match pc.handle_offer(payload).await {
                        Ok(answer_payload) => {
                            pc.set_stream_profile(requested_stream_profile);
                            last_offer_fingerprint = current_offer_fingerprint;
                            // Stocke l'ANSWER pour qu'il soit re-tenté si la
                            // signaling est tombée juste avant son envoi.
                            *state.pending_answer.lock().await = Some(answer_payload.clone());
                            match sig.send_answer(answer_payload).await {
                                Ok(()) => {
                                    tracing::info!("📤 Answer SDP envoyé");
                                    *state.pending_answer.lock().await = None;
                                    if !h264_sender_started {
                                        if let Some(pc) = webrtc.as_ref() {
                                            tracing::info!("🎥 Démarrage stream WebRTC H.264 (screen)");
                                            pc.start_h264_screen_sender();
                                            h264_sender_started = true;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "❌ Envoi ANSWER échoué: {e} — gardé pour retry au prochain reconnect signaling"
                                    );
                                }
                            }
                        }
                        Err(e) => tracing::warn!("❌ Erreur WebRTC OFFER->ANSWER: {e}"),
                    }
                } else {
                    tracing::warn!("❌ Offer sans payload");
                }
            }

            // ── ICE candidate ─────────────────────────────────────────
            SignalType::Ice => {
                state.viewer_activity_epoch.fetch_add(1, Ordering::Relaxed);
                state.consecutive_1003.store(0, Ordering::Release);
                state.grace_cancel.notify_waiters();
                tracing::info!("🧊 ICE candidate reçu");
                if let (Some(pc), Some(payload)) = (webrtc.as_ref(), msg.payload.as_ref()) {
                    if let Err(e) = pc.add_ice_candidate(payload).await {
                        tracing::warn!("⚠️ ICE candidate rejeté: {e}");
                    }
                }
            }

            // ── Chat ──────────────────────────────────────────────────
            SignalType::Chat => {
                if let Some(payload) = &msg.payload {
                    let content     = payload["content"].as_str().unwrap_or("").to_string();
                    let sender_name = payload["senderName"].as_str().unwrap_or("?").to_string();
                    tracing::info!("💬 [{sender_name}]: {content}");

                    // Forward to frontend via chat channel
                    if let Some(tx) = state.chat_tx.lock().await.as_ref() {
                        let _ = tx.send((sender_name, content));
                    }
                }
            }

            SignalType::StreamProfile => {
                let profile = msg
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("profile"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(|raw| StreamQualityProfile::from_payload(raw))
                    .unwrap_or(StreamQualityProfile::Quality);

                requested_stream_profile = profile;
                if let Some(pc) = webrtc.as_ref() {
                    pc.set_stream_profile(profile);

                    // Champ optionnel `paused` : suspend/reprend toute émission
                    // de frame pendant un transfert de fichier.
                    if let Some(paused) = msg
                        .payload
                        .as_ref()
                        .and_then(|p| p.get("paused"))
                        .and_then(serde_json::Value::as_bool)
                    {
                        pc.set_frame_emission_paused(paused);
                    }

                    if let Some(bitrate_bps) = msg
                        .payload
                        .as_ref()
                        .and_then(|p| p.get("bitrateBps"))
                        .and_then(serde_json::Value::as_u64)
                    {
                        pc.adjust_bitrate(bitrate_bps as u32);
                    }

                    if let Some(fps_tier) = msg
                        .payload
                        .as_ref()
                        .and_then(|p| p.get("fpsTier"))
                        .and_then(serde_json::Value::as_str)
                        .and_then(FpsTier::from_payload)
                    {
                        pc.set_fps_tier(fps_tier);
                    }
                }
                tracing::info!("🎛️ Stream profile reçu: {:?}", profile);
            }
            // ── LEAVE ─────────────────────────────────────────────────
            SignalType::Leave => {
                let leave_reason = msg
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();

                let explicit_manual_leave = matches!(
                    leave_reason.as_str(),
                    "manual_disconnect" | "manual" | "user" | "explicit"
                );

                tracing::info!(
                    "🚪 Signal LEAVE reçu de '{}' (reason='{}', manual={})",
                    msg.from, leave_reason, explicit_manual_leave
                );

                if explicit_manual_leave {
                    // Explicit user action → tear down immediately, like before.
                    if let Some(pc) = webrtc.as_ref() {
                        pc.close().await;
                    }
                    log_session_event(
                        "session_stop",
                        serde_json::json!({
                            "reason": "viewer_leave_manual",
                            "from": msg.from,
                            "leaveReason": leave_reason,
                        }),
                    );
                    leave_session(Arc::clone(&state)).await;
                    return DispatchOutcome::Stop;
                }

                // Non-explicit (peer_disconnected, refresh, tab close, network blip…):
                // do NOT touch the WebRTC peer or the session state. Schedule a grace
                // window during which the viewer may come back via JOIN/OFFER/ICE.
                // The signaling socket reconnect loop will keep retrying meanwhile.
                schedule_viewer_grace_period(Arc::clone(&state), "viewer_leave_remote");

                // Break out of dispatch so the outer loop reconnects the signaling
                // socket. The session remains alive; if the viewer returns within
                // the window, JOIN/OFFER will cancel the grace task.
                break;
            }

            SignalType::Error => {
                if let Some(payload) = &msg.payload {
                    let is_socket_close = payload
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        .map(|kind| kind == "socket-close")
                        .unwrap_or(false);

                    if is_socket_close {
                        let close_code = payload
                            .get("code")
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|value| u16::try_from(value).ok());

                        if close_code == Some(1003) {
                            // 1003 = server says "no" (token/session invalid).
                            //
                            // BUT — if the WebRTC peer is already Connected, the
                            // signaling socket is no longer required: video, audio,
                            // input and file transfer all flow peer-to-peer.
                            // The signaling server going away (free-tier instability,
                            // session pruned after viewer's LEAVE was forwarded, …)
                            // must NOT tear down a healthy peer connection.
                            let peer_connected = match webrtc.as_ref() {
                                Some(pc) => pc.is_peer_connected(),
                                None => false,
                            };
                            if peer_connected {
                                tracing::info!(
                                    "🛡️ 1003 ignoré — peer WebRTC déjà Connected, signaling devenu optionnel"
                                );
                                schedule_viewer_grace_period(
                                    Arc::clone(&state),
                                    "socket_close_1003_peer_connected",
                                );
                                break; // outer loop will retry signaling reconnect
                            }

                            // Outside grace → the session is dead, close immediately.
                            // Inside grace → the server may be transiently unhappy
                            //   (e.g. brief race with the viewer's LEAVE). We allow
                            //   ONE attempt: if a second 1003 follows on the next
                            //   reconnect, the server's verdict is definitive and we
                            //   honor it (the viewer cannot come back on this token).
                            const MAX_1003_DURING_GRACE: u64 = 1;

                            if state.grace_active.load(Ordering::Acquire) {
                                let count = state
                                    .consecutive_1003
                                    .fetch_add(1, Ordering::AcqRel)
                                    + 1;

                                if count > MAX_1003_DURING_GRACE {
                                    tracing::info!(
                                        "⛔ {count} × 1003 pendant la grâce — verdict serveur définitif, fermeture session"
                                    );
                                    log_session_event(
                                        "session_stop",
                                        serde_json::json!({
                                            "reason": "socket_close_1003_grace_exceeded",
                                            "consecutive1003": count,
                                        }),
                                    );
                                    leave_session(Arc::clone(&state)).await;
                                    return DispatchOutcome::Stop;
                                }

                                tracing::info!(
                                    "🛡️ Signal fermé (1003 #{count}) pendant la grâce — un dernier essai puis abandon"
                                );
                                log_session_event(
                                    "socket_close_1003_during_grace",
                                    serde_json::json!({
                                        "reason": "deferred_to_grace",
                                        "consecutive1003": count,
                                    }),
                                );
                                break; // exit dispatch; outer loop retries once
                            }

                            tracing::info!(
                                "⛔ Signal fermé par serveur (1003), fin de session locale"
                            );
                            log_session_event(
                                "session_stop",
                                serde_json::json!({
                                    "reason": "socket_close_1003",
                                }),
                            );
                            leave_session(Arc::clone(&state)).await;
                            return DispatchOutcome::Stop;
                        }

                        let is_retryable = close_code
                            .map(is_retryable_signaling_close_code)
                            .unwrap_or(true);

                        if !is_retryable {
                            tracing::info!(
                                "⛔ Signal fermé (code {:?}), pas de reconnexion automatique",
                                close_code
                            );
                            log_session_event(
                                "session_stop",
                                serde_json::json!({
                                    "reason": "socket_close_non_retryable",
                                    "closeCode": close_code,
                                }),
                            );
                            leave_session(Arc::clone(&state)).await;
                            return DispatchOutcome::Stop;
                        }

                        // Transient close (1000/1006/1011/1012/1013): keep the
                        // session alive, let the outer loop reconnect signaling,
                        // and arm a grace timer so we don't leak a session if the
                        // viewer never reappears. JOIN/OFFER/ICE will cancel it.
                        tracing::info!(
                            "🔌 Signal fermé (code {:?}), reconnexion autorisée + grâce armée",
                            close_code
                        );
                        schedule_viewer_grace_period(
                            Arc::clone(&state),
                            "signaling_socket_close",
                        );
                        break;
                    }

                    let is_peer_not_connected = payload
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(|message| message.contains("Peer not connected: viewer"))
                        .unwrap_or(false);

                    if is_peer_not_connected {
                        tracing::info!(
                            "ℹ️ Viewer pas encore connecté (signal normal pendant l'initialisation)"
                        );
                    } else {
                        tracing::warn!("⚠️ Signal ERROR serveur: {payload}");
                    }
                } else {
                    tracing::warn!("⚠️ Signal ERROR serveur sans payload");
                }
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

                tracing::info!("📂 Demande liste fichiers: {path}");
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
                    tracing::info!("📥 Téléchargement demandé: {path}");
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
                        tracing::info!("✅ Fichier envoyé: {path}");
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
                    tracing::info!("📤 Upload démarré: {:?}", uploading_path);
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
                            tracing::info!("📦 Chunk {}/{} reçu", chunk_index + 1, total_chunks);
                            if chunk_index + 1 >= total_chunks {
                                tracing::info!("✅ Fichier reçu: {dest}");
                                uploading_path   = None;
                                uploading_append = false;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("❌ Erreur sauvegarde: {e}");
                            let _ = sig.send_file_error(&e).await;
                        }
                    }
                }
            }

            // ── Input from DataChannel (sent via WebRTC, not signaling) ──
            // Handled in the WebRTC layer; here for completeness / future use
            _ => {
                tracing::info!("📨 Signal ignoré: {:?}", msg.signal_type);
            }
        }

    }
    // NOTE: we intentionally do NOT close the WebRTC peer here. The peer is
    // persisted in `state.webrtc` so it survives signaling reconnects and
    // viewer-side refreshes. It is closed only by `leave_session()` (which
    // runs on a true session end: manual leave, 1003, grace expiry, …).
    if state.status.lock().await.in_session {
        DispatchOutcome::Reconnect
    } else {
        DispatchOutcome::Stop
    }
}

// ─── leave_session ────────────────────────────────────────────────────────────
/// Equivalent of `LeaveSessionAsync()` in `SessionManager.cs`.
pub async fn leave_session(state: Arc<SharedState>) {
    // Mark in_session=false FIRST so the grace task (if any) sees a closed
    // session when it wakes from grace_cancel and logs "shutdown" instead of
    // a false "viewer returned".
    {
        let mut s = state.status.lock().await;
        s.in_session = false;
        s.session_id = None;
        s.technician = None;
    }

    // Cancel any pending grace task; the session is going away for real now.
    state.grace_cancel.notify_waiters();

    // Reset pending answer
    *state.pending_answer.lock().await = None;

    // Tear down the persisted WebRTC peer (DataChannels, video sender, …).
    if let Some(pc) = state.webrtc.lock().await.take() {
        pc.close().await;
    }

    if let Some(sig) = state.signaling.lock().await.take() {
        sig.disconnect().await;
    }

    tracing::info!("🚪 Session terminée");
    log_session_event(
        "session_closed",
        serde_json::json!({
            "inSession": false,
        }),
    );
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

