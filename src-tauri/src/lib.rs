// Lumière IT — Tauri backend entry point
//
// Exposes all Rust functions to the SvelteKit frontend via `invoke()`.
// Each `#[tauri::command]` mirrors a feature from the original C# agent
// or the Angular viewer services.

use std::sync::Arc;
use tauri::{Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

pub mod agent;

use agent::autostart::{
    disable_autostart, enable_autostart, is_autostart_enabled,
};
use agent::metrics::{AgentMetrics, MetricsCollector};
use agent::webrtc::IceServerConfig;
use agent::session::{
    get_file_list, join_session, leave_session, refresh_agent_role, send_chat_message,
    start_agent, stop_agent, AgentStatus, SharedState,
};
use agent::file_transfer::FileListResponse;
use agent::auth::PendingSession;
use agent::tray::{build_tray, show_and_focus};

// ─── App state ────────────────────────────────────────────────────────────────

/// Global shared state — stored in Tauri's managed state store.
pub struct AppState {
    pub agent: Arc<SharedState>,
    /// Reuse the same MetricsCollector across invocations so we don't spin up
    /// a fresh sysinfo `System` on every `get_metrics` call from the frontend
    /// (the UI typically polls every 1-5s, and `System` re-initialisation is
    /// non-trivial on Windows).
    pub metrics: Arc<MetricsCollector>,
}

// ─── Metrics ──────────────────────────────────────────────────────────────────

/// Returns a real-time system metrics snapshot.
/// Called from Svelte: `invoke("get_metrics")`
#[tauri::command]
fn get_metrics(state: State<'_, AppState>) -> AgentMetrics {
    state.metrics.collect()
}

// ─── Agent lifecycle ──────────────────────────────────────────────────────────

/// Starts the agent background loop: register → login → heartbeat/metrics/session-poll.
/// Called from Svelte: `invoke("start_agent", { serverUrl })`
#[tauri::command]
async fn start_agent_cmd(
    server_url: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    start_agent(Arc::clone(&state.agent), server_url).await
}

/// Stops the agent loop gracefully (marks OFFLINE on the server).
/// Called from Svelte: `invoke("stop_agent")`
#[tauri::command]
async fn stop_agent_cmd(state: State<'_, AppState>) -> Result<(), String> {
    stop_agent(Arc::clone(&state.agent)).await;
    Ok(())
}

/// Returns the current agent status (running, authenticated, in_session, …).
/// Called from Svelte: `invoke("get_agent_status")`
#[tauri::command]
async fn get_agent_status(state: State<'_, AppState>) -> Result<AgentStatus, String> {
    Ok(state.agent.status.lock().await.clone())
}

/// Returns the agent's JWT (or empty string if not logged in yet).
/// Svelte stores it in localStorage as `token` so `technicianApi` and
/// every REST call automatically attaches `Authorization: Bearer …`.
#[tauri::command]
async fn get_agent_token(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.agent.jwt_token.lock().await.clone().unwrap_or_default())
}

/// Re-call `/agents/login` and update the in-memory role.
/// Used by Svelte while in PENDING mode (polled every ~30s) to detect when
/// the technician finally assigns this machine to a user.
/// Called from Svelte: `invoke("refresh_agent_role")`
#[tauri::command]
async fn refresh_agent_role_cmd(state: State<'_, AppState>) -> Result<String, String> {
    refresh_agent_role(Arc::clone(&state.agent)).await
}

// ─── Session management ───────────────────────────────────────────────────────

/// Joins a remote session (typically called internally when a pending session
/// is detected, but also callable from the frontend for manual connect).
/// Called from Svelte: `invoke("join_session", { signalingToken, sessionId, allowRemoteInput, allowFileTransfer })`
#[tauri::command]
async fn join_session_cmd(
    signaling_token: String,
    session_id: i64,
    allow_remote_input: bool,
    allow_file_transfer: bool,
    server_url: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let pending = PendingSession {
        id: session_id,
        signaling_token,
        technician_username: "viewer".to_string(),
        allow_remote_input,
        allow_file_transfer,
    };

    let resolved_server_url = match server_url {
        Some(url) if !url.trim().is_empty() => url,
        _ => state.agent.status.lock().await.server_url.clone(),
    };

    if resolved_server_url.trim().is_empty() {
        return Err("server_url is required (provide it from frontend or start agent first)".to_string());
    }

    join_session(Arc::clone(&state.agent), &resolved_server_url, &pending).await
}

/// Leaves the current remote session.
/// Called from Svelte: `invoke("leave_session")`
#[tauri::command]
async fn leave_session_cmd(state: State<'_, AppState>) -> Result<(), String> {
    leave_session(Arc::clone(&state.agent)).await;
    Ok(())
}

// ─── Chat ─────────────────────────────────────────────────────────────────────

/// Sends a chat message to the connected viewer via the signaling channel.
/// Called from Svelte: `invoke("send_chat", { content, senderName })`
#[tauri::command]
async fn send_chat(
    content: String,
    sender_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    send_chat_message(Arc::clone(&state.agent), content, sender_name).await
}

// ─── File transfer ────────────────────────────────────────────────────────────

/// Returns the directory listing for a given path.
/// Called from Svelte: `invoke("get_file_list", { path })`
#[tauri::command]
fn get_file_list_cmd(path: String) -> FileListResponse {
    get_file_list(&path)
}

#[tauri::command]
async fn get_ice_servers_cmd() -> Result<Vec<IceServerConfig>, String> {
    Ok(agent::webrtc::resolve_ice_servers_for_frontend().await)
}

// ─── Utilities ────────────────────────────────────────────────────────────────

/// Legacy greet command (kept for Tauri template compatibility).
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

