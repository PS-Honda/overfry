use std::{convert::Infallible, sync::Arc};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use chrono::Utc;
use futures::stream;
use reqwest::Client;
use serde_json::{json, Value};
use tower_http::timeout::TimeoutLayer;
use std::time::Duration;
use uuid::Uuid;

use tauri::{Emitter, Manager};

use crate::{
    audit::AuditState,
    cors::{make_cors_layer, origin_ok},
    error::AppError,
    models::{AuditEntry, AuditResult, AuthConfig, AuthMethod},
    store::StoreState,
};

const MAX_BODY_SIZE: usize = 1024 * 1024; // 1 MB

// ── Shared server state ────────────────────────────────────────────────────────

#[derive(Clone)]
struct ProxyState {
    connection_id: Uuid,
    auth:          Arc<tokio::sync::RwLock<AuthConfig>>,
    client:        Client,
    app:           tauri::AppHandle,
}

// ── Router factory ─────────────────────────────────────────────────────────────

pub fn create_router(
    connection_id: Uuid,
    auth:          AuthConfig,
    app:           tauri::AppHandle,
    mcp_path:      String,
) -> Result<Router, AppError> {
    let client = if auth.preset.as_deref() == Some("obsidian") {
        Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| AppError::Upstream(e.to_string()))?
    } else {
        Client::builder()
            .build()
            .map_err(|e| AppError::Upstream(e.to_string()))?
    };

    let state = Arc::new(ProxyState {
        connection_id,
        auth: Arc::new(tokio::sync::RwLock::new(auth)),
        client,
        app,
    });

    let path = mcp_path.clone();
    let router = Router::new()
        .route(&path, get(handle_sse).post(handle_rpc))
        .with_state(state)
        .layer(make_cors_layer())
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ));

    Ok(router)
}

// ── Handlers ───────────────────────────────────────────────────────────────────

async fn handle_rpc(
    State(state): State<Arc<ProxyState>>,
    headers:      HeaderMap,
    body:         Bytes,
) -> Response {
    if !origin_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Extract Mcp-Session-Id to forward to upstream
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    // Parse body
    let body_value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_err(None, -32700, "Parse error")).into_response(),
    };

    let req_id: Value = body_value.get("id").cloned().unwrap_or(Value::Null);

    let (audit_result, response) =
        do_proxy_request(&state, &body_value, req_id.clone(), session_id).await;

    // Emit audit
    {
        let auth = state.auth.read().await;
        let target_url = auth.base_url.trim_end_matches('/').to_string();
        drop(auth);
        if let Some(audit_state) = state.app.try_state::<AuditState>() {
            let entry = AuditEntry {
                id:            Uuid::new_v4(),
                connection_id: state.connection_id,
                tool_name:     "proxy".to_string(),
                path:          Some(target_url),
                timestamp:     Utc::now(),
                session_id:    None,
                result:        audit_result,
            };
            audit_state.0.append(entry.clone());
            let _ = state.app.emit("audit-entry-added", &entry);
        }
    }

    response
}

