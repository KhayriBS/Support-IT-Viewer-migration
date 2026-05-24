//! ICE servers configuration & resolution.
//!
//! Responsible for loading TURN/STUN credentials from:
//!  1. The `LUMIERE_ICE_SERVERS` env override (full JSON or CSV).
//!  2. The Metered TURN API (if `LUMIERE_METERED_DOMAIN` + `LUMIERE_METERED_API_KEY` set).
//!  3. Hardcoded fallback (best-effort, public openrelay).
//!
//! Returns either viewer-friendly `IceServerConfig` rows (serialized to the
//! Svelte front-end via the `get_ice_servers_cmd` Tauri command) or `RTCIceServer`
//! entries ready for the agent's own `RTCPeerConnection`.

use std::fs;
use std::time::Duration;

use webrtc::ice_transport::ice_credential_type::RTCIceCredentialType;
use webrtc::ice_transport::ice_server::RTCIceServer;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IceServerConfig {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
}

fn default_ice_servers() -> Vec<IceServerConfig> {
    // Metered "global.relay" static credentials (account: lumieretech).
    // Used as a hard fallback when neither LUMIERE_ICE_SERVERS nor the Metered
    // API at LUMIERE_METERED_DOMAIN are reachable. Rotated manually if Metered
    // invalidates the static creds.
    vec![
        IceServerConfig {
            urls: vec!["stun:stun.relay.metered.ca:80".to_owned()],
            username: None,
            credential: None,
        },
        IceServerConfig {
            urls: vec!["turn:global.relay.metered.ca:80".to_owned()],
            username: Some("d156f70e60e74c734ec39dc8".to_owned()),
            credential: Some("Z9zO5Kp3c5P/c6e0".to_owned()),
        },
        IceServerConfig {
            urls: vec!["turn:global.relay.metered.ca:80?transport=tcp".to_owned()],
            username: Some("d156f70e60e74c734ec39dc8".to_owned()),
            credential: Some("Z9zO5Kp3c5P/c6e0".to_owned()),
        },
        IceServerConfig {
            urls: vec!["turn:global.relay.metered.ca:443".to_owned()],
            username: Some("d156f70e60e74c734ec39dc8".to_owned()),
            credential: Some("Z9zO5Kp3c5P/c6e0".to_owned()),
        },
        IceServerConfig {
            urls: vec!["turns:global.relay.metered.ca:443?transport=tcp".to_owned()],
            username: Some("d156f70e60e74c734ec39dc8".to_owned()),
            credential: Some("Z9zO5Kp3c5P/c6e0".to_owned()),
        },
    ]
}

