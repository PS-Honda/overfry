use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

const MAX_RETRIES: u32 = 5;
const RETRY_DELAY_SECS: u64 = 15;

// ── State ─────────────────────────────────────────────────────────────────────

pub struct TunnelManager {
    child: Option<tauri_plugin_shell::process::CommandChild>,
    pub url: Option<String>,
    pub status: TunnelStatus,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum TunnelStatus {
    Connecting,
    Active,
    Unavailable,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self { child: None, url: None, status: TunnelStatus::Unavailable }
    }
}

pub struct TunnelState(pub Mutex<TunnelManager>);

// ── Public API ────────────────────────────────────────────────────────────────

/// Start the Cloudflare tunnel. Called once on app launch.
/// Emits `tunnel-status-changed { status: "Connecting" }` immediately,
/// then `{ url, status: "Active" }` when URL is found,
/// or `{ status: "Unavailable" }` if spawn fails.
pub fn tunnel_start(app: &AppHandle, port: u16) {
    // Mark as Connecting in state so get_tunnel_status returns it immediately
    if let Some(state) = app.try_state::<TunnelState>() {
        let mut m = state.0.lock().unwrap();
        m.status = TunnelStatus::Connecting;
        m.url = None;
    }
    let _ = app.emit("tunnel-status-changed", json!({ "url": null, "status": "Connecting" }));

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        spawn_cloudflared_with_retry(app_clone, port, MAX_RETRIES).await;
    });
}

/// Returns current tunnel status snapshot for frontend to query on startup/reload.
pub fn get_status_snapshot(app: &AppHandle) -> serde_json::Value {
    if let Some(state) = app.try_state::<TunnelState>() {
        let m = state.0.lock().unwrap();
        let status_str = match m.status {
            TunnelStatus::Connecting   => "Connecting",
            TunnelStatus::Active       => "Active",
            TunnelStatus::Unavailable  => "Unavailable",
        };
        json!({ "url": m.url, "status": status_str })
    } else {
        json!({ "url": null, "status": "Unavailable" })
    }
}

// ── Internal ──────────────────────────────────────────────────────────────────

async fn spawn_cloudflared_with_retry(app: AppHandle, port: u16, max_retries: u32) {
    for attempt in 0..=max_retries {
        let retries_left = max_retries - attempt;
        let done = spawn_cloudflared_once(&app, port, retries_left).await;
        if done { break; }
        // Not done = should retry; delay already slept inside
    }
}

/// Returns `true` when the retry loop should stop.
async fn spawn_cloudflared_once(app: &AppHandle, port: u16, retries_left: u32) -> bool {
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
            emit_unavailable(app, retries_left > 0);
            if retries_left > 0 {
                tracing::info!("cloudflared: retrying in {RETRY_DELAY_SECS}s ({retries_left} attempts left)");
                tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
            }
            return retries_left == 0; // true = stop, false = try again
        }
    };

    // Store child handle. Keep a local Option in case try_state fails
    // (shouldn't happen, but avoids silent kill if state is unavailable).
    let _local_child = if let Some(state) = app.try_state::<TunnelState>() {
        let mut m = state.0.lock().unwrap();
        m.status = TunnelStatus::Connecting;
        m.url = None;
        m.child = Some(child);
        None::<tauri_plugin_shell::process::CommandChild>
    } else {
        Some(child) // keep alive locally
    };

    // Drain stdout/stderr until URL found or process exits
    while let Some(event) = rx.recv().await {
        let line = match &event {
            CommandEvent::Stdout(b) | CommandEvent::Stderr(b) => {
                String::from_utf8_lossy(b).into_owned()
            }
            CommandEvent::Terminated(status) => {
                tracing::info!("cloudflared: process terminated (exit={:?})", status.code);
                emit_unavailable(app, retries_left > 0);
                if retries_left > 0 {
                    tracing::info!("cloudflared: retrying in {RETRY_DELAY_SECS}s ({retries_left} attempts left)");
                    tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
                }
                return retries_left == 0;
            }
            _ => continue,
        };

        tracing::debug!("cloudflared: {}", line.trim_end());

        if let Some(url) = extract_tunnel_url(&line) {
            if let Some(state) = app.try_state::<TunnelState>() {
                let mut m = state.0.lock().unwrap();
                m.url = Some(url.clone());
                m.status = TunnelStatus::Active;
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

    true // rx closed without Terminated — process likely still running, stop loop
}

fn emit_unavailable(app: &AppHandle, will_retry: bool) {
    let local_enabled = if let Some(h) = app.try_state::<crate::incoming_auth::IncomingAuthStateHandle>() {
        h.0.set_tunnel_active(false);
        h.0.local_enabled()
    } else {
        false
    };
    if let Some(state) = app.try_state::<TunnelState>() {
        let mut m = state.0.lock().unwrap();
        m.status = TunnelStatus::Unavailable;
        m.url = None;
        m.child = None;
    }
    let status = if will_retry { "Connecting" } else { "Unavailable" };
    let _ = app.emit("tunnel-status-changed", json!({ "url": null, "status": status }));
    let _ = app.emit("auth-status-changed", json!({
        "tunnel_forced": false,
        "local_enabled": local_enabled,
        "effective":     local_enabled,
    }));
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
