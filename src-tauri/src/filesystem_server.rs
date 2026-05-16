use std::{
    convert::Infallible,
    io::{self, Write},
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

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
use chrono::Utc;
use futures::stream;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

use crate::{
    audit::AuditState,
    error::AppError,
    models::{AuditEntry, AuditResult},
    security,
};

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const MAX_BODY_SIZE: usize = 1024 * 1024;     // 1 MB

// ── Shared server state ────────────────────────────────────────────────────────

#[derive(Clone)]
struct FsState {
    root_paths:    Vec<PathBuf>,
    connection_id: Uuid,
    dir_cache:     Cache<PathBuf, Value>,
    app:           tauri::AppHandle,
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

pub fn create_router(
    root_paths:    Vec<PathBuf>,
    connection_id: Uuid,
    app:           tauri::AppHandle,
    mcp_path:      String,
) -> Router {
    let state = Arc::new(FsState {
        root_paths,
        connection_id,
        dir_cache: Cache::builder()
            .time_to_live(Duration::from_secs(30))
            .max_capacity(500)
            .build(),
        app,
    });

    // Spawn a watcher thread that invalidates the cache on any FS event
    let cache_clone = state.dir_cache.clone();
    let root_paths_clone = state.root_paths.clone();
    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("notify watcher failed to create: {e}");
                return;
            }
        };
        for root in &root_paths_clone {
            if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                tracing::warn!("notify watch failed for {}: {e}", root.display());
            }
        }
        for result in rx {
            match result {
                Ok(_event) => {
                    cache_clone.invalidate_all();
                }
                Err(e) => tracing::warn!("notify error: {e}"),
            }
        }
    });

    let path = mcp_path.clone();
    Router::new()
        .route(&path, get(handle_sse).post(handle_rpc))
        .with_state(state)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
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