/// Read an environment variable, with a fallback to `.env.local` files reachable
/// from the current working directory or the running executable. Tauri's dev
/// binary lives in `src-tauri/target/{debug,release}/`, so the repo-root
/// `.env.local` is reachable via multiple relative depths.
///
/// Re-exported so other agent modules (e.g. tunables in [`super::webrtc`]) can
/// share the same env lookup behavior.
pub(super) fn read_env_or_local(key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let mut search_paths: Vec<std::path::PathBuf> = Vec::new();

    if let Ok(mut cwd) = std::env::current_dir() {
        for _ in 0..5 {
            search_paths.push(cwd.join(".env.local"));
            if !cwd.pop() {
                break;
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            let Some(d) = dir.clone() else { break; };
            search_paths.push(d.join(".env.local"));
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    for path in &search_paths {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((name, value)) = line.split_once('=') else {
                continue;
            };

            if name.trim() != key {
                continue;
            }

            let mut parsed = value.trim().to_string();
            if parsed.len() >= 2 {
                let quoted_with_single = parsed.starts_with('\'') && parsed.ends_with('\'');
                let quoted_with_double = parsed.starts_with('"') && parsed.ends_with('"');
                if quoted_with_single || quoted_with_double {
                    parsed = parsed[1..parsed.len() - 1].to_string();
                }
            }

            if !parsed.is_empty() {
                tracing::info!("📂 [env] {key} loaded from {}", path.display());
                return Some(parsed);
            }
        }
    }

    tracing::info!("⚠️ [env] {key} NOT found (searched {} paths)", search_paths.len());
    None
}

fn to_rtc_ice_servers(input: &[IceServerConfig]) -> Vec<RTCIceServer> {
    input
        .iter()
        .filter_map(|server| {
            // `webrtc-rs` may fail peer initialization with `turns:` URLs on some builds.
            // Keep STUN/TURN entries and drop only unsupported TURNS entries on the agent side
            // so SDP answer generation is never blocked.
            let urls: Vec<String> = server
                .urls
                .iter()
                .filter_map(|url| {
                    let trimmed = url.trim();
                    if trimmed.is_empty() || trimmed.starts_with("turns:") {
                        return None;
                    }
                    Some(trimmed.to_string())
                })
                .collect();

            if urls.is_empty() {
                return None;
            }

            let has_turn = urls.iter().any(|url| url.starts_with("turn:"));
            let username = server
                .username
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let credential = server
                .credential
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();

            if has_turn && (username.is_empty() || credential.is_empty()) {
                tracing::warn!(
                    "Skipping TURN ICE server without credentials: {:?}",
                    urls
                );
                return None;
            }

            let mut rtc_ice = RTCIceServer {
                urls: urls.clone(),
                username: username.clone(),
                credential: credential.clone(),
                ..Default::default()
            };

            if has_turn {
                rtc_ice.credential_type = RTCIceCredentialType::Password;
            }

            tracing::info!(
                "🧊 [ICE→RTC] Passing to webrtc-rs: urls={:?} user='{}' has_turn={} cred_type={:?}",
                urls, username, has_turn, rtc_ice.credential_type
            );

            Some(rtc_ice)
        })
        .collect()
}

fn parse_ice_servers_from_json(raw: &str) -> Option<Vec<IceServerConfig>> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let array = if let Some(items) = value.as_array() {
        items.clone()
    } else {
        value
            .get("iceServers")
            .and_then(serde_json::Value::as_array)
            .cloned()?
    };

    let mut servers = Vec::new();
    for item in array {
        let Some(obj) = item.as_object() else {
            continue;
        };

        let urls = if let Some(urls_array) = obj.get("urls").and_then(serde_json::Value::as_array) {
            urls_array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        } else if let Some(single) = obj.get("urls").and_then(serde_json::Value::as_str) {
            vec![single.to_string()]
        } else if let Some(single) = obj.get("url").and_then(serde_json::Value::as_str) {
            vec![single.to_string()]
        } else {
            Vec::new()
        };

        if urls.is_empty() {
            continue;
        }

        let username = obj
            .get("username")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string());
        let credential = obj
            .get("credential")
            .or_else(|| obj.get("password"))
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string());

        servers.push(IceServerConfig {
            urls,
            username,
            credential,
        });
    }

    if servers.is_empty() {
        None
    } else {
        Some(servers)
    }
}

fn load_ice_servers_from_env() -> Option<Vec<IceServerConfig>> {
    // Supports either:
    // - LUMIERE_ICE_SERVERS='[{"urls":["stun:..." ]},{"urls":["turn:..."],"username":"u","credential":"p"}]'
    // - LUMIERE_ICE_SERVERS='stun:stun.l.google.com:19302,turn:turn.example.com:3478'
    let raw = read_env_or_local("LUMIERE_ICE_SERVERS").map(|s| s.trim().to_string());
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return None;
    };

    if raw.starts_with('[') {
        return parse_ice_servers_from_json(&raw);
    }

    let urls: Vec<String> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if urls.is_empty() {
        return None;
    }

    Some(vec![IceServerConfig {
        urls,
        username: None,
        credential: None,
    }])
}

