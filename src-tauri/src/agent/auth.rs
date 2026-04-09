//! HTTP client for the Lumière signaling server.
//!
//! Port of `AgentAuthService.cs` — replaces `HttpClient` + `FormUrlEncodedContent`
//! with `reqwest` (async, TLS via rustls).

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};

use super::metrics::AgentMetrics;

// ─── Data models ──────────────────────────────────────────────────────────────

/// Equivalent of the C# `Agent` class returned by `/agents/register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub machine_id: String,
    pub hostname: String,
    pub os: String,
    pub status: String,
}

/// Pending session returned by `GET /sessions/pending/{machineId}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSession {
    pub id: i64,
    pub signaling_token: String,
    pub technician_username: String,
    pub allow_remote_input: bool,
    pub allow_file_transfer: bool,
}

// ─── AgentAuthService ─────────────────────────────────────────────────────────

/// Mirrors `AgentAuthService.cs`.
///
/// All methods are async and return `Result<_, String>` (the error is the
/// HTTP status + body, like `EnsureOk()` in C#).
pub struct AgentAuthService {
    client: Client,
    server_url: String,
}

impl AgentAuthService {
    /// Creates a new service. `server_url` example: `"http://192.168.218.49:8080"`.
    pub fn new(server_url: impl Into<String>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to build reqwest client");

        Self {
            client,
            server_url: server_url.into().trim_end_matches('/').to_string(),
        }
    }

    // ── POST /agents/register ─────────────────────────────────────────────────
    /// Equivalent: `RegisterOrUpdateAgentAsync(machineId, hostname, os)`
    pub async fn register_or_update(
        &self,
        machine_id: &str,
        hostname: &str,
        os: &str,
    ) -> Result<Agent, String> {
        let url = format!("{}/agents/register", self.server_url);
        let params = [
            ("machineId", machine_id),
            ("hostname", hostname),
            ("os", os),
        ];

        let resp = self
            .client
            .post(&url)
            .form(&params)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        ensure_ok(&resp, &url)?;
        resp.json::<Agent>().await.map_err(|e| e.to_string())
    }

    // ── POST /agents/login ────────────────────────────────────────────────────
    /// Equivalent: `LoginAgentAsync(machineId, os)` → JWT token string.
    pub async fn login(&self, machine_id: &str, os: &str) -> Result<String, String> {
        let url = format!("{}/agents/login", self.server_url);
        let params = [("machineId", machine_id), ("os", os)];

        let resp = self
            .client
            .post(&url)
            .form(&params)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        ensure_ok(&resp, &url)?;

        let raw = resp.text().await.map_err(|e| e.to_string())?;
        // Strip surrounding quotes if server returns a JSON string `"token"`
        let token = raw.trim().trim_matches('"').to_string();

        if token.is_empty() {
            return Err("Agent login failed: empty token".into());
        }
        Ok(token)
    }

    // ── POST /agents/heartbeat ────────────────────────────────────────────────
    /// Equivalent: `SendHeartbeatAsync(machineId)`.
    pub async fn send_heartbeat(&self, machine_id: &str, token: &str) -> Result<(), String> {
        let url = format!("{}/agents/heartbeat", self.server_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .form(&[("machineId", machine_id)])
            .send()
            .await
            .map_err(|e| e.to_string())?;

        ensure_ok(&resp, &url)
    }

    // ── POST /agents/offline ──────────────────────────────────────────────────
    /// Equivalent: `MarkAgentOfflineAsync(machineId)`.
    pub async fn mark_offline(&self, machine_id: &str, token: &str) -> Result<(), String> {
        let url = format!("{}/agents/offline", self.server_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .form(&[("machineId", machine_id)])
            .send()
            .await
            .map_err(|e| e.to_string())?;

        ensure_ok(&resp, &url)
    }

    // ── POST /agents/metrics ──────────────────────────────────────────────────
    /// Equivalent: `SendMetricsAsync(metrics)`.
    pub async fn send_metrics(&self, metrics: &AgentMetrics, token: &str) -> Result<(), String> {
        let url = format!("{}/agents/metrics", self.server_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(metrics)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        ensure_ok(&resp, &url)
    }

    // ── GET /sessions/pending/{machineId} ─────────────────────────────────────
    /// Equivalent: `GetPendingSessionAsync(machineId)`.
    pub async fn get_pending_session(
        &self,
        machine_id: &str,
        token: &str,
    ) -> Result<Option<PendingSession>, String> {
        let url = format!("{}/sessions/pending/{}", self.server_url, machine_id);

        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            StatusCode::NOT_FOUND | StatusCode::NO_CONTENT => return Ok(None),
            s if !s.is_success() => {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("HTTP {} on {}: {}", s, url, body));
            }
            _ => {}
        }

        let text = resp.text().await.map_err(|e| e.to_string())?;
        if text.trim().is_empty() || text.trim() == "null" {
            return Ok(None);
        }

        serde_json::from_str::<PendingSession>(&text)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    // ── POST /sessions/stop/{sessionId} ──────────────────────────────────────
    /// Equivalent: `StopSessionAsync(sessionId)`.
    pub async fn stop_session(&self, session_id: i64, token: &str) -> Result<(), String> {
        let url = format!("{}/sessions/stop/{}", self.server_url, session_id);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        ensure_ok(&resp, &url)
    }
}

// ─── Helper ───────────────────────────────────────────────────────────────────

/// Equivalent of `EnsureOk()` in C#.
fn ensure_ok(resp: &reqwest::Response, url: &str) -> Result<(), String> {
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {} on {}", resp.status(), url))
    }
}
