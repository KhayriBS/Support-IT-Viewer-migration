//! WebSocket signaling client.
//!
//! Port of `SignalingClient.cs` — replaces `ClientWebSocket` (.NET)
//! with `tokio-tungstenite` (async WebSocket for Rust/Tokio).
//!
//! Connection URL (same as C#):
//!   `ws://{server}/ws/signaling?token={token}&role=agent&sessionId={id}`

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

// ─── Signal types ─────────────────────────────────────────────────────────────
// Equivalent of `SignalType` enum in C#

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SignalType {
    Join,
    Offer,
    Answer,
    Ice,
    Leave,
    Chat,
    StreamStats,
    Error,
    FileListRequest,
    FileList,
    FileDownloadRequest,
    FileUploadRequest,
    FileData,
    FileComplete,
    FileError,
    #[serde(other)]
    Unknown,
}

// ─── SignalMessage ─────────────────────────────────────────────────────────────
// Equivalent of `SignalMessage` class in C#

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalMessage {
    #[serde(rename = "type")]
    pub signal_type: SignalType,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl SignalMessage {
    pub fn new(signal_type: SignalType, to: impl Into<String>, payload: Option<Value>) -> Self {
        Self {
            signal_type,
            from: "agent".to_string(),
            to: to.into(),
            session_id: None,
            payload,
        }
    }
}

// ─── SignalingClient ──────────────────────────────────────────────────────────

/// Outbound messages queued from outside the WS loop.
type WsSender = mpsc::UnboundedSender<Message>;

/// Inbound signal events dispatched to the session layer.
pub type SignalEvent = SignalMessage;

pub struct SignalingClient {
    server_url: String,
    session_id: Arc<Mutex<Option<String>>>,
    /// Channel to send WS frames from any task without holding the socket lock.
    tx: Arc<Mutex<Option<WsSender>>>,
}

impl SignalingClient {
    pub fn new(server_url: impl Into<String>) -> Self {
        // http → ws, https → wss  (same as C# constructor)
        let url = server_url
            .into()
            .trim_end_matches('/')
            .replace("https://", "wss://")
            .replace("http://", "ws://");

        Self {
            server_url: url,
            session_id: Arc::new(Mutex::new(None)),
            tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Sets the session id before connecting (mirrors `SetSessionId()`).
    pub async fn set_session_id(&self, id: impl Into<String>) {
        *self.session_id.lock().await = Some(id.into());
    }

    /// Returns `true` if the WebSocket send channel is open.
    pub async fn is_connected(&self) -> bool {
        self.tx.lock().await.is_some()
    }

    // ── ConnectAsync ──────────────────────────────────────────────────────────
    /// Opens the WebSocket and starts the receive loop.
    ///
    /// `event_tx` receives every inbound `SignalMessage` dispatched from
    /// `ProcessMessage()` (equivalent of the event handlers in C#).
    ///
    /// Returns immediately after the connection is established;
    /// the receive loop runs in a background `tokio::task`.
    pub async fn connect(
        &self,
        signaling_token: &str,
        event_tx: mpsc::UnboundedSender<SignalEvent>,
    ) -> Result<(), String> {
        let session_id = self
            .session_id
            .lock()
            .await
            .clone()
            .ok_or("SessionId must be set before connecting")?;

        let ws_url = format!(
            "{}/ws/signaling?token={}&role=agent&sessionId={}",
            self.server_url, signaling_token, session_id
        );

        println!("🔌 Connexion à {}…", ws_url);

        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket connect error: {e}"))?;

        println!("✅ Connecté au serveur signaling");

        let (mut sink, mut stream) = ws_stream.split();

        // ── Outbound channel (mirrors SendAsync) ──────────────────────────────
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
        *self.tx.lock().await = Some(out_tx);

        // Task: forward outbound messages to the WS sink
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Task: receive loop (mirrors ReceiveLoopAsync)
        let event_tx_clone = event_tx.clone();
        tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<SignalMessage>(&text) {
                            Ok(msg) => {
                                println!("📨 Reçu {:?} de {}", msg.signal_type, msg.from);
                                let _ = event_tx_clone.send(msg);
                            }
                            Err(e) => eprintln!("⚠️ Parse signal error: {e} — {text}"),
                        }
                    }
                    Ok(Message::Close(_)) => {
                        println!("🔌 Serveur a fermé la connexion");
                        break;
                    }
                    Err(e) => {
                        eprintln!("🔌 WebSocket error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
            println!("🔌 Receive loop terminée");
        });

        Ok(())
    }

    // ── SendAsync ─────────────────────────────────────────────────────────────
    /// Sends a `SignalMessage` to the server (mirrors `SendAsync()`).
    pub async fn send(&self, mut msg: SignalMessage) -> Result<(), String> {
        msg.from = "agent".to_string();
        msg.session_id = self.session_id.lock().await.clone();

        let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
        println!("📤 Envoi {:?} → {} (session: {:?})", msg.signal_type, msg.to, msg.session_id);

        let guard = self.tx.lock().await;
        if let Some(tx) = guard.as_ref() {
            tx.send(Message::Text(json.into()))
                .map_err(|_| "WS send channel closed".to_string())
        } else {
            Err("WebSocket non connecté".to_string())
        }
    }

    // ── Convenience senders (mirror named methods in C#) ─────────────────────

    pub async fn send_answer(&self, sdp_payload: Value) -> Result<(), String> {
        self.send(SignalMessage::new(SignalType::Answer, "viewer", Some(sdp_payload))).await
    }

    pub async fn send_ice_candidate(&self, ice_payload: Value) -> Result<(), String> {
        self.send(SignalMessage::new(SignalType::Ice, "viewer", Some(ice_payload))).await
    }

    pub async fn send_chat(&self, content: &str, sender_name: &str) -> Result<(), String> {
        let payload = serde_json::json!({
            "content": content,
            "senderName": sender_name,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        self.send(SignalMessage::new(SignalType::Chat, "viewer", Some(payload))).await
    }

    pub async fn send_stream_stats(
        &self,
        mbps: f64,
        fps: f64,
        bytes_per_second: i64,
    ) -> Result<(), String> {
        let payload = serde_json::json!({
            "mbps": mbps,
            "fps": fps,
            "bytesPerSecond": bytes_per_second,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        self.send(SignalMessage::new(SignalType::StreamStats, "viewer", Some(payload))).await
    }

    pub async fn send_screen_frame(&self, payload: Value) -> Result<(), String> {
        // Use FILE_DATA to ensure frames are routed to the viewer
        // even if the backend treats STREAM_STATS as a special-case payload.
        self.send(SignalMessage::new(SignalType::FileData, "viewer", Some(payload))).await
    }

    pub async fn send_file_list(&self, payload: Value) -> Result<(), String> {
        self.send(SignalMessage::new(SignalType::FileList, "viewer", Some(payload))).await
    }

    pub async fn send_file_data(&self, payload: Value) -> Result<(), String> {
        self.send(SignalMessage::new(SignalType::FileData, "viewer", Some(payload))).await
    }

    pub async fn send_file_complete(&self, file_name: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "fileName": file_name });
        self.send(SignalMessage::new(SignalType::FileComplete, "viewer", Some(payload))).await
    }

    pub async fn send_file_error(&self, error: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "error": error });
        self.send(SignalMessage::new(SignalType::FileError, "viewer", Some(payload))).await
    }

    // ── DisconnectAsync ───────────────────────────────────────────────────────
    /// Closes the outbound channel (the receive task will exit on its own).
    pub async fn disconnect(&self) {
        *self.tx.lock().await = None;
        println!("🔌 Client déconnecté");
    }
}
