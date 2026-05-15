use std::{
    collections::HashMap,
    convert::Infallible,
    io::Write,
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
use gray_matter::{engine::YAML, Matter};
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tower::ServiceBuilder;
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

    Router::new()
        .route("/mcp", get(handle_sse).post(handle_rpc))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY_SIZE)),
        )
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
                "serverInfo": { "name": "overfry-obsidian-filesystem", "version": "0.2.0" }
            }),
        ),
        "tools/list" => rpc_ok(req.id, json!({ "tools": tool_defs() })),
        "tools/call" => {
            let params = req.params.unwrap_or(Value::Null);
            let name = params["name"].as_str().unwrap_or("").to_string();
            let args = params["arguments"].clone();

            let tool_result = call_tool(state, &name, args.clone());

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

// ── Frontmatter helpers ────────────────────────────────────────────────────────

/// Parse frontmatter from file content. Returns (frontmatter as JSON Value, body string).
/// If no frontmatter present, returns (empty object, full content).
fn parse_frontmatter(content: &str) -> (Value, String) {
    let matter: Matter<YAML> = Matter::new();
    let result = matter.parse(content);

    let fm_value = match result.data {
        Some(pod) => {
            pod.deserialize::<Value>().unwrap_or_else(|_| json!({}))
        }
        None => json!({}),
    };

    (fm_value, result.content)
}

/// Serialize frontmatter value back to YAML string.
fn serialize_frontmatter(value: &Value) -> Result<String, AppError> {
    serde_yaml::to_string(value)
        .map_err(|e| AppError::Other(format!("YAML serialization failed: {e}")))
}

/// Reconstruct file content with updated frontmatter.
fn reconstruct_file(fm_value: &Value, body: &str) -> Result<String, AppError> {
    let yaml_str = serialize_frontmatter(fm_value)?;
    // serde_yaml adds a leading "---\n" document marker; strip it if present
    let yaml_body = yaml_str
        .strip_prefix("---\n")
        .unwrap_or(&yaml_str);
    Ok(format!("---\n{yaml_body}---\n{body}"))
}

// ── Heading section finder ─────────────────────────────────────────────────────

/// Returns `(heading_line_end, section_start, section_end)` as byte offsets.
/// `heading_line_end` = index after the heading line (including `\n`).
/// `section_start`    = first content byte after heading (same as heading_line_end).
/// `section_end`      = byte index where next same-or-higher heading starts (or content.len()).
fn find_heading_section(content: &str, heading: &str) -> Option<(usize, usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let mut found_level: Option<u32> = None;
    let mut heading_line_end: Option<usize> = None;
    let mut section_start: Option<usize> = None;
    let mut pos: usize = 0;

    for line in lines.iter() {
        let trimmed = line.trim_start_matches('#');
        let hashes = line.len() - trimmed.len();
        let is_heading = hashes > 0 && trimmed.starts_with(' ');
        let text = trimmed.trim();

        if is_heading {
            if let Some(level) = found_level {
                if (hashes as u32) <= level {
                    // Next same-or-higher heading found — section ends here
                    return Some((heading_line_end.unwrap(), section_start.unwrap(), pos));
                }
            } else if text.eq_ignore_ascii_case(heading) {
                found_level = Some(hashes as u32);
                heading_line_end = Some(pos + line.len() + 1); // +1 for \n
                section_start = Some(pos + line.len() + 1);
            }
        }
        pos += line.len() + 1; // +1 for \n
    }

    if found_level.is_some() {
        Some((heading_line_end.unwrap(), section_start.unwrap(), content.len()))
    } else {
        None
    }
}

// ── Tag extraction helpers ─────────────────────────────────────────────────────

/// Extract inline #tags from markdown content (not inside frontmatter).
fn extract_inline_tags(body: &str) -> Vec<String> {
    let re = regex::Regex::new(r"#([a-zA-Z][a-zA-Z0-9/_-]*)").expect("valid regex");
    re.captures_iter(body)
        .map(|cap| cap[1].to_string())
        .collect()
}

/// Extract tags from a frontmatter `tags:` field (list or comma-separated string).
fn extract_frontmatter_tags(fm: &Value) -> Vec<String> {
    match fm.get("tags") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

// ── Tool implementations ───────────────────────────────────────────────────────

fn call_tool(state: &FsState, name: &str, args: Value) -> Result<Value, AppError> {
    let root = state.root_paths.first()
        .ok_or_else(|| AppError::Other("no root paths configured".into()))?;

    match name {
        // ── Base filesystem tools (identical to filesystem_server) ──────────

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

            std::fs::canonicalize(parent)
                .map_err(|_| AppError::Other("parent directory does not exist".into()))?;

            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            tmp.write_all(content.as_bytes())?;
            tmp.persist(&real_path).map_err(|e| e.error)?;

            invalidate_cache(state, parent);
            Ok(json!([{ "type": "text", "text": "written successfully" }]))
        }

        "append_file" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let content = args["content"].as_str()
                .ok_or_else(|| AppError::Other("content required".into()))?;

            let real = security::validate_path(root, path_str)?;

            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&real)?;
            f.write_all(content.as_bytes())?;

            Ok(json!([{ "type": "text", "text": "appended successfully" }]))
        }

        "move_file" => {
            let src_str = args["src"].as_str()
                .ok_or_else(|| AppError::Other("src required".into()))?;
            let dst_str = args["dst"].as_str()
                .ok_or_else(|| AppError::Other("dst required".into()))?;

            let src = security::validate_path(root, src_str)?;
            let dst = security::validate_writable_path(root, dst_str)?;

            if std::fs::metadata(&dst).is_ok() {
                return Err(AppError::OperationNotPermitted(
                    "destination already exists".into(),
                ));
            }

            let dst_parent = dst.parent()
                .ok_or_else(|| AppError::Other("dst has no parent".into()))?;
            if std::fs::metadata(dst_parent).is_err() {
                return Err(AppError::Other("dst parent directory does not exist".into()));
            }

            match std::fs::rename(&src, &dst) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(17) || e.raw_os_error() == Some(18)
                    || e.kind() == std::io::ErrorKind::CrossesDevices =>
                {
                    std::fs::copy(&src, &dst)?;
                    std::fs::remove_file(&src)?;
                }
                Err(e) => return Err(AppError::Io(e)),
            }

            let src_parent = src.parent().unwrap_or(&src);
            invalidate_cache(state, src_parent);
            invalidate_cache(state, dst_parent);

            Ok(json!([{ "type": "text", "text": "moved successfully" }]))
        }

        "search_files" => {
            let query = args["query"].as_str()
                .ok_or_else(|| AppError::Other("query required".into()))?;
            let path_str = args["path"].as_str().unwrap_or(".");

            let real = security::validate_path(root, path_str)?;

            let re = regex::Regex::new(query)
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
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            security::validate_path(root, path_str)?;
            Err(AppError::OperationNotPermitted(
                "deletion not supported — delete files manually".into(),
            ))
        }

        // ── Obsidian-specific tools ────────────────────────────────────────

        "read_frontmatter" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let real = security::validate_path(root, path_str)?;

            let size = std::fs::metadata(&real)?.len();
            if size > MAX_FILE_SIZE {
                return Err(AppError::FileTooLarge(size));
            }
            let content = std::fs::read_to_string(&real)
                .map_err(|_| AppError::Other("cannot read file as text".into()))?;

            let (fm_value, _body) = parse_frontmatter(&content);
            let text = serde_json::to_string_pretty(&fm_value)
                .map_err(|e| AppError::Other(e.to_string()))?;

            Ok(json!([{ "type": "text", "text": text }]))
        }

        "write_frontmatter" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let key = args["key"].as_str()
                .ok_or_else(|| AppError::Other("key required".into()))?;
            let value = args["value"].clone();
            if value.is_null() {
                return Err(AppError::Other("value required".into()));
            }

            let real = security::validate_path(root, path_str)?;

            let size = std::fs::metadata(&real)?.len();
            if size > MAX_FILE_SIZE {
                return Err(AppError::FileTooLarge(size));
            }
            let content = std::fs::read_to_string(&real)
                .map_err(|_| AppError::Other("cannot read file as text".into()))?;

            let (mut fm_value, body) = parse_frontmatter(&content);

            // Ensure fm_value is an object
            if !fm_value.is_object() {
                fm_value = json!({});
            }
            fm_value.as_object_mut()
                .ok_or_else(|| AppError::Other("frontmatter is not an object".into()))?
                .insert(key.to_string(), value);

            let new_content = reconstruct_file(&fm_value, &body)?;

            let parent = real.parent()
                .ok_or_else(|| AppError::Other("path has no parent directory".into()))?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            tmp.write_all(new_content.as_bytes())?;
            tmp.persist(&real).map_err(|e| e.error)?;

            invalidate_cache(state, parent);
            Ok(json!([{ "type": "text", "text": "frontmatter updated successfully" }]))
        }

        "patch_file" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let target_type = args["target_type"].as_str()
                .ok_or_else(|| AppError::Other("target_type required".into()))?;
            let target = args["target"].as_str()
                .ok_or_else(|| AppError::Other("target required".into()))?;
            let operation = args["operation"].as_str()
                .ok_or_else(|| AppError::Other("operation required".into()))?;
            let patch_content = args["content"].as_str()
                .ok_or_else(|| AppError::Other("content required".into()))?;

            let real = security::validate_path(root, path_str)?;

            let size = std::fs::metadata(&real)?.len();
            if size > MAX_FILE_SIZE {
                return Err(AppError::FileTooLarge(size));
            }
            let file_content = std::fs::read_to_string(&real)
                .map_err(|_| AppError::Other("cannot read file as text".into()))?;

            let new_content = match target_type {
                "frontmatter" => {
                    let (mut fm_value, body) = parse_frontmatter(&file_content);

                    if !fm_value.is_object() {
                        fm_value = json!({});
                    }
                    let obj = fm_value.as_object_mut()
                        .ok_or_else(|| AppError::Other("frontmatter is not an object".into()))?;

                    let existing = obj.get(target).cloned().unwrap_or(Value::Null);
                    let new_val = match operation {
                        "replace" => Value::String(patch_content.to_string()),
                        "append" => match &existing {
                            Value::Array(arr) => {
                                let mut new_arr = arr.clone();
                                new_arr.push(Value::String(patch_content.to_string()));
                                Value::Array(new_arr)
                            }
                            Value::String(s) => {
                                Value::String(format!("{s}\n{patch_content}"))
                            }
                            _ => Value::String(patch_content.to_string()),
                        },
                        "prepend" => match &existing {
                            Value::String(s) => {
                                Value::String(format!("{patch_content}\n{s}"))
                            }
                            _ => Value::String(patch_content.to_string()),
                        },
                        op => return Err(AppError::Other(format!("unknown operation: {op}"))),
                    };
                    obj.insert(target.to_string(), new_val);

                    reconstruct_file(&fm_value, &body)?
                }

                "heading" => {
                    let (heading_line_end, section_start, section_end) =
                        find_heading_section(&file_content, target)
                            .ok_or_else(|| AppError::Other(format!("heading not found: {target}")))?;

                    let before_section = &file_content[..section_start];
                    let section_body = &file_content[section_start..section_end];
                    let after_section = &file_content[section_end..];

                    let _ = heading_line_end; // used implicitly via section_start

                    let new_section = match operation {
                        "replace" => patch_content.to_string(),
                        "append" => {
                            // Trim trailing newline from section for clean append
                            let trimmed = section_body.trim_end_matches('\n');
                            format!("{trimmed}\n{patch_content}")
                        }
                        "prepend" => format!("{patch_content}\n{section_body}"),
                        op => return Err(AppError::Other(format!("unknown operation: {op}"))),
                    };

                    format!("{before_section}{new_section}{after_section}")
                }

                t => return Err(AppError::Other(format!("unknown target_type: {t}"))),
            };

            let parent = real.parent()
                .ok_or_else(|| AppError::Other("path has no parent directory".into()))?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            tmp.write_all(new_content.as_bytes())?;
            tmp.persist(&real).map_err(|e| e.error)?;

            invalidate_cache(state, parent);
            Ok(json!([{ "type": "text", "text": "patch applied successfully" }]))
        }

        "list_tags" => {
            let search_root = match args["path"].as_str() {
                Some(path_str) => security::validate_path(root, path_str)?,
                None => root.clone(),
            };

            let mut tag_counts: HashMap<String, u64> = HashMap::new();

            for entry in ignore::WalkBuilder::new(&search_root).build() {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext != "md" {
                    continue;
                }

                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                let (fm_value, body) = parse_frontmatter(&content);

                // Frontmatter tags
                for tag in extract_frontmatter_tags(&fm_value) {
                    *tag_counts.entry(tag).or_insert(0) += 1;
                }

                // Inline #tags from body
                for tag in extract_inline_tags(&body) {
                    *tag_counts.entry(tag).or_insert(0) += 1;
                }
            }

            // Sort by count descending
            let mut sorted: Vec<(String, u64)> = tag_counts.into_iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

            let tags_obj: Value = sorted.into_iter()
                .map(|(k, v)| (k, Value::Number(v.into())))
                .collect::<serde_json::Map<_, _>>()
                .into();

            let text = serde_json::to_string_pretty(&json!({ "tags": tags_obj }))
                .map_err(|e| AppError::Other(e.to_string()))?;
            Ok(json!([{ "type": "text", "text": text }]))
        }

        _ => Err(AppError::Other(format!("unknown tool: {name}"))),
    }
}