async fn load_ice_servers_from_metered() -> Option<Vec<IceServerConfig>> {
    let domain = match read_env_or_local("LUMIERE_METERED_DOMAIN") {
        Some(d) if !d.trim().is_empty() => d.trim().to_string(),
        _ => {
            tracing::info!("🧊 [ICE] LUMIERE_METERED_DOMAIN absent — skip Metered API");
            return None;
        }
    };
    let api_key = match read_env_or_local("LUMIERE_METERED_API_KEY") {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => {
            tracing::info!("🧊 [ICE] LUMIERE_METERED_API_KEY absent — skip Metered API");
            return None;
        }
    };

    let endpoint = format!(
        "https://{domain}/api/v1/turn/credentials?apiKey={api_key}",
        domain = domain,
        api_key = api_key
    );
    tracing::info!("🧊 [ICE] Fetching Metered TURN from {domain}...");

    let response = match reqwest::Client::new()
        .get(&endpoint)
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("❌ [ICE] Metered API request failed: {e}");
            return None;
        }
    };

    let status = response.status();
    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("❌ [ICE] Metered API body read failed: {e}");
            return None;
        }
    };

    if !status.is_success() {
        tracing::warn!("❌ [ICE] Metered API returned HTTP {status}: {body}");
        return None;
    }

    match parse_ice_servers_from_json(&body) {
        Some(servers) => {
            tracing::info!("✅ [ICE] Metered returned {} server(s)", servers.len());
            for s in &servers {
                tracing::info!(
                    "   • urls={:?} user={:?}",
                    s.urls,
                    s.username.as_deref().unwrap_or("")
                );
            }
            Some(servers)
        }
        None => {
            tracing::warn!("❌ [ICE] Metered API JSON parse failed. Body: {body}");
            None
        }
    }
}

/// Frontend-facing resolution: returns a list of `IceServerConfig` for the
/// Svelte UI to use in its own RTCPeerConnection (the technician side).
pub async fn resolve_ice_servers_for_frontend() -> Vec<IceServerConfig> {
    if let Some(env_servers) = load_ice_servers_from_env() {
        tracing::info!(
            "🧊 [ICE] Source: LUMIERE_ICE_SERVERS env override ({} servers)",
            env_servers.len()
        );
        return env_servers;
    }
    if let Some(metered_servers) = load_ice_servers_from_metered().await {
        tracing::info!(
            "🧊 [ICE] Source: Metered API ({} servers)",
            metered_servers.len()
        );
        return metered_servers;
    }
    let defaults = default_ice_servers();
    tracing::info!(
        "⚠️ [ICE] Source: built-in defaults ({} servers — likely openrelay, often broken)",
        defaults.len()
    );
    defaults
}

