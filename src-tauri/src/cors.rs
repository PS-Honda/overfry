use axum::http::HeaderMap;
use tower_http::cors::AllowOrigin;

/// Allowed MCP client origins (applied in both `origin_ok` and `make_cors_layer`):
///
/// | Origin                     | Client                              |
/// |----------------------------|-------------------------------------|
/// | (none)                     | Claude Desktop, curl, native tools  |
/// | tauri://…                  | Tauri webview (this app)            |
/// | https://claude.ai          | Claude.ai web app                   |
/// | http://localhost:PORT      | MCP Inspector, local dev tools      |
/// | http://127.0.0.1:PORT      | Same, alternate form                |
/// | vscode-webview://…         | VS Code extensions                  |
///
/// Everything else is rejected (DNS-rebinding / cross-origin protection).
pub fn origin_ok(headers: &HeaderMap) -> bool {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => true, // no Origin = same-origin or non-browser → allow
        Some(o) if o.starts_with("tauri://") => true,
        Some("https://claude.ai") => true,
        Some(o) if o.starts_with("http://localhost:") => true,
        Some(o) if o.starts_with("http://127.0.0.1:") => true,
        Some(o) if o.starts_with("vscode-webview://") => true,
        Some(_) => false,
    }
}

/// CORS response-header layer that mirrors the same allowlist as `origin_ok`.
pub fn make_cors_layer() -> tower_http::cors::CorsLayer {
    tower_http::cors::CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _req| {
            let o = origin.to_str().unwrap_or("");
            o == "https://claude.ai"
                || o.starts_with("tauri://")
                || o.starts_with("http://localhost:")
                || o.starts_with("http://127.0.0.1:")
                || o.starts_with("vscode-webview://")
        }))
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers(tower_http::cors::Any)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn with_origin(origin: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("origin", origin.parse().unwrap());
        h
    }

    #[test]
    fn no_origin_allowed() {
        assert!(origin_ok(&HeaderMap::new()));
    }

    #[test]
    fn tauri_allowed() {
        assert!(origin_ok(&with_origin("tauri://localhost")));
    }

    #[test]
    fn claude_ai_allowed() {
        assert!(origin_ok(&with_origin("https://claude.ai")));
    }

    #[test]
    fn localhost_port_allowed() {
        assert!(origin_ok(&with_origin("http://localhost:5173")));
        assert!(origin_ok(&with_origin("http://localhost:3000")));
    }

    #[test]
    fn loopback_port_allowed() {
        assert!(origin_ok(&with_origin("http://127.0.0.1:51552")));
    }

    #[test]
    fn vscode_webview_allowed() {
        assert!(origin_ok(&with_origin("vscode-webview://abc123")));
    }

    #[test]
    fn external_https_rejected() {
        assert!(!origin_ok(&with_origin("https://example.com")));
        assert!(!origin_ok(&with_origin("https://evil.com")));
    }

    #[test]
    fn external_http_rejected() {
        assert!(!origin_ok(&with_origin("http://evil.com")));
    }

    #[test]
    fn bare_localhost_no_port_rejected() {
        // Must have a port — bare "http://localhost" without port is suspicious
        assert!(!origin_ok(&with_origin("http://localhost")));
    }
}
