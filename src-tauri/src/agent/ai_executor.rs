//! Agent IA cote machine controlee.
//!
//! Recoit des messages JSON `{ "type": "AI_ACTION", "action": {...} }` sur le
//! DataChannel `input`. Decompose la liste d'actions, les execute en serie
//! via :
//!   * `enigo`              pour souris/clavier (Click, TypeText, Key, Move…)
//!   * `tokio::process`     pour `Shell`
//!   * `screen_capture`     pour renvoyer une capture apres `Screenshot`
//!
//! Les coordonnees x/y sont NORMALISEES dans [0, 1] par Gemini. On les
//! denormalise contre la resolution reelle de l'ecran principal AVANT
//! d'appeler enigo.
//!
//! Chaque etape envoie un compte-rendu textuel JSON sur le meme DataChannel
//! sous la forme `{ "type": "AI_ACTION_RESULT", "ok": bool, "message": str,
//! "screenshot"?: str }`. Le viewer affiche ces messages dans le chat.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use webrtc::data_channel::RTCDataChannel;

use super::screen_capture::capture_primary_jpeg_base64;

// ── DTO ──────────────────────────────────────────────────────────────────────

/// Une action telle qu'envoyee par le backend (apres parsing Gemini).
/// Schema "plat" identique au DTO Java `AiAction` — tous les champs sont
/// optionnels et leur presence depend de `r#type`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AiAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub button: Option<String>,
    pub text: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<String>,
    pub cmd: Option<String>,
    pub shell: Option<String>,
    pub ms: Option<u64>,
    // ── scroll ──────────────────────────────────────────────────────────
    pub dy: Option<i32>,
    pub dx: Option<i32>,
    // ── drag (x/y = depart, destX/destY = arrivee) ──────────────────────
    #[serde(rename = "destX")]
    pub dest_x: Option<f64>,
    #[serde(rename = "destY")]
    pub dest_y: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ActionResult<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    ok: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<&'a str>,
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Decompose un message AI_ACTION ou AI_PLAN du DataChannel et l'execute.
///
/// `raw_json` doit contenir soit :
///   * `{ "type": "AI_ACTION", "action": <AiAction> }`  → 1 action
///   * `{ "type": "AI_PLAN",   "actions": [...] }`      → plusieurs actions
///
/// La sortie va sur `channel` (le DataChannel `input` regagne du peer viewer).
/// La fonction ne panique jamais — chaque erreur est convertie en un
/// AI_ACTION_RESULT `ok=false` que le viewer affiche.
pub async fn dispatch(channel: Arc<RTCDataChannel>, raw_json: &str) {
    let executor = AiExecutor::new(channel);

    let value: Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(e) => {
            executor
                .reply_error(None, &format!("Invalid AI JSON: {e}"))
                .await;
            return;
        }
    };

    let msg_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let actions: Vec<AiAction> = match msg_type {
        "AI_ACTION" => match value.get("action") {
            Some(a) => match serde_json::from_value::<AiAction>(a.clone()) {
                Ok(parsed) => vec![parsed],
                Err(e) => {
                    executor
                        .reply_error(None, &format!("Invalid AI action shape: {e}"))
                        .await;
                    return;
                }
            },
            None => {
                executor.reply_error(None, "AI_ACTION missing 'action' field").await;
                return;
            }
        },
        "AI_PLAN" => match value.get("actions") {
            Some(a) => match serde_json::from_value::<Vec<AiAction>>(a.clone()) {
                Ok(parsed) => parsed,
                Err(e) => {
                    executor
                        .reply_error(None, &format!("Invalid AI plan shape: {e}"))
                        .await;
                    return;
                }
            },
            None => {
                executor.reply_error(None, "AI_PLAN missing 'actions' field").await;
                return;
            }
        },
        other => {
            executor
                .reply_error(None, &format!("Unknown AI message type: {other}"))
                .await;
            return;
        }
    };

    if actions.is_empty() {
        executor.reply_error(None, "Empty action list").await;
        return;
    }

    println!("🤖 AI executing {} action(s)", actions.len());
    for action in actions {
        executor.execute(action).await;
    }
}

// ── Implementation ───────────────────────────────────────────────────────────

