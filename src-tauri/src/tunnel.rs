use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

// ── State ─────────────────────────────────────────────────────────────────────

pub struct TunnelManager {
    child: Option<tauri_plugin_shell::process::CommandChild>,
    pub url: Option<String>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self { child: None, url: None }
    }
}

pub struct TunnelState(pub Mutex<TunnelManager>);

// ── Public API ────────────────────────────────────────────────────────────────

/// Start the Cloudflare tunnel. Called once on app launch.
/// Emits `tunnel-status-changed { status: "Connecting" }` immediately,
/// then `{ url, status: "Active" }` when URL is found,
/// or `{ status: "Unavailable" }` if spawn fails.
pub fn tunnel_start(app: &AppHandle, port: u16) {
    // Emit Connecting immediately so navbar shows spinner
    let _ = app.emit("tunnel-status-changed", json!({ "url": null, "status": "Connecting" }));

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        spawn_cloudflared(app_clone, port).await;
    });
}

// ── Internal ──────────────────────────────────────────────────────────────────

async fn spawn_cloudflared(app: AppHandle, port: u16) {
    let url_arg = format!("http://127.0.0.1:{port}");

    let spawn_result = app
        .shell()
        .sidecar("cloudflared")
        .map(|cmd| cmd.args(["tunnel", "--no-autoupdate", "--url", &url_arg]))
        .and_then(|cmd| cmd.spawn());

    let (mut rx, child) = match spawn_result {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!("cloudflared: spawn failed: {e}");
            let local_enabled = if let Some(h) = app.try_state::<crate::incoming_auth::IncomingAuthStateHandle>() {
                h.0.set_tunnel_active(false);
                h.0.local_enabled()
            } else {
                false
            };
            let _ = app.emit("tunnel-status-changed", json!({ "url": null, "status": "Unavailable" }));
            let _ = app.emit("auth-status-changed", json!({
                "tunnel_forced": false,
                "local_enabled": local_enabled,
                "effective":     local_enabled,
            }));
            return;
        }
    };

    // Store child handle
    if let Some(state) = app.try_state::<TunnelState>() {
        state.0.lock().unwrap().child = Some(child);
    }

    // Read output until the public URL appears or the process exits
    while let Some(event) = rx.recv().await {
        let line = match &event {
            CommandEvent::Stdout(b) | CommandEvent::Stderr(b) => {
                String::from_utf8_lossy(b).into_owned()
            }
            CommandEvent::Terminated(_) => {
                tracing::info!("cloudflared: process terminated");
                let local_enabled = if let Some(h) = app.try_state::<crate::incoming_auth::IncomingAuthStateHandle>() {
                    h.0.set_tunnel_active(false);
                    h.0.local_enabled()
                } else {
                    false
                };
                let _ = app.emit("tunnel-status-changed", json!({ "url": null, "status": "Unavailable" }));
                let _ = app.emit("auth-status-changed", json!({
                    "tunnel_forced": false,
                    "local_enabled": local_enabled,
                    "effective":     local_enabled,
                }));
                break;
            }
            _ => continue,
        };

        tracing::debug!("cloudflared: {}", line.trim_end());

        if let Some(url) = extract_tunnel_url(&line) {
            if let Some(state) = app.try_state::<TunnelState>() {
                state.0.lock().unwrap().url = Some(url.clone());
            }
            let local_enabled = if let Some(h) = app.try_state::<crate::incoming_auth::IncomingAuthStateHandle>() {
                h.0.set_tunnel_active(true);
                h.0.local_enabled()
            } else {
                false
            };
            let _ = app.emit("tunnel-status-changed", json!({ "url": url, "status": "Active" }));
            let _ = app.emit("auth-status-changed", json!({
                "tunnel_forced": true,
                "local_enabled": local_enabled,
                "effective":     true,
            }));
            // Keep draining so cloudflared's pipe stays open
        }
    }
}

fn extract_tunnel_url(line: &str) -> Option<String> {
    let pos = line.find("https://")?;
    let rest = &line[pos..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/');
    if url.contains("trycloudflare.com") {
        Some(url.to_string())
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_url_from_cloudflared_output() {
        let line = "2024-01-01T00:00:00Z INF  |  https://abc-def-123.trycloudflare.com       |";
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://abc-def-123.trycloudflare.com".to_string())
        );
    }

    #[test]
    fn extract_url_ignores_non_tunnel() {
        let line = "connecting to https://api.cloudflare.com/something";
        assert_eq!(extract_tunnel_url(line), None);
    }

    #[test]
    fn extract_url_strips_trailing_slash() {
        let line = "visit https://my-tunnel.trycloudflare.com/ now";
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://my-tunnel.trycloudflare.com".to_string())
        );
    }
}
