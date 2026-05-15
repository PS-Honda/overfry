use std::{convert::Infallible, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use futures::stream;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{error::AppError, security};

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

// ── Shared server state ────────────────────────────────────────────────────────

#[derive(Clone)]
struct FsState {
    root_paths: Vec<PathBuf>,
    dir_cache:  Cache<PathBuf, Value>,
}

// ── JSON-RPC types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id:      Option<Value>,
    method:  String,
    params:  Option<Value>,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id:      Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result:  Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error:   Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code:    i32,
    message: String,
}

// ── Router factory ─────────────────────────────────────────────────────────────

pub fn create_router(root_paths: Vec<PathBuf>) -> Router {
    let state = Arc::new(FsState {
        root_paths,
        dir_cache: Cache::builder()
            .time_to_live(Duration::from_secs(30))
            .max_capacity(500)
            .build(),
    });
    Router::new()
        .route("/mcp", get(handle_sse).post(handle_rpc))
        .with_state(state)
}

// ── Handlers ───────────────────────────────────────────────────────────────────

async fn handle_rpc(
    State(state): State<Arc<FsState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !origin_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_err(None, -32700, "Parse error")).into_response(),
    };

    if body.is_array() {
        let responses: Vec<Value> = body.as_array().unwrap().iter()
            .filter_map(|r| serde_json::from_value::<RpcRequest>(r.clone()).ok())
            .filter(|r| r.id.is_some())
            .map(|req| serde_json::to_value(dispatch(&state, req)).unwrap_or(Value::Null))
            .collect();
        Json(Value::Array(responses)).into_response()
    } else {
        match serde_json::from_value::<RpcRequest>(body) {
            // Notifications (no id) — acknowledge without body
            Ok(req) if req.id.is_none() => StatusCode::ACCEPTED.into_response(),
            Ok(req) => {
                Json(serde_json::to_value(dispatch(&state, req)).unwrap_or(Value::Null))
                    .into_response()
            }
            Err(_) => Json(rpc_err(None, -32700, "Parse error")).into_response(),
        }
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
    match headers.get("origin") {
        None => true,
        Some(v) => {
            let o = v.to_str().unwrap_or("");
            o.starts_with("tauri://")
                || o.starts_with("http://127.0.0.1")
                || o.starts_with("http://localhost")
        }
    }
}

// ── MCP dispatch ───────────────────────────────────────────────────────────────

fn dispatch(state: &FsState, req: RpcRequest) -> RpcResponse {
    match req.method.as_str() {
        "initialize" => rpc_ok(
            req.id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "overfry-filesystem", "version": "0.1.0" }
            }),
        ),
        "tools/list" => rpc_ok(req.id, json!({ "tools": tool_defs() })),
        "tools/call" => {
            let params = req.params.unwrap_or(Value::Null);
            let name = params["name"].as_str().unwrap_or("").to_string();
            let args = params["arguments"].clone();
            match call_tool(state, &name, args) {
                Ok(content) => rpc_ok(req.id, json!({ "content": content, "isError": false })),
                Err(e) => rpc_ok(
                    req.id,
                    json!({ "content": [{"type":"text","text": e.to_string()}], "isError": true }),
                ),
            }
        }
        _ => rpc_err(req.id, -32601, "Method not found"),
    }
}

// ── Tool implementations ───────────────────────────────────────────────────────

fn call_tool(state: &FsState, name: &str, args: Value) -> Result<Value, AppError> {
    let root = state.root_paths.first()
        .ok_or_else(|| AppError::Other("no root paths configured".into()))?;

    match name {
        "list_directory" => {
            let path_str = args["path"].as_str().unwrap_or(".");
            let cache_key = root.join(path_str.trim_start_matches(['/', '\\']));

            if let Some(cached) = state.dir_cache.get(&cache_key) {
                return Ok(cached);
            }

            let real = security::validate_path(root, path_str)?;
            let mut entries: Vec<Value> = std::fs::read_dir(&real)?
                .filter_map(|e| e.ok())
                .map(|e| {
                    let meta = e.metadata().ok();
                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    let size = meta.as_ref().filter(|m| m.is_file()).map(|m| m.len());
                    json!({
                        "name": e.file_name().to_string_lossy().into_owned(),
                        "type": if is_dir { "directory" } else { "file" },
                        "size": size,
                    })
                })
                .collect();

            // directories first, then alphabetical
            entries.sort_by(|a, b| {
                b["type"].as_str().cmp(&a["type"].as_str())
                    .then(a["name"].as_str().cmp(&b["name"].as_str()))
            });

            let text = serde_json::to_string_pretty(&entries)
                .map_err(|e| AppError::Other(e.to_string()))?;
            let result = json!([{ "type": "text", "text": text }]);
            state.dir_cache.insert(cache_key, result.clone());
            Ok(result)
        }

        "read_file" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let real = security::validate_path(root, path_str)?;
            let size = std::fs::metadata(&real)?.len();
            if size > MAX_FILE_SIZE {
                return Err(AppError::FileTooLarge(size));
            }
            let bytes = std::fs::read(&real)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| AppError::Other("binary file: cannot read as text".into()))?;
            Ok(json!([{ "type": "text", "text": text }]))
        }

        _ => Err(AppError::Other(format!("unknown tool: {name}"))),
    }
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "list_directory",
            "description": "List files and directories at a path within the root. Use '.' for root.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to root" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "read_file",
            "description": "Read text content of a file within the root. Returns error for binary files. Max 10 MB.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to root" }
                },
                "required": ["path"]
            }
        }
    ])
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn rpc_ok(id: Option<Value>, result: Value) -> RpcResponse {
    RpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
}

fn rpc_err(id: Option<Value>, code: i32, message: impl Into<String>) -> RpcResponse {
    RpcResponse { jsonrpc: "2.0", id, result: None, error: Some(RpcError { code, message: message.into() }) }
}