struct AiExecutor {
    channel: Arc<RTCDataChannel>,
    /// `enigo` n'est pas Send/Sync sur toutes les plateformes — on le serialise
    /// derriere un Mutex. De toute facon les actions sont sequentielles.
    #[cfg(windows)]
    enigo: Mutex<enigo::Enigo>,
    #[cfg(not(windows))]
    enigo: Mutex<()>,
}

impl AiExecutor {
    fn new(channel: Arc<RTCDataChannel>) -> Self {
        #[cfg(windows)]
        {
            let settings = enigo::Settings::default();
            let enigo = enigo::Enigo::new(&settings)
                .expect("Enigo init failed (vibration with input subsystem?)");
            Self {
                channel,
                enigo: Mutex::new(enigo),
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                channel,
                enigo: Mutex::new(()),
            }
        }
    }

    async fn execute(&self, action: AiAction) {
        let kind = action.action_type.clone();
        println!("🤖 AI action: {kind}");

        let result: Result<Option<String>, String> = match kind.as_str() {
            "click" | "double_click" | "move" => self.do_pointer(&action).await,
            "type_text" => self.do_type_text(&action).await,
            "key" => self.do_key(&action).await,
            "shell" => self.do_shell(&action).await,
            "wait" => self.do_wait(&action).await,
            "screenshot" => self.do_screenshot().await,
            "scroll" => self.do_scroll(&action).await,
            "drag" => self.do_drag(&action).await,
            other => Err(format!("Unsupported action type: {other}")),
        };

        match result {
            Ok(screenshot) => self.reply_ok(Some(&kind), screenshot).await,
            Err(e) => self.reply_error(Some(&kind), &e).await,
        }
    }

    // ── Souris ───────────────────────────────────────────────────────────────

    #[cfg(windows)]
    async fn do_pointer(&self, action: &AiAction) -> Result<Option<String>, String> {
        use enigo::{Button, Coordinate, Direction, Mouse};

        let x_norm = action.x.ok_or("missing x")?;
        let y_norm = action.y.ok_or("missing y")?;
        let (screen_w, screen_h) = primary_screen_size()?;
        let x = (x_norm.clamp(0.0, 1.0) * screen_w as f64).round() as i32;
        let y = (y_norm.clamp(0.0, 1.0) * screen_h as f64).round() as i32;

        let button = match action.button.as_deref().unwrap_or("left") {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        };

        let mut enigo = self.enigo.lock().await;
        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| format!("move_mouse failed: {e}"))?;