/// Agent-side resolution: returns `RTCIceServer` entries ready to be plugged
/// into the agent's own `RTCPeerConnection` configuration.
pub(super) async fn resolve_ice_servers_for_peer() -> Vec<RTCIceServer> {
    let servers = resolve_ice_servers_for_frontend().await;
    to_rtc_ice_servers(&servers)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── JSON parsing ─────────────────────────────────────────────────────────

    #[test]
    fn parse_json_accepts_array_of_servers() {
        let raw = r#"[
            {"urls": ["stun:stun.l.google.com:19302"]},
            {"urls": ["turn:turn.example.com:3478"], "username": "u", "credential": "p"}
        ]"#;
        let servers = parse_ice_servers_from_json(raw).expect("should parse");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].urls, vec!["stun:stun.l.google.com:19302"]);
        assert_eq!(servers[1].username.as_deref(), Some("u"));
        assert_eq!(servers[1].credential.as_deref(), Some("p"));
    }

    #[test]
    fn parse_json_accepts_iceservers_wrapper_object() {
        let raw = r#"{"iceServers": [{"urls": ["stun:foo:3478"]}]}"#;
        let servers = parse_ice_servers_from_json(raw).expect("should parse");
        assert_eq!(servers.len(), 1);
    }

    #[test]
    fn parse_json_accepts_single_url_string() {
        let raw = r#"[{"urls": "stun:single.example.com:3478"}]"#;
        let servers = parse_ice_servers_from_json(raw).expect("should parse");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:single.example.com:3478"]);
    }

    #[test]
    fn parse_json_accepts_legacy_url_key() {
        // Some configs use "url" (singular) instead of "urls".
        let raw = r#"[{"url": "stun:legacy.example.com"}]"#;
        let servers = parse_ice_servers_from_json(raw).expect("should parse");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:legacy.example.com"]);
    }

    #[test]
    fn parse_json_accepts_password_alias_for_credential() {
        let raw = r#"[{"urls": ["turn:x"], "username": "u", "password": "pw"}]"#;
        let servers = parse_ice_servers_from_json(raw).expect("should parse");
        assert_eq!(servers[0].credential.as_deref(), Some("pw"));
    }

    #[test]
    fn parse_json_returns_none_when_array_empty() {
        assert!(parse_ice_servers_from_json("[]").is_none());
    }

    #[test]
    fn parse_json_returns_none_on_invalid_json() {
        assert!(parse_ice_servers_from_json("not json").is_none());
    }

    #[test]
    fn parse_json_skips_entries_without_urls() {
        let raw = r#"[{"username": "u"}, {"urls": ["stun:ok"]}]"#;
        let servers = parse_ice_servers_from_json(raw).expect("should parse");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:ok"]);
    }

    // ── webrtc-rs conversion ─────────────────────────────────────────────────

    #[test]
    fn to_rtc_drops_turns_urls() {
        // webrtc-rs 0.11 has known issues with TURNS URLs → dropped.
        let input = vec![IceServerConfig {
            urls: vec!["turns:example.com:443".to_owned()],
            username: Some("u".into()),
            credential: Some("p".into()),
        }];
        let out = to_rtc_ice_servers(&input);
        assert!(out.is_empty(), "TURNS should be filtered out");
    }

    #[test]
    fn to_rtc_keeps_stun_and_turn() {
        let input = vec![IceServerConfig {
            urls: vec![
                "stun:stun.example.com:3478".to_owned(),
                "turn:turn.example.com:3478".to_owned(),
                "turns:turns.example.com:5349".to_owned(),
            ],
            username: Some("u".into()),
            credential: Some("p".into()),
        }];
        let out = to_rtc_ice_servers(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].urls.len(), 2);
        assert!(out[0].urls.iter().any(|u| u.starts_with("stun:")));
        assert!(out[0].urls.iter().any(|u| u.starts_with("turn:")));
        assert!(!out[0].urls.iter().any(|u| u.starts_with("turns:")));
    }

    #[test]
    fn to_rtc_drops_turn_entries_without_credentials() {
        // TURN without username/credential is invalid — must be skipped, not crash.
        let input = vec![IceServerConfig {
            urls: vec!["turn:turn.example.com:3478".to_owned()],
            username: None,
            credential: None,
        }];
        let out = to_rtc_ice_servers(&input);
        assert!(out.is_empty());
    }

    #[test]
    fn to_rtc_keeps_stun_without_credentials() {
        // STUN doesn't need credentials and must always pass through.
        let input = vec![IceServerConfig {
            urls: vec!["stun:stun.example.com:3478".to_owned()],
            username: None,
            credential: None,
        }];
        let out = to_rtc_ice_servers(&input);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn to_rtc_trims_whitespace_from_urls() {
        let input = vec![IceServerConfig {
            urls: vec!["  stun:trimmed.example.com  ".to_owned(), "".to_owned()],
            username: None,
            credential: None,
        }];
        let out = to_rtc_ice_servers(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].urls, vec!["stun:trimmed.example.com"]);
    }

    #[test]
    fn default_servers_include_metered_fallback() {
        let defaults = default_ice_servers();
        assert!(defaults.iter().any(|s| s.urls.iter().any(|u| u.contains("metered.ca"))));
    }
}