/// Execute a single proxy request. Returns (audit_result, response).
/// When the upstream returns 401 and OAuth is configured, tries to refresh
/// the token once and retries.
async fn do_proxy_request(
    state:      &Arc<ProxyState>,
    body_value: &Value,
    req_id:     Value,
    session_id: Option<String>,
) -> (AuditResult, Response) {
    let (target_url, bearer) = {
        let auth = state.auth.read().await;
        let url = auth.base_url.trim_end_matches('/').to_string();
        let bearer = pick_bearer(&auth);
        (url, bearer)
    };

    // If OAuth and no access_token yet, return friendly error
    {
        let auth = state.auth.read().await;
        if auth.auth_method == AuthMethod::OAuth && auth.access_token.is_empty() {
            let msg = "OAuth not authorized — click 'Authorize' on the connection card first.";
            return (
                AuditResult::Error(msg.to_string()),
                Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
            );
        }
    }

    match send_request(
        &state.client,
        &target_url,
        bearer.as_deref(),
        &state.auth,
        body_value,
        session_id.as_deref(),
    )
    .await
    {
        Err(msg) => (
            AuditResult::Error(msg.clone()),
            Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
        ),
        Ok((status, resp_headers, bytes)) => {
            // 401 + OAuth → attempt refresh then retry once
            if status == 401 {
                let is_oauth = state.auth.read().await.auth_method == AuthMethod::OAuth;
                if is_oauth {
                    if let Ok(new_bearer) = try_refresh(state).await {
                        match send_request(
                            &state.client,
                            &target_url,
                            Some(&new_bearer),
                            &state.auth,
                            body_value,
                            session_id.as_deref(),
                        )
                        .await
                        {
                            Err(msg) => {
                                return (
                                    AuditResult::Error(msg.clone()),
                                    Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
                                )
                            }
                            Ok((_, retry_headers, bytes2)) => {
                                return build_response(bytes2, req_id, &retry_headers);
                            }
                        }
                    }
                }
                let msg = format!("upstream error: {status}");
                return (
                    AuditResult::Error(msg.clone()),
                    Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
                );
            }
            if !status.is_success() {
                let msg = format!("upstream error: {status}");
                return (
                    AuditResult::Error(msg.clone()),
                    Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
                );
            }
            build_response(bytes, req_id, &resp_headers)
        }
    }
}

/// Dispatch bytes to the right response builder based on upstream Content-Type,
/// then attach upstream `Mcp-Session-Id` to the response if present.
fn build_response(
    bytes:        Bytes,
    req_id:       Value,
    resp_headers: &reqwest::header::HeaderMap,
) -> (AuditResult, Response) {
    let is_sse = resp_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("text/event-stream"))
        .unwrap_or(false);

    let (audit, mut response) = if is_sse {
        sse_passthrough(bytes, req_id)
    } else {
        parse_bytes(bytes, req_id)
    };

    // Forward Mcp-Session-Id from upstream response to client
    if let Some(sid) = resp_headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(val) = HeaderValue::from_str(sid) {
            response.headers_mut().insert("mcp-session-id", val);
        }
    }

    (audit, response)
}

fn pick_bearer(auth: &AuthConfig) -> Option<String> {
    match auth.auth_method {
        AuthMethod::OAuth => {
            if !auth.access_token.is_empty() { Some(auth.access_token.clone()) } else { None }
        }
        AuthMethod::Token => {
            if !auth.token.is_empty() { Some(auth.token.clone()) } else { None }
        }
    }
}

async fn send_request(
    client:     &Client,
    target_url: &str,
    bearer:     Option<&str>,
    auth_lock:  &Arc<tokio::sync::RwLock<AuthConfig>>,
    body_value: &Value,
    session_id: Option<&str>,
) -> Result<(reqwest::StatusCode, reqwest::header::HeaderMap, Bytes), String> {
    let extra_headers = {
        let auth = auth_lock.read().await;
        auth.extra_headers.clone()
    };

    let mut req_builder = client
        .post(target_url)
        .header("Content-Type", "application/json")
        // Per MCP spec §2: client MUST include both content types in Accept
        .header("Accept", "application/json, text/event-stream");

    if let Some(token) = bearer {
        req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
    }

    // Forward Mcp-Session-Id for stateful session continuity (spec §3.3)
    if let Some(sid) = session_id {
        if let Ok(val) = reqwest::header::HeaderValue::from_str(sid) {
            req_builder = req_builder.header("Mcp-Session-Id", val);
        }
    }

    for (k, v) in &extra_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            req_builder = req_builder.header(name, value);
        }
    }

    req_builder = req_builder.json(body_value);

    let resp = req_builder
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;

    let status      = resp.status();
    let resp_headers = resp.headers().clone();
    let bytes       = resp
        .bytes()
        .await
        .map_err(|e| format!("reading upstream body: {e}"))?;

    Ok((status, resp_headers, bytes))
}

