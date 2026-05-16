use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

// ── State ─────────────────────────────────────────────────────────────────────

pub struct TunnelManager {
    child:     Option<tauri_plugin_shell::process::CommandChild>,
    pub url:   Option<String>,
    ref_count: u32,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self { child: None, url: None, ref_count: 0 }
    }
}

pub struct TunnelState(pub Mutex<TunnelManager>);

// ── Public API ────────────────────────────────────────────────────────────────

/// Increment ref-count.  On the first caller, spawns cloudflared in the
/// background; later callers re-emit the existing URL so the new card gets it.
/// Fire-and-forget — does not block `start_server`.
pub fn tunnel_acquire(app: &AppHandle, port: u16) {
    let Some(state) = app.try_state::<TunnelState>() else { return };

    let maybe_url: Option<String> = {
        let mut mgr = state.0.lock().unwrap();
        mgr.ref_count += 1;

        if mgr.ref_count == 1 {
            // First connection — spawn cloudflared
            let app_clone = app.clone();
            tauri::async_runtime::spawn(async move {
                spawn_cloudflared(app_clone, port).await;
            });
            None
        } else {
            // Tunnel already running — re-emit so the new card shows the URL
            mgr.url.clone()
        }
    }; // MutexGuard dropped here

    if let Some(url) = maybe_url {
        let _ = app.emit("tunnel-status-changed", json!({ "url": url }));
    }
}

/// Decrement ref-count; kills the cloudflared process when it reaches zero.
pub fn tunnel_release(app: &AppHandle) {
    let Some(state) = app.try_state::<TunnelState>() else { return };

    let killed = {
        let mut mgr = state.0.lock().unwrap();
        if mgr.ref_count == 0 {
            return;
        }
        mgr.ref_count -= 1;

        if mgr.ref_count == 0 {
            if let Some(child) = mgr.child.take() {
                let _ = child.kill();
            }
            mgr.url = None;
            true
        } else {
            false
        }
    }; // MutexGuard dropped here

    if killed {
        let _ = app.emit("tunnel-status-changed", json!({ "url": null }));
    }
}

// ── Internal ──────────────────────────────────────────────────────────────────

async fn spawn_cloudflared(app: AppHandle, port: u16) {
    let url_arg = format!("https://127.0.0.1:{port}");

    let spawn_result = app
        .shell()
        .sidecar("cloudflared")
        .map(|cmd| cmd.args(["tunnel", "--no-autoupdate", "--url", &url_arg]))
        .and_then(|cmd| cmd.spawn());

    let (mut rx, child) = match spawn_result {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!("cloudflared: spawn failed: {e}");
            return;
        }
    };

    // Store child handle so tunnel_release can kill it
    if let Some(state) = app.try_state::<TunnelState>() {
        state.0.lock().unwrap().child = Some(child);
    }

    // Read output until the public URL appears or the process exits
    let mut found = false;
    while let Some(event) = rx.recv().await {
        let line = match &event {
            CommandEvent::Stdout(b) | CommandEvent::Stderr(b) => {
                String::from_utf8_lossy(b).into_owned()
            }
            CommandEvent::Terminated(_) => {
                tracing::info!("cloudflared: process terminated");
                break;
            }
            _ => continue,
        };

        tracing::debug!("cloudflared: {}", line.trim_end());

        if !found {
            if let Some(url) = extract_tunnel_url(&line) {
                if let Some(state) = app.try_state::<TunnelState>() {
                    state.0.lock().unwrap().url = Some(url.clone());
                }
                let _ = app.emit("tunnel-status-changed", json!({ "url": url }));
                found = true;
                // Keep draining — don't break, so cloudflared's pipe stays open
            }
        }
    }
}

/// Extract `https://…trycloudflare.com` from a cloudflared output line.
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