        match action.action_type.as_str() {
            "click" => enigo
                .button(button, Direction::Click)
                .map_err(|e| format!("click failed: {e}"))?,
            "double_click" => {
                enigo
                    .button(button, Direction::Click)
                    .map_err(|e| format!("click 1 failed: {e}"))?;
                tokio::time::sleep(Duration::from_millis(60)).await;
                enigo
                    .button(button, Direction::Click)
                    .map_err(|e| format!("click 2 failed: {e}"))?;
            }
            "move" => {} // already moved
            _ => unreachable!(),
        }
        Ok(None)
    }

    #[cfg(not(windows))]
    async fn do_pointer(&self, _action: &AiAction) -> Result<Option<String>, String> {
        Err("Pointer actions are Windows-only in this build".into())
    }

    // ── Clavier : texte ──────────────────────────────────────────────────────

    #[cfg(windows)]
    async fn do_type_text(&self, action: &AiAction) -> Result<Option<String>, String> {
        use enigo::Keyboard;
        let text = action.text.as_deref().ok_or("missing text")?;
        if text.is_empty() {
            return Ok(None);
        }
        let mut enigo = self.enigo.lock().await;
        enigo
            .text(text)
            .map_err(|e| format!("type_text failed: {e}"))?;
        Ok(None)
    }

    #[cfg(not(windows))]
    async fn do_type_text(&self, _action: &AiAction) -> Result<Option<String>, String> {
        Err("type_text is Windows-only in this build".into())
    }

    // ── Clavier : touches speciales / raccourcis ─────────────────────────────

    #[cfg(windows)]
    async fn do_key(&self, action: &AiAction) -> Result<Option<String>, String> {
        use enigo::{Direction, Key, Keyboard};

        let key_name = action.key.as_deref().ok_or("missing key")?;
        let key = map_key_name(key_name)
            .ok_or_else(|| format!("Unknown key name: {key_name}"))?;

        // Resolve modifiers
        let mut mod_keys: Vec<Key> = Vec::new();
        for m in &action.modifiers {
            let mk = match m.to_lowercase().as_str() {
                "ctrl" | "control" => Key::Control,
                "alt" => Key::Alt,
                "shift" => Key::Shift,
                "meta" | "win" | "super" => Key::Meta,
                other => return Err(format!("Unknown modifier: {other}")),
            };
            mod_keys.push(mk);
        }

        let mut enigo = self.enigo.lock().await;

        // Press modifiers DOWN, send key click, release modifiers UP
        for mk in &mod_keys {
            enigo
                .key(*mk, Direction::Press)
                .map_err(|e| format!("mod press failed: {e}"))?;
        }
        let click_result = enigo.key(key, Direction::Click);
        for mk in mod_keys.iter().rev() {
            // best-effort release even if click failed
            let _ = enigo.key(*mk, Direction::Release);
        }
        click_result.map_err(|e| format!("key click failed: {e}"))?;
        Ok(None)
    }

    #[cfg(not(windows))]
    async fn do_key(&self, _action: &AiAction) -> Result<Option<String>, String> {
        Err("key actions are Windows-only in this build".into())
    }

    // ── Shell ────────────────────────────────────────────────────────────────

    async fn do_shell(&self, action: &AiAction) -> Result<Option<String>, String> {
        let cmd = action.cmd.as_deref().ok_or("missing cmd")?;
        let shell = action
            .shell
            .as_deref()
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "powershell".to_string()
                } else {
                    "bash".to_string()
                }
            });

        let (program, args): (&str, Vec<String>) = match shell.as_str() {
            "cmd" => ("cmd", vec!["/C".into(), cmd.into()]),
            "powershell" => (
                "powershell",
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    cmd.into(),
                ],
            ),
            "bash" => ("bash", vec!["-c".into(), cmd.into()]),
            other => return Err(format!("Unknown shell: {other}")),
        };

        // Timeout 30s — protege contre les commandes interactives qui pendraient.
        let exec = TokioCommand::new(program)
            .args(&args)
            .output();

        let output = tokio::time::timeout(Duration::from_secs(30), exec)
            .await
            .map_err(|_| "Shell command timed out after 30s".to_string())?
            .map_err(|e| format!("spawn failed: {e}"))?;

        // Resume stdout + stderr en un message lisible (tronque a 1500 chars
        // pour ne pas saturer le DataChannel).
        let stdout = truncate(&String::from_utf8_lossy(&output.stdout), 800);
        let stderr = truncate(&String::from_utf8_lossy(&output.stderr), 500);
        let mut msg = format!("exit={}", output.status.code().unwrap_or(-1));
        if !stdout.is_empty() {
            msg.push_str("\nstdout: ");
            msg.push_str(&stdout);
        }
        if !stderr.is_empty() {
            msg.push_str("\nstderr: ");
            msg.push_str(&stderr);
        }

        if output.status.success() {
            Ok(Some(msg))
        } else {
            Err(msg)
        }
    }

    // ── Wait / Screenshot ────────────────────────────────────────────────────

    async fn do_wait(&self, action: &AiAction) -> Result<Option<String>, String> {
        let ms = action.ms.unwrap_or(500).clamp(50, 10_000);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(Some(format!("waited {ms}ms")))
    }

    async fn do_screenshot(&self) -> Result<Option<String>, String> {
        // q=60 → ~ 100-300 KB de base64. Suffisant pour la verif Gemini sans
        // saturer le DataChannel SCTP.
        let b64 = capture_primary_jpeg_base64(60)?;
        Ok(Some(b64))
    }

    // ── Scroll ───────────────────────────────────────────────────────────────

    #[cfg(windows)]
    async fn do_scroll(&self, action: &AiAction) -> Result<Option<String>, String> {
        use enigo::{Axis, Coordinate, Mouse};

        let dy = action.dy.unwrap_or(0);
        let dx = action.dx.unwrap_or(0);
        if dy == 0 && dx == 0 {
            return Err("scroll missing dy/dx".into());
        }

        let mut enigo = self.enigo.lock().await;

        // Position le curseur si x/y fournis (sinon scroll a la position courante)
        if let (Some(nx), Some(ny)) = (action.x, action.y) {
            let (screen_w, screen_h) = primary_screen_size()?;
            let px = (nx.clamp(0.0, 1.0) * screen_w as f64).round() as i32;
            let py = (ny.clamp(0.0, 1.0) * screen_h as f64).round() as i32;
            enigo
                .move_mouse(px, py, Coordinate::Abs)
                .map_err(|e| format!("scroll: move_mouse failed: {e}"))?;
        }

        if dy != 0 {
            enigo
                .scroll(dy, Axis::Vertical)
                .map_err(|e| format!("scroll vertical failed: {e}"))?;
        }
        if dx != 0 {
            enigo
                .scroll(dx, Axis::Horizontal)
                .map_err(|e| format!("scroll horizontal failed: {e}"))?;
        }
        Ok(Some(format!("scrolled dy={dy} dx={dx}")))
    }

    #[cfg(not(windows))]
    async fn do_scroll(&self, _action: &AiAction) -> Result<Option<String>, String> {
        Err("scroll is Windows-only in this build".into())
    }

    // ── Drag ─────────────────────────────────────────────────────────────────

    #[cfg(windows)]
    async fn do_drag(&self, action: &AiAction) -> Result<Option<String>, String> {
        use enigo::{Button, Coordinate, Direction, Mouse};

        let from_x = action.x.ok_or("drag missing x (origin)")?;
        let from_y = action.y.ok_or("drag missing y (origin)")?;
        let to_x = action.dest_x.ok_or("drag missing destX")?;
        let to_y = action.dest_y.ok_or("drag missing destY")?;
        let (screen_w, screen_h) = primary_screen_size()?;

        let sx = (from_x.clamp(0.0, 1.0) * screen_w as f64).round() as i32;
        let sy = (from_y.clamp(0.0, 1.0) * screen_h as f64).round() as i32;
        let ex = (to_x.clamp(0.0, 1.0) * screen_w as f64).round() as i32;
        let ey = (to_y.clamp(0.0, 1.0) * screen_h as f64).round() as i32;

        let button = match action.button.as_deref().unwrap_or("left") {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        };

        let mut enigo = self.enigo.lock().await;

        // 1. Position curseur au point de depart
        enigo
            .move_mouse(sx, sy, Coordinate::Abs)
            .map_err(|e| format!("drag: move_mouse to origin failed: {e}"))?;

        // 2. Bouton enfonce
        enigo
            .button(button, Direction::Press)
            .map_err(|e| format!("drag: button press failed: {e}"))?;

        // 3. Drag avec etapes intermediaires (simule un drag naturel) — 8 steps,
        //    1ms entre chaque → assez rapide pour ne pas bloquer, assez progressif
        //    pour que les controles type slider/scrollbar reagissent correctement
        //    (certains UI ignorent les saut "teleport" et n'agissent que sur les
        //    mouvements continus).
        let steps = 8;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let ix = (sx as f64 + (ex - sx) as f64 * t).round() as i32;
            let iy = (sy as f64 + (ey - sy) as f64 * t).round() as i32;
            enigo
                .move_mouse(ix, iy, Coordinate::Abs)
                .map_err(|e| format!("drag: intermediate move failed: {e}"))?;
            tokio::time::sleep(Duration::from_millis(8)).await;
        }

        // 4. Relacher le bouton
        enigo
            .button(button, Direction::Release)
            .map_err(|e| format!("drag: button release failed: {e}"))?;

        Ok(Some(format!("dragged ({sx},{sy}) → ({ex},{ey})")))
    }

    #[cfg(not(windows))]
    async fn do_drag(&self, _action: &AiAction) -> Result<Option<String>, String> {
        Err("drag is Windows-only in this build".into())
    }

    // ── Replies ──────────────────────────────────────────────────────────────

    async fn reply_ok(&self, action: Option<&str>, screenshot_or_msg: Option<String>) {
        // Pour `screenshot` on met la chaine base64 dans `screenshot`. Pour
        // les autres actions, c'est un message texte (ou rien).
        let is_screenshot = matches!(action, Some("screenshot"));
        let (screenshot, message) = if is_screenshot {
            (screenshot_or_msg, "screenshot captured".to_string())
        } else {
            (None, screenshot_or_msg.unwrap_or_default())
        };

        let payload = ActionResult {
            msg_type: "AI_ACTION_RESULT",
            ok: true,
            message,
            screenshot,
            action,
        };
        let json = match serde_json::to_string(&payload) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[ai] failed to serialize AI_ACTION_RESULT: {e}");
                return;
            }
        };
        if let Err(e) = self.channel.send_text(json).await {
            eprintln!("[ai] failed to send AI_ACTION_RESULT: {e}");
        }
    }

    async fn reply_error(&self, action: Option<&str>, message: &str) {
        let payload = ActionResult {
            msg_type: "AI_ACTION_RESULT",
            ok: false,
            message: message.to_string(),
            screenshot: None,
            action,
        };
        let json = match serde_json::to_string(&payload) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[ai] failed to serialize error AI_ACTION_RESULT: {e}");
                return;
            }
        };
        if let Err(e) = self.channel.send_text(json).await {
            eprintln!("[ai] failed to send error AI_ACTION_RESULT: {e}");
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn primary_screen_size() -> Result<(i32, i32), String> {
    use screenshots::Screen;
    let screens = Screen::all().map_err(|e| format!("screen enumerate: {e}"))?;
    let s = screens
        .iter()
        .find(|s| s.display_info.is_primary)
        .or_else(|| screens.first())
        .ok_or("no primary screen")?;
    Ok((s.display_info.width as i32, s.display_info.height as i32))
}

#[cfg(not(windows))]
fn primary_screen_size() -> Result<(i32, i32), String> {
    Err("primary_screen_size unavailable on this platform".into())
}

#[cfg(windows)]
fn map_key_name(key: &str) -> Option<enigo::Key> {
    use enigo::Key;
    // Caracteres ASCII simples → Key::Unicode
    if key.chars().count() == 1 {
        if let Some(ch) = key.chars().next() {
            return Some(Key::Unicode(ch));
        }
    }
    Some(match key.to_lowercase().as_str() {
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "arrowleft" | "left" => Key::LeftArrow,
        "arrowright" | "right" => Key::RightArrow,
        "arrowup" | "up" => Key::UpArrow,
        "arrowdown" | "down" => Key::DownArrow,
        "space" => Key::Space,
        "shift" => Key::Shift,
        "control" | "ctrl" => Key::Control,
        "alt" => Key::Alt,
        "meta" | "win" | "super" => Key::Meta,
        "capslock" => Key::CapsLock,
        "f1" => Key::F1, "f2" => Key::F2, "f3" => Key::F3, "f4" => Key::F4,
        "f5" => Key::F5, "f6" => Key::F6, "f7" => Key::F7, "f8" => Key::F8,
        "f9" => Key::F9, "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        // ── Touches média Windows ─────────────────────────────────────────
        // Bien meilleures que cliquer le slider à l'œil — Gemini est
        // explicitement instruit de les preferer (cf prompt Spring).
        "volumeup"   | "volume_up"   | "volume-up"   => Key::VolumeUp,
        "volumedown" | "volume_down" | "volume-down" => Key::VolumeDown,
        "volumemute" | "volume_mute" | "volume-mute" | "mute" => Key::VolumeMute,
        "medianext"     | "media_next"      | "media-next"      => Key::MediaNextTrack,
        "mediaprev"     | "media_prev"      | "media-prev"
            | "mediaprevious"| "media_previous"  | "media-previous"        => Key::MediaPrevTrack,
        "mediaplay"     | "media_play"      | "media-play"
            | "playpause"   | "play_pause"      | "play-pause"
            | "mediaplaypause" | "media_play_pause" | "media-play-pause"  => Key::MediaPlayPause,
        "mediastop"     | "media_stop"      | "media-stop"      => Key::MediaStop,
        _ => return None,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.trim().to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out.trim().to_string()
    }
}