/// Parse plain-JSON upstream response.
fn parse_bytes(bytes: Bytes, req_id: Value) -> (AuditResult, Response) {
    // Try plain JSON first
    if let Ok(mut val) = serde_json::from_slice::<Value>(&bytes) {
        if val.get("error").is_some() {
            val["id"] = req_id;
        }
        return (AuditResult::Ok, Json(val).into_response());
    }

    // Upstream may respond with SSE (text/event-stream) without the Content-Type
    // header being set — extract first data: event as fallback
    if let Ok(text) = std::str::from_utf8(&bytes) {
        for line in text.lines() {
            let Some(payload) = line.strip_prefix("data: ") else { continue };
            if payload.trim() == "[DONE]" { continue; }
            if let Ok(mut val) = serde_json::from_str::<Value>(payload) {
                if val.get("error").is_some() {
                    val["id"] = req_id;
                }
                return (AuditResult::Ok, Json(val).into_response());
            }
        }
    }

    let msg = "upstream returned non-JSON body".to_string();
    (AuditResult::Error(msg.clone()), Json(rpc_err(Some(req_id), -32603, msg)).into_response())
}

/// Stream all SSE events from an upstream `text/event-stream` response back
/// to the client as SSE, preserving all events (not just the first).
fn sse_passthrough(bytes: Bytes, req_id: Value) -> (AuditResult, Response) {
    let events: Vec<Result<Event, Infallible>> = if let Ok(text) = std::str::from_utf8(&bytes) {
        text.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|p| p.trim() != "[DONE]")
            .filter_map(|p| serde_json::from_str::<Value>(p).ok())
            .filter_map(|val| Event::default().json_data(val).ok())
            .map(Ok)
            .collect()
    } else {
        vec![]
    };

    if events.is_empty() {
        // Fall back to parse_bytes in case Content-Type was wrong
        return parse_bytes(bytes, req_id);
    }

    (AuditResult::Ok, Sse::new(stream::iter(events)).into_response())
}

async fn try_refresh(state: &Arc<ProxyState>) -> Result<String, ()> {
    let (token_url, client_id, client_secret, refresh_token) = {
        let auth = state.auth.read().await;
        (
            auth.oauth_token_url.clone(),
            auth.client_id.clone(),
            auth.client_secret.clone(),
            auth.refresh_token.clone(),
        )
    };

    if token_url.is_empty() || refresh_token.is_empty() {
        return Err(());
    }

    match crate::oauth::refresh_access_token(&token_url, &client_id, &client_secret, &refresh_token).await {
        Ok((access, refresh)) => {
            // Persist new tokens to store
            let conn_id = state.connection_id.to_string();
            if let Some(store_state) = state.app.try_state::<StoreState>() {
                let s = store_state.0.lock().unwrap();
                if let Ok(mut conns) = s.load_connections() {
                    if let Some(c) = conns.iter_mut().find(|c| c.id.to_string() == conn_id) {
                        if let Some(ref mut a) = c.auth_config {
                            a.access_token  = access.clone();
                            a.refresh_token = refresh.clone();
                        }
                    }
                    let _ = s.save_connections(&conns);
                }
            }
            // Update in-memory state
            {
                let mut auth = state.auth.write().await;
                auth.access_token  = access.clone();
                auth.refresh_token = refresh;
            }
            Ok(access)
        }
        Err(_) => Err(()),
    }
}

async fn handle_sse(headers: HeaderMap) -> Response {
    if !origin_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Keep SSE connection open for server-initiated messages (spec §4).
    // We don't currently push server-initiated events, but the pending stream
    // correctly returns Content-Type: text/event-stream and stays open.
    let stream = stream::pending::<Result<Event, Infallible>>();
    Sse::new(stream).into_response()
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn rpc_err(id: Option<Value>, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message.into() }
    })
}