// ── Tool definitions ───────────────────────────────────────────────────────────

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
        },
        {
            "name": "read_frontmatter",
            "description": "Read YAML frontmatter from a Markdown file. Returns an empty object if no frontmatter is present.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to root (must exist)" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "write_frontmatter",
            "description": "Set or update a single key in the YAML frontmatter of a Markdown file. Creates frontmatter if absent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":  { "type": "string", "description": "Path relative to root (must exist)" },
                    "key":   { "type": "string", "description": "Frontmatter key to set" },
                    "value": { "description": "Value to set (string, number, boolean, array, or object)" }
                },
                "required": ["path", "key", "value"]
            }
        },
        {
            "name": "patch_file",
            "description": "Patch a section of a Markdown file identified by a heading or frontmatter key. Supports append, prepend, and replace operations.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":        { "type": "string", "description": "Path relative to root (must exist)" },
                    "target_type": { "type": "string", "enum": ["heading", "frontmatter"], "description": "Whether the target is a heading or a frontmatter key" },
                    "target":      { "type": "string", "description": "Heading text (case-insensitive) or frontmatter key name" },
                    "operation":   { "type": "string", "enum": ["append", "prepend", "replace"], "description": "How to apply the patch" },
                    "content":     { "type": "string", "description": "Content to insert" }
                },
                "required": ["path", "target_type", "target", "operation", "content"]
            }
        },
        {
            "name": "list_tags",
            "description": "List all #tags (inline and frontmatter tags:) in Markdown files, sorted by frequency.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Optional path relative to root to search within (default: entire vault)" }
                },
                "required": []
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

    #[test]
    fn find_heading_section_basic() {
        let content = "# Hello\nsome content\n## Subheading\nmore\n# Other\nend";
        let result = find_heading_section(content, "Hello");
        assert!(result.is_some());
        let (_, start, end) = result.unwrap();
        let section = &content[start..end];
        assert!(section.contains("some content"));
        assert!(!section.contains("end"));
    }

    #[test]
    fn find_heading_section_to_eof() {
        let content = "# Hello\ncontent here\n";
        let result = find_heading_section(content, "Hello");
        assert!(result.is_some());
        let (_, start, end) = result.unwrap();
        let section = &content[start..end];
        assert!(section.contains("content here"));
    }

    #[test]
    fn find_heading_section_missing() {
        let content = "# Hello\ncontent\n";
        let result = find_heading_section(content, "NotHere");
        assert!(result.is_none());
    }

    #[test]
    fn parse_frontmatter_with_yaml() {
        let content = "---\ntitle: Test\ntags:\n  - rust\n---\nBody here";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm["title"], Value::String("Test".into()));
        assert!(body.contains("Body here"));
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "Just body content";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_object());
        assert_eq!(fm.as_object().unwrap().len(), 0);
        assert!(body.contains("Just body content"));
    }

    #[test]
    fn extract_inline_tags_basic() {
        let body = "This has #rust and #obsidian/daily tags";
        let tags = extract_inline_tags(body);
        assert!(tags.contains(&"rust".to_string()));
        assert!(tags.contains(&"obsidian/daily".to_string()));
    }

    #[test]
    fn extract_frontmatter_tags_list() {
        let fm = json!({ "tags": ["rust", "dev"] });
        let tags = extract_frontmatter_tags(&fm);
        assert_eq!(tags, vec!["rust", "dev"]);
    }

    #[test]
    fn extract_frontmatter_tags_string() {
        let fm = json!({ "tags": "rust, dev, obsidian" });
        let tags = extract_frontmatter_tags(&fm);
        assert_eq!(tags, vec!["rust", "dev", "obsidian"]);
    }
}
