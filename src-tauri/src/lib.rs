// Lumière IT — Tauri backend entry point
//
// Exposes all Rust functions to the SvelteKit frontend via `invoke()`.
// Each `#[tauri::command]` mirrors a feature from the original C# agent
// or the Angular viewer services.

use std::sync::Arc;
use tauri::State;

pub mod agent;

use agent::metrics::{AgentMetrics, MetricsCollector};
use agent::session::{
    get_file_list, join_session, leave_session, send_chat_message, start_agent, stop_agent,
    AgentStatus, SharedState,
};
use agent::file_transfer::FileListResponse;
use agent::auth::PendingSession;

// ─── App state ────────────────────────────────────────────────────────────────

/// Global shared state — stored in Tauri's managed state store.
pub struct AppState {
    pub agent: Arc<SharedState>,
}

// ─── Metrics ──────────────────────────────────────────────────────────────────

/// Returns a real-time system metrics snapshot.
/// Called from Svelte: `invoke("get_metrics")`
#[tauri::command]
fn get_metrics() -> AgentMetrics {
    MetricsCollector::new().collect()
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

// ─── Utilities ────────────────────────────────────────────────────────────────

/// Legacy greet command (kept for Tauri template compatibility).
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

// ─── Tauri setup ──────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let shared_state = SharedState::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            agent: shared_state,
        })
        .invoke_handler(tauri::generate_handler![
            // metrics
            get_metrics,
            // agent lifecycle
            start_agent_cmd,
            stop_agent_cmd,
            get_agent_status,
            // session
            join_session_cmd,
            leave_session_cmd,
            // chat
            send_chat,
            // file transfer
            get_file_list_cmd,
            // legacy
            greet,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
