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

    // Parse body
    let body_value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_err(None, -32700, "Parse error")).into_response(),
    };

    let req_id: Value = body_value.get("id").cloned().unwrap_or(Value::Null);

    // First attempt
    let (audit_result, response) =
        do_proxy_request(&state, &body_value, req_id.clone()).await;

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

    match send_request(&state.client, &target_url, bearer.as_deref(), &state.auth, body_value).await {
        Err(msg) => (AuditResult::Error(msg.clone()), Json(rpc_err(Some(req_id), -32603, msg)).into_response()),
        Ok((status, bytes)) => {
            // 401 + OAuth → attempt refresh then retry once
            if status == 401 {
                let is_oauth = state.auth.read().await.auth_method == AuthMethod::OAuth;
                if is_oauth {
                    if let Ok(new_bearer) = try_refresh(state).await {
                        // Retry with new token
                        match send_request(&state.client, &target_url, Some(&new_bearer), &state.auth, body_value).await {
                            Err(msg) => return (AuditResult::Error(msg.clone()), Json(rpc_err(Some(req_id), -32603, msg)).into_response()),
                            Ok((_, bytes2)) => return parse_bytes(bytes2, req_id),
                        }
                    }
                }
                let msg = format!("upstream error: {status}");
                return (AuditResult::Error(msg.clone()), Json(rpc_err(Some(req_id), -32603, msg)).into_response());
            }
            if !status.is_success() {
                let msg = format!("upstream error: {status}");
                return (AuditResult::Error(msg.clone()), Json(rpc_err(Some(req_id), -32603, msg)).into_response());
            }
            parse_bytes(bytes, req_id)
        }
    }
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
) -> Result<(reqwest::StatusCode, Bytes), String> {
    let extra_headers = {
        let auth = auth_lock.read().await;
        auth.extra_headers.clone()
    };

    let mut req_builder = client
        .post(target_url)
        .header("Content-Type", "application/json");

    if let Some(token) = bearer {
        req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
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

    let resp = req_builder.send().await
        .map_err(|e| format!("upstream request failed: {e}"))?;

    let status = resp.status();
    let bytes  = resp.bytes().await
        .map_err(|e| format!("reading upstream body: {e}"))?;

    Ok((status, bytes))
}

fn parse_bytes(bytes: Bytes, req_id: Value) -> (AuditResult, Response) {
    match serde_json::from_slice::<Value>(&bytes) {
        Err(_) => {
            let msg = "upstream returned non-JSON body".to_string();
            (AuditResult::Error(msg.clone()), Json(rpc_err(Some(req_id), -32603, msg)).into_response())
        }
        Ok(mut val) => {
            if val.get("error").is_some() {
                val["id"] = req_id;
            }
            (AuditResult::Ok, Json(val).into_response())
        }
    }
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
    let stream = stream::pending::<Result<Event, Infallible>>();
    Sse::new(stream).into_response()
}

// ── Security ───────────────────────────────────────────────────────────────────

fn origin_ok(headers: &HeaderMap) -> bool {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => true, // no Origin header = same-origin or non-browser → allow
        Some(o) if o.starts_with("tauri://") => true,
        Some(_) => false,
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn rpc_err(id: Option<Value>, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message.into() }
    })
}