// ─── Autostart commands ───────────────────────────────────────────────────────

/// Returns `true` when HKCU\…\Run\LumiereAgent is set.
/// Called from Svelte: `invoke("get_autostart_status")`
#[tauri::command]
fn get_autostart_status() -> bool {
    is_autostart_enabled()
}

/// Enables / disables Windows autostart by writing the registry directly.
/// Called from Svelte: `invoke("set_autostart", { enabled: true })`
#[tauri::command]
fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<(), String> {
    if enabled {
        enable_autostart()?;
    } else {
        disable_autostart()?;
    }
    // Keep the tray checkbox in sync with the new registry state — the
    // user might toggle from the UI while the menu is closed.
    if let Some(handles) =
        app.try_state::<agent::tray::TrayHandles<tauri::Wry>>()
    {
        if let Ok(item) = handles.autostart_item.lock() {
            let _ = item.set_checked(enabled);
        }
    }
    Ok(())
}

/// Brings the main window to the foreground. Used by the frontend when
/// a pending session approval arrives while the window is hidden in the
/// tray — without this, the approval modal renders but stays invisible
/// and the technician can't connect.
///
/// Idempotent: if the window is already visible, just refocuses it.
#[tauri::command]
fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    show_and_focus(&window).map_err(|e| format!("show_and_focus failed: {e}"))?;
    // Set always-on-top briefly so the modal grabs attention even if
    // the user is on another desktop / fullscreen app — then we drop
    // it so we don't permanently steal focus from other windows.
    let _ = window.set_always_on_top(true);
    let win_clone = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let _ = win_clone.set_always_on_top(false);
    });
    Ok(())
}

/// Returns a short human-readable label for the current agent state.
/// Used by the AgentStatus.svelte card *and* by the tray "status" row.
#[tauri::command]
async fn get_agent_status_label(state: State<'_, AppState>) -> Result<String, String> {
    let s = state.agent.status.lock().await;
    let label = if s.in_session {
        match &s.technician {
            Some(name) if !name.is_empty() => format!("Session en cours avec {name}"),
            _ => "Session en cours".to_string(),
        }
    } else if s.authenticated {
        "Agent actif — En attente".to_string()
    } else if s.running {
        "Connexion au serveur…".to_string()
    } else {
        "Hors ligne".to_string()
    };
    Ok(label)
}

// ─── Tauri setup ──────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise le subscriber `tracing` une seule fois au demarrage.
    // Niveau par defaut : info. Override via RUST_LOG.
    // En production, ENABLED_INFO laisse passer assez de logs pour le debug
    // utilisateur sans noyer la console (les hot paths emettent en debug/trace).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init();

    let shared_state = SharedState::new();
    let metrics = Arc::new(MetricsCollector::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            agent: shared_state,
            metrics,
        })
        .invoke_handler(tauri::generate_handler![
            // metrics
            get_metrics,
            // agent lifecycle
            start_agent_cmd,
            stop_agent_cmd,
            get_agent_status,
            get_agent_status_label,
            refresh_agent_role_cmd,
            get_agent_token,
            // session
            join_session_cmd,
            leave_session_cmd,
            // chat
            send_chat,
            // file transfer
            get_file_list_cmd,
            // webrtc
            get_ice_servers_cmd,
            // autostart
            get_autostart_status,
            set_autostart,
            // window control
            show_main_window,
            // legacy
            greet,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // ── Inject AppHandle into SharedState ────────────────────
            // Lets the agent loop (session.rs) update the tray and
            // post Windows notifications without a circular dep
            // back to Tauri internals.
            {
                let state: tauri::State<'_, AppState> = app.state();
                let agent = Arc::clone(&state.agent);
                let handle_for_state = handle.clone();
                tauri::async_runtime::spawn(async move {
                    *agent.app_handle.lock().await = Some(handle_for_state);
                });
            }

            // ── Tray icon + menu ─────────────────────────────────────
            // Built before we touch the window so the icon is ready
            // by the time we hide/show.
            if let Err(err) = build_tray(&handle) {
                tracing::warn!("Tray init failed (continuing anyway): {err}");
            }

            // ── --minimized boot flag ────────────────────────────────
            // Set by the Run-key command line on Windows login. When
            // present we leave the window hidden — only the tray icon
            // is visible, matching AnyDesk / Chrome Remote Desktop.
            let args: Vec<String> = std::env::args().collect();
            let minimized = args
                .iter()
                .any(|a| a == agent::autostart::AUTOSTART_FLAG);

            if let Some(window) = app.get_webview_window("main") {
                if minimized {
                    let _ = window.hide();
                    tracing::info!(
                        "🕶  Agent demarre en arriere-plan (flag {} present)",
                        agent::autostart::AUTOSTART_FLAG
                    );
                } else {
                    let _ = show_and_focus(&window);

                    // First-launch nudge: if autostart is OFF, prompt
                    // the user to enable it. Cheap to do here — the
                    // notification plugin no-ops on first call if the
                    // OS hasn't granted permission.
                    if !is_autostart_enabled() {
                        let _ = app
                            .notification()
                            .builder()
                            .title("Lumiere Agent")
                            .body(
                                "Activer le demarrage automatique pour que l'agent \
                                 reste toujours actif au demarrage de Windows.",
                            )
                            .show();
                    }
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // ── Intercept the [X] button on the main window ──────────
            // We want AnyDesk-style behaviour: hiding to tray, not
            // tearing down the agent. The process keeps running, the
            // signaling loop stays connected, sessions still come in.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                    tracing::info!(
                        "🪟 Fenetre principale masquee dans le tray (close intercepted)"
                    );
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
