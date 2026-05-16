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
    models::{AuditEntry, AuditResult, AuthConfig},
};

const MAX_BODY_SIZE: usize = 1024 * 1024; // 1 MB

// ── Shared server state ────────────────────────────────────────────────────────

#[derive(Clone)]
struct ProxyState {
    connection_id: Uuid,
    auth:          AuthConfig,
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
        auth,
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

    // Parse body to validate it is valid JSON
    let body_value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return Json(rpc_err(None, -32700, "Parse error")).into_response();
        }
    };

    // Extract request id to echo back in error responses (JSON-RPC spec)
    let req_id: Value = body_value.get("id").cloned().unwrap_or(Value::Null);

    // Determine upstream URL
    let target_url = format!("{}/mcp", state.auth.base_url.trim_end_matches('/'));

    // Build upstream request
    let mut req_builder = state
        .client
        .post(&target_url)
        .header("Content-Type", "application/json");

    // Inject bearer token if non-empty
    if !state.auth.token.is_empty() {
        req_builder = req_builder.header(
            "Authorization",
            format!("Bearer {}", state.auth.token),
        );
    }

    // Inject extra headers
    for (k, v) in &state.auth.extra_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            req_builder = req_builder.header(name, value);
        }
    }

    req_builder = req_builder.json(&body_value);

    let upstream_result = req_builder.send().await;

    // Determine audit result and response
    let (audit_result, response) = match upstream_result {
        Err(e) => {
            let msg = format!("upstream request failed: {e}");
            (
                AuditResult::Error(msg.clone()),
                Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
            )
        }
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let msg = format!("upstream error: {status}");
                (
                    AuditResult::Error(msg.clone()),
                    Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
                )
            } else {
                match resp.bytes().await {
                    Err(e) => {
                        let msg = format!("reading upstream body: {e}");
                        (
                            AuditResult::Error(msg.clone()),
                            Json(rpc_err(Some(req_id), -32603, msg)).into_response(),
                        )
                    }
                    Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                        Err(_) => {
                            let msg = "upstream returned non-JSON body".to_string();
                            (
                                AuditResult::Error(msg.clone()),
                                Json(rpc_err(Some(req_id.clone()), -32603, msg)).into_response(),
                            )
                        }
                        Ok(mut val) => {
                            // Pass through upstream JSON-RPC errors with correct id
                            if val.get("error").is_some() {
                                val["id"] = req_id;
                                (AuditResult::Ok, Json(val).into_response())
                            } else {
                                (AuditResult::Ok, Json(val).into_response())
                            }
                        },
                    },
                }
            }
        }
    };

    // Emit audit entry
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

    response
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