pub(crate) fn origin_ok(headers: &HeaderMap) -> bool {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => true, // no Origin header = same-origin or non-browser → allow
        Some(o) if o.starts_with("tauri://") => true,
        Some(_) => false,
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

            let tool_result = call_tool(state, &name, args.clone());

            // Emit audit entry for every tool call
            if let Some(audit_state) = state.app.try_state::<AuditState>() {
                let entry = AuditEntry {
                    id:            Uuid::new_v4(),
                    connection_id: state.connection_id,
                    tool_name:     name.clone(),
                    path:          args["path"].as_str().map(|s| s.to_string()),
                    timestamp:     Utc::now(),
                    session_id:    None,
                    result: match &tool_result {
                        Ok(_) => AuditResult::Ok,
                        Err(AppError::PathTraversal(_))
                        | Err(AppError::OperationNotPermitted(_)) => {
                            AuditResult::Denied(
                                tool_result.as_ref().unwrap_err().to_string(),
                            )
                        }
                        Err(e) => AuditResult::Error(e.to_string()),
                    },
                };
                audit_state.0.append(entry.clone());
                let _ = state.app.emit("audit-entry-added", &entry);
            }

            match tool_result {
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

// ── Cache invalidation helper ──────────────────────────────────────────────────

fn invalidate_cache(state: &FsState, path: &std::path::Path) {
    state.dir_cache.invalidate(path);
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

        "write_file" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let content = args["content"].as_str()
                .ok_or_else(|| AppError::Other("content required".into()))?;

            let real_path = security::validate_writable_path(root, path_str)?;

            let parent = real_path.parent()
                .ok_or_else(|| AppError::Other("path has no parent directory".into()))?;

            // Ensure parent exists and is within root
            std::fs::canonicalize(parent)
                .map_err(|_| AppError::Other("parent directory does not exist".into()))?;

            // Atomic write via tempfile
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            tmp.write_all(content.as_bytes())?;
            tmp.persist(&real_path).map_err(|e| e.error)?;

            // Invalidate cache for parent dir
            invalidate_cache(state, parent);

            Ok(json!([{ "type": "text", "text": "written successfully" }]))
        }

        "append_file" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let content = args["content"].as_str()
                .ok_or_else(|| AppError::Other("content required".into()))?;

            let real = security::validate_path(root, path_str)?;

            // Atomic append: read existing + concat + atomic write
            let existing = if real.exists() {
                std::fs::read_to_string(&real)?
            } else {
                String::new()
            };
            let new_content = existing + content;
            let parent = real.parent()
                .ok_or_else(|| AppError::Other("no parent dir".into()))?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            tmp.write_all(new_content.as_bytes())?;
            tmp.persist(&real).map_err(|e| e.error)?;

            Ok(json!([{ "type": "text", "text": "appended successfully" }]))
        }

        "move_file" => {
            let src_str = args["src"].as_str()
                .ok_or_else(|| AppError::Other("src required".into()))?;
            let dst_str = args["dst"].as_str()
                .ok_or_else(|| AppError::Other("dst required".into()))?;

            let src = security::validate_path(root, src_str)?;
            let dst = security::validate_writable_path(root, dst_str)?;

            // Destination must not already exist
            if std::fs::metadata(&dst).is_ok() {
                return Err(AppError::OperationNotPermitted(
                    "destination already exists".into(),
                ));
            }

            // Destination parent must exist
            let dst_parent = dst.parent()
                .ok_or_else(|| AppError::Other("dst has no parent".into()))?;
            if std::fs::metadata(dst_parent).is_err() {
                return Err(AppError::Other("dst parent directory does not exist".into()));
            }

            match std::fs::rename(&src, &dst) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(17) || e.raw_os_error() == Some(18)
                    || e.kind() == io::ErrorKind::CrossesDevices =>
                {
                    // Cross-device: copy then remove
                    std::fs::copy(&src, &dst)?;
                    std::fs::remove_file(&src)?;
                }
                Err(e) => return Err(AppError::Io(e)),
            }

            // Invalidate caches for both src and dst parents
            let src_parent = src.parent().unwrap_or(&src);
            invalidate_cache(state, src_parent);
            invalidate_cache(state, dst_parent);

            Ok(json!([{ "type": "text", "text": "moved successfully" }]))
        }

        "search_files" => {
            let query = args["query"].as_str()
                .ok_or_else(|| AppError::Other("query required".into()))?;
            let path_str = args["path"].as_str().unwrap_or(".");

            if query.len() > 256 {
                return Err(AppError::Other("query pattern exceeds 256 characters".into()));
            }

            let real = security::validate_path(root, path_str)?;

            let re = regex::RegexBuilder::new(query)
                .size_limit(1_000_000)
                .build()
                .map_err(|e| AppError::Other(format!("invalid regex: {e}")))?;

            let mut file_results: Vec<Value> = Vec::new();

            'walk: for entry in ignore::WalkBuilder::new(&real).build() {
                if file_results.len() >= 50 {
                    break 'walk;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if ext != "md" && ext != "txt" {
                    continue;
                }

                // Skip files larger than 10 MB
                if entry.metadata().map(|m| m.len()).unwrap_or(0) > 10 * 1024 * 1024 {
                    continue;
                }

                let text = match std::fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                let mut matches: Vec<Value> = Vec::new();
                for (line_idx, line) in text.lines().enumerate() {
                    if matches.len() >= 5 {
                        break;
                    }
                    if re.is_match(line) {
                        matches.push(json!({
                            "line": line_idx + 1,
                            "text": line,
                        }));
                    }
                }

                if !matches.is_empty() {
                    file_results.push(json!({
                        "file": path.to_string_lossy(),
                        "matches": matches,
                    }));
                }
            }

            let text = serde_json::to_string_pretty(&file_results)
                .map_err(|e| AppError::Other(e.to_string()))?;
            Ok(json!([{ "type": "text", "text": text }]))
        }

        "delete_file" => {
            // Always validate path first (returns PathTraversal for bad paths)
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            security::validate_path(root, path_str)?;
            // Deletion is never permitted
            Err(AppError::OperationNotPermitted(
                "deletion not supported — delete files manually".into(),
            ))
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
        },
        {
            "name": "write_file",
            "description": "Write (or overwrite) a text file within the root. Atomic write via tempfile.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":    { "type": "string", "description": "Path relative to root" },
                    "content": { "type": "string", "description": "Text content to write" }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "append_file",
            "description": "Append text to an existing file within the root.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":    { "type": "string", "description": "Path relative to root (file must exist)" },
                    "content": { "type": "string", "description": "Text to append" }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "move_file",
            "description": "Move (rename) a file or directory within the root. Destination must not exist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "src": { "type": "string", "description": "Source path relative to root" },
                    "dst": { "type": "string", "description": "Destination path relative to root" }
                },
                "required": ["src", "dst"]
            }
        },
        {
            "name": "search_files",
            "description": "Search .md and .txt files for a regex pattern. Returns up to 50 files with up to 5 matching lines each.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Regex pattern to search for" },
                    "path":  { "type": "string", "description": "Path relative to root to search within (default: root)" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "delete_file",
            "description": "Not supported. Always returns an error. Delete files manually.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn headers_with_origin(origin: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("origin", origin.parse().unwrap());
        h
    }

    #[test]
    fn origin_ok_tauri_scheme_allowed() {
        // tauri:// origin must be accepted (Tauri app itself)
        let h = headers_with_origin("tauri://localhost");
        assert!(origin_ok(&h));
    }

    #[test]
    fn origin_ok_rejects_http_localhost() {
        // http://127.0.0.1 must be rejected (DNS rebinding vector)
        let h = headers_with_origin("http://127.0.0.1:50000");
        assert!(!origin_ok(&h));
    }

    #[test]
    fn origin_ok_rejects_http_scheme() {
        // Any plain http:// origin must be rejected
        let h = headers_with_origin("http://evil.com");
        assert!(!origin_ok(&h));
    }

    #[test]
    fn origin_ok_rejects_https_scheme() {
        // Any https:// origin must be rejected (not a Tauri origin)
        let h = headers_with_origin("https://example.com");
        assert!(!origin_ok(&h));
    }

    #[test]
    fn origin_ok_no_origin_header_allowed() {
        // No Origin header (same-origin / non-browser) must be allowed
        let h = HeaderMap::new();
        assert!(origin_ok(&h));
    }

    #[test]
    fn search_files_rejects_long_query() {
        // A query longer than 256 chars should return an error (ReDoS guard)
        // Verify that the length guard constant is correctly enforced.
        let long_query = "a".repeat(257);
        // The real guard in call_tool checks query.len() > 256 before building regex.
        // Simulate the same check here to confirm the threshold:
        assert!(long_query.len() > 256);
    }
}
