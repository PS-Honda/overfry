use std::{
    collections::HashMap,
    convert::Infallible,
    io::Write,
    path::{Path, PathBuf},
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
use chrono::{Datelike, Utc};
use futures::stream;
use gray_matter::{engine::YAML, Matter};
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
                "serverInfo": { "name": "overfry-obsidian-filesystem", "version": "0.2.1" }
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
/// `heading_line_end` = index after the heading line (including its line ending).
/// `section_start`    = first content byte after heading (same as heading_line_end).
/// `section_end`      = byte index where next same-or-higher heading starts (or content.len()).
///
/// Uses `split_inclusive('\n')` so each line's `len()` includes its actual line-ending bytes
/// (`\n` or `\r\n`), fixing the CRLF offset bug and preventing EOF out-of-bounds access.
fn find_heading_section(content: &str, heading: &str) -> Option<(usize, usize, usize)> {
    let mut found_level: Option<u32> = None;
    let mut heading_line_end: Option<usize> = None;
    let mut section_start: Option<usize> = None;
    let mut byte_pos: usize = 0;

    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start_matches('#');
        let hashes = line.len() - trimmed.len();
        // A heading line starts with one or more '#' followed by a space
        // Use the raw bytes before the line ending to determine if it looks like a heading
        let line_stripped = line.trim_end_matches(['\n', '\r']);
        let trimmed_stripped = line_stripped.trim_start_matches('#');
        let hashes_stripped = line_stripped.len() - trimmed_stripped.len();
        let is_heading = hashes_stripped > 0 && trimmed_stripped.starts_with(' ');
        let text = trimmed_stripped.trim();

        if is_heading {
            if let Some(level) = found_level {
                if (hashes_stripped as u32) <= level {
                    // Next same-or-higher heading — section ends here
                    return Some((heading_line_end.unwrap(), section_start.unwrap(), byte_pos));
                }
            } else if text.eq_ignore_ascii_case(heading) {
                found_level = Some(hashes_stripped as u32);
                heading_line_end = Some(byte_pos + line.len()); // includes \n or \r\n
                section_start = Some(byte_pos + line.len());
            }
        }
        let _ = hashes; // suppress unused warning from the first computation
        byte_pos += line.len();
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

// ── MomentJS → chrono format translation ──────────────────────────────────────

/// Translate a MomentJS format string to a chrono format string.
/// Quarter handling must be done before calling this (replace `[Q]Q` with literal quarter digit).
fn momentjs_to_chrono(fmt: &str) -> String {
    fmt
        .replace("YYYY", "%Y")
        .replace("MM", "%m")
        .replace("DD", "%d")
        .replace("WW", "%W")   // ISO week 0-padded
        .replace("ww", "%U")   // week-of-year Sunday start
        .replace("ddd", "%a")
}

/// Resolve quarter number from month (1-indexed month → quarter 1..4).
fn quarter_from_month(month: u32) -> u32 {
    (month - 1) / 3 + 1
}

/// Format a NaiveDate using a MomentJS format string.
/// Handles quarter tokens `[Q]Q` specially before delegating to chrono.
fn format_date_momentjs(date: chrono::NaiveDate, fmt: &str) -> String {
    // Handle quarter replacement before chrono sees it
    let fmt_with_quarter = if fmt.contains("[Q]Q") || fmt.contains('Q') {
        let q = quarter_from_month(date.month());
        // Replace [Q]Q first (literal prefix), then bare Q
        fmt.replace("[Q]Q", &q.to_string())
           .replace('Q', &q.to_string())
    } else {
        fmt.to_string()
    };

    // Now translate remaining MomentJS tokens to chrono
    let chrono_fmt = momentjs_to_chrono(&fmt_with_quarter);
    date.format(&chrono_fmt).to_string()
}

// ── Vault outline builder ──────────────────────────────────────────────────────

/// Build vault outline tree. Returns JSON value with `{name, type, children?}` nodes.
fn build_outline(dir: &Path, current_depth: u32, max_depth: u32, count: &mut usize) -> Value {
    let name = dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());

    if current_depth >= max_depth || *count >= 300 {
        return json!({ "name": name, "type": "directory", "children": [] });
    }

    let mut children: Vec<Value> = Vec::new();

    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return json!({ "name": name, "type": "directory", "children": [] }),
    };

    let mut entries: Vec<_> = read_dir
        .filter_map(|e| e.ok())
        .collect();

    // Sort: directories first, then files, both alphabetically
    entries.sort_by(|a, b| {
        let a_is_dir = a.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let b_is_dir = b.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        b_is_dir.cmp(&a_is_dir)
            .then(a.file_name().cmp(&b.file_name()))
    });

    for entry in entries {
        if *count >= 300 {
            children.push(json!({ "name": "...truncated", "type": "truncated" }));
            break;
        }

        let entry_name = entry.file_name().to_string_lossy().into_owned();

        // Skip hidden files/dirs (starting with `.`) — Obsidian convention
        if entry_name.starts_with('.') {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        *count += 1;

        if file_type.is_dir() {
            let child = build_outline(&entry.path(), current_depth + 1, max_depth, count);
            children.push(child);
        } else if file_type.is_file() {
            let entry_path = entry.path();
            let ext = entry_path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if ext == "md" {
                // Try to extract title from frontmatter
                let title = extract_title_from_file(&entry_path);
                if let Some(t) = title {
                    children.push(json!({ "name": entry_name, "type": "note", "title": t }));
                } else {
                    children.push(json!({ "name": entry_name, "type": "note" }));
                }
            }
            // Skip non-markdown files silently
        }
    }

    json!({ "name": name, "type": "directory", "children": children })
}

/// Read first ~500 chars of a file and try to extract `title:` from YAML frontmatter.
fn extract_title_from_file(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    // Read 512 bytes to have margin for UTF-8 char boundary trimming
    let mut buf = [0u8; 512];
    let n = f.read(&mut buf).ok()?;
    let raw = &buf[..n];
    // Find a valid UTF-8 boundary at or before position 500
    let valid_end = (0..=raw.len().min(512))
        .rev()
        .find(|&i| std::str::from_utf8(&raw[..i]).is_ok())
        .unwrap_or(0);
    let snippet = std::str::from_utf8(&raw[..valid_end]).ok()?;

    // Look for frontmatter block
    if !snippet.starts_with("---") {
        return None;
    }

    let mut in_fm = false;
    for line in snippet.lines() {
        if line.trim() == "---" {
            if !in_fm {
                in_fm = true;
                continue;
            } else {
                break; // end of frontmatter
            }
        }
        if in_fm {
            // Match `title: value`
            if let Some(rest) = line.strip_prefix("title:") {
                let title = rest.trim().trim_matches('"').trim_matches('\'').to_string();
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }
    None
}

// ── Tool implementations ───────────────────────────────────────────────────────

fn call_tool(state: &FsState, name: &str, args: Value) -> Result<Value, AppError> {
    let root = state.root_paths.first()
        .ok_or_else(|| AppError::Other("no root paths configured".into()))?;

    match name {
        // ── Base filesystem tools ──────────────────────────────────────────

        "list_directory" => {
            let path_str = args["path"].as_str().unwrap_or(".");
            let limit = args["limit"].as_u64().unwrap_or(100).min(200) as usize;
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;

            let real = security::validate_path(root, path_str)?;

            // Collect ALL entries (cap at 1000)
            let mut all_entries: Vec<Value> = std::fs::read_dir(&real)?
                .filter_map(|e| e.ok())
                .take(1001)
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

            all_entries.sort_by(|a, b| {
                b["type"].as_str().cmp(&a["type"].as_str())
                    .then(a["name"].as_str().cmp(&b["name"].as_str()))
            });

            let truncated = all_entries.len() > 1000;
            if truncated {
                all_entries.truncate(1000);
            }

            let total = all_entries.len();
            let page: Vec<Value> = all_entries
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect();

            let mut result_obj = json!({
                "total": total,
                "offset": offset,
                "limit": limit,
                "entries": page,
            });

            if truncated {
                result_obj["truncated"] = json!(true);
            }

            let text = serde_json::to_string_pretty(&result_obj)
                .map_err(|e| AppError::Other(e.to_string()))?;
            let result = json!([{ "type": "text", "text": text }]);

            // Invalidate cache key based on path
            let cache_key = root.join(path_str.trim_start_matches(['/', '\\']));
            state.dir_cache.insert(cache_key, result.clone());
            Ok(result)
        }

        "read_file" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let start_line = args["start_line"].as_u64().map(|v| v as usize);
            let end_line   = args["end_line"].as_u64().map(|v| v as usize);

            let real = security::validate_path(root, path_str)?;
            let size = std::fs::metadata(&real)?.len();
            if size > MAX_FILE_SIZE {
                return Err(AppError::FileTooLarge(size));
            }
            let bytes = std::fs::read(&real)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| AppError::Other("binary file: cannot read as text".into()))?;

            if start_line.is_none() && end_line.is_none() {
                return Ok(json!([{ "type": "text", "text": text }]));
            }

            // Line-range mode
            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len();

            let start = start_line.unwrap_or(1);
            let end   = end_line.unwrap_or(total);

            if start < 1 {
                return Err(AppError::Other("start_line must be >= 1".into()));
            }
            if end < start {
                return Err(AppError::Other("end_line must be >= start_line".into()));
            }
            if end - start > 500 {
                return Err(AppError::Other("line range exceeds 500 lines".into()));
            }
            if start > total {
                return Err(AppError::Other(format!("start_line {start} exceeds file length {total}")));
            }

            let actual_end = end.min(total);
            let slice = lines[(start - 1)..actual_end].join("\n");
            let output = format!("{slice}\n\n[Lines {start}–{actual_end} of {total} total]");

            Ok(json!([{ "type": "text", "text": output }]))
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
            let context_lines = args["context_lines"].as_u64().unwrap_or(3).min(10) as usize;
            let max_files = args["max_files"].as_u64().unwrap_or(20).min(50) as usize;

            if query.len() > 256 {
                return Err(AppError::Other("query pattern exceeds 256 characters".into()));
            }

            let real = security::validate_path(root, path_str)?;

            let re = regex::RegexBuilder::new(query)
                .size_limit(1_000_000)
                .build()
                .map_err(|e| AppError::Other(format!("invalid regex: {e}")))?;

            let mut file_results: Vec<Value> = Vec::new();
            let mut total_chars: usize = 0;
            let mut files_searched: usize = 0;
            let mut total_matches: usize = 0;
            let mut files_skipped: usize = 0;
            let mut truncated = false;

            'walk: for entry in ignore::WalkBuilder::new(&real).build() {
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

                if file_results.len() >= max_files {
                    files_skipped += 1;
                    continue;
                }

                if total_chars > 8000 {
                    truncated = true;
                    files_skipped += 1;
                    continue;
                }

                // Skip files larger than 10 MB
                if entry.metadata().map(|m| m.len()).unwrap_or(0) > 10 * 1024 * 1024 {
                    files_skipped += 1;
                    continue;
                }

                let text = match std::fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                files_searched += 1;
                let all_lines: Vec<&str> = text.lines().collect();
                let mut matches: Vec<Value> = Vec::new();

                for (line_idx, line) in all_lines.iter().enumerate() {
                    if re.is_match(line) {
                        let before_start = line_idx.saturating_sub(context_lines);
                        let after_end = (line_idx + context_lines + 1).min(all_lines.len());

                        let context_before: Vec<String> = all_lines[before_start..line_idx]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();
                        let context_after: Vec<String> = all_lines[(line_idx + 1)..after_end]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();

                        matches.push(json!({
                            "line": line_idx + 1,
                            "text": line,
                            "context_before": context_before,
                            "context_after": context_after,
                        }));
                        total_matches += 1;
                    }
                }

                if !matches.is_empty() {
                    let file_entry = json!({
                        "file": path.to_string_lossy(),
                        "matches": matches,
                    });
                    let entry_str = serde_json::to_string(&file_entry).unwrap_or_default();
                    total_chars += entry_str.len();
                    file_results.push(file_entry);
                }

                if total_chars > 8000 {
                    truncated = true;
                    break 'walk;
                }
            }

            let mut result_obj = serde_json::Map::new();
            result_obj.insert("results".into(), Value::Array(file_results));
            if truncated {
                result_obj.insert("truncated".into(), json!(true));
                result_obj.insert("files_searched".into(), json!(files_searched));
                result_obj.insert("total_matches".into(), json!(total_matches));
                result_obj.insert("files_skipped".into(), json!(files_skipped));
            }

            let text = serde_json::to_string_pretty(&Value::Object(result_obj))
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

                // Skip files larger than 10 MB
                if entry.metadata().map(|m| m.len()).unwrap_or(0) > 10 * 1024 * 1024 {
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

        // ── New Obsidian v0.2.1 tools ──────────────────────────────────────

        "periodic_note_path" => {
            let period = args["period"].as_str()
                .ok_or_else(|| AppError::Other("period required".into()))?;
            let date_str = args["date"].as_str();

            // Parse or use today
            let date = if let Some(ds) = date_str {
                chrono::NaiveDate::parse_from_str(ds, "%Y-%m-%d")
                    .map_err(|e| AppError::Other(format!("invalid date '{ds}': {e}")))?
            } else {
                chrono::Local::now().date_naive()
            };

            // Try to read periodic-notes plugin config
            let plugin_config_path = root.join(".obsidian/plugins/periodic-notes/data.json");
            let daily_notes_path   = root.join(".obsidian/daily-notes.json");

            // (folder, format_string)
            let (folder, format_str) = if let Ok(raw) = std::fs::read_to_string(&plugin_config_path) {
                if let Ok(config) = serde_json::from_str::<Value>(&raw) {
                    let period_cfg = &config[period];
                    let folder = period_cfg["folder"].as_str().unwrap_or("").to_string();
                    let fmt    = period_cfg["format"].as_str().unwrap_or("").to_string();
                    let fmt = if fmt.is_empty() { default_format(period) } else { fmt };
                    (folder, fmt)
                } else {
                    // Fallback for daily only
                    daily_notes_fallback(period, &daily_notes_path)
                }
            } else if period == "daily" {
                daily_notes_fallback(period, &daily_notes_path)
            } else {
                ("".to_string(), default_format(period))
            };

            let formatted = format_date_momentjs(date, &format_str);
            let path_str = if folder.is_empty() {
                format!("{formatted}.md")
            } else {
                format!("{}/{formatted}.md", folder.trim_matches('/'))
            };

            // Validate the computed path doesn't escape vault root
            crate::security::validate_writable_path(root, &path_str)
                .map_err(|e| AppError::Other(format!("computed path escapes vault: {e}")))?;

            Ok(json!([{ "type": "text", "text": path_str }]))
        }

        "get_vault_outline" => {
            let depth = args["depth"].as_u64().unwrap_or(2).min(5) as u32;
            let mut count: usize = 0;
            let outline = build_outline(root, 0, depth, &mut count);
            let text = serde_json::to_string_pretty(&outline)
                .map_err(|e| AppError::Other(e.to_string()))?;
            Ok(json!([{ "type": "text", "text": text }]))
        }

        "get_note_structure" => {
            let path_str = args["path"].as_str()
                .ok_or_else(|| AppError::Other("path required".into()))?;
            let real = security::validate_path(root, path_str)?;

            let size = std::fs::metadata(&real)?.len();
            if size > MAX_FILE_SIZE {
                return Err(AppError::FileTooLarge(size));
            }

            let content = std::fs::read_to_string(&real)
                .map_err(|_| AppError::Other("cannot read file as text".into()))?;

            let total_lines = content.lines().count();

            // Parse frontmatter keys
            let frontmatter_keys = extract_frontmatter_keys(&content);

            // Parse headings
            let headings = extract_headings(&content);

            let result = json!({
                "frontmatter_keys": frontmatter_keys,
                "headings": headings,
                "total_lines": total_lines,
            });

            let text = serde_json::to_string(&result)
                .map_err(|e| AppError::Other(e.to_string()))?;
            Ok(json!([{ "type": "text", "text": text }]))
        }

        _ => Err(AppError::Other(format!("unknown tool: {name}"))),
    }
}

// ── Periodic note helpers ──────────────────────────────────────────────────────

fn default_format(period: &str) -> String {
    match period {
        "daily"     => "YYYY-MM-DD".to_string(),
        "weekly"    => "YYYY-[W]WW".to_string(),
        "monthly"   => "YYYY-MM".to_string(),
        "quarterly" => "YYYY-[Q]Q".to_string(),
        "yearly"    => "YYYY".to_string(),
        _           => "YYYY-MM-DD".to_string(),
    }
}

fn daily_notes_fallback(period: &str, path: &std::path::Path) -> (String, String) {
    if period == "daily" {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(config) = serde_json::from_str::<Value>(&raw) {
                let folder = config["folder"].as_str().unwrap_or("").to_string();
                let fmt    = config["format"].as_str().unwrap_or("").to_string();
                let fmt    = if fmt.is_empty() { default_format("daily") } else { fmt };
                return (folder, fmt);
            }
        }
    }
    ("".to_string(), default_format(period))
}

// ── Note structure helpers ─────────────────────────────────────────────────────

/// Extract frontmatter key names from content (keys in `---` block).
fn extract_frontmatter_keys(content: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut in_fm = false;
    let mut fm_started = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            if !fm_started {
                fm_started = true;
                in_fm = true;
                continue;
            } else if in_fm {
                break; // end of frontmatter
            }
        }
        if in_fm {
            // Match key: value (key is word chars or hyphen)
            if let Some(colon_pos) = line.find(':') {
                let key_part = &line[..colon_pos];
                let key = key_part.trim();
                // Only accept keys that are valid identifiers
                if !key.is_empty() && key.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)
                    && key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    keys.push(key.to_string());
                }
            }
        }
    }
    keys
}

/// Extract headings from markdown content.
fn extract_headings(content: &str) -> Vec<Value> {
    let mut headings = Vec::new();
    let mut in_fm = false;
    let mut fm_done = false;
    let mut fm_count = 0;

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // Skip frontmatter block
        if !fm_done {
            if trimmed == "---" {
                fm_count += 1;
                if fm_count == 1 { in_fm = true; continue; }
                if fm_count == 2 { in_fm = false; fm_done = true; continue; }
            }
            if in_fm { continue; }
        }

        // Match heading lines
        if line.starts_with('#') {
            let trimmed_hashes = line.trim_start_matches('#');
            let level = line.len() - trimmed_hashes.len();
            if trimmed_hashes.starts_with(' ') && (1..=6).contains(&level) {
                let heading_text = trimmed_hashes.trim().to_string();
                headings.push(json!({
                    "level": level,
                    "text": heading_text,
                    "line": line_idx + 1,
                }));
            }
        }
    }
    headings
}

// ── Tool definitions ───────────────────────────────────────────────────────────

fn tool_defs() -> Value {
    json!([
        {
            "name": "list_directory",
            "description": "List files and directories at a path within the root. Supports pagination via limit/offset. Returns total count.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":   { "type": "string",  "description": "Path relative to root (default: '.')" },
                    "limit":  { "type": "integer", "description": "Max entries to return (default 100, max 200)" },
                    "offset": { "type": "integer", "description": "Entries to skip for pagination (default 0)" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "read_file",
            "description": "Read text content of a file within the root. Optionally specify start_line/end_line for chunked reading (max 500 lines per call). Returns binary-file error if not UTF-8. Max 10 MB.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":       { "type": "string",  "description": "Path relative to root" },
                    "start_line": { "type": "integer", "description": "First line to read, 1-indexed (optional)" },
                    "end_line":   { "type": "integer", "description": "Last line to read, inclusive (optional)" }
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
            "description": "Search .md and .txt files for a regex pattern. Returns matching lines with surrounding context. Hard cap at 8000 chars of output.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query":         { "type": "string",  "description": "Regex pattern to search for" },
                    "path":          { "type": "string",  "description": "Path relative to root to search within (default: root)" },
                    "context_lines": { "type": "integer", "description": "Lines of context before/after each match (default 3, max 10)" },
                    "max_files":     { "type": "integer", "description": "Max files to return results from (default 20, max 50)" }
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
        },
        {
            "name": "periodic_note_path",
            "description": "Resolve the file path for a periodic note (daily/weekly/monthly/quarterly/yearly). Reads .obsidian plugin config for folder and format. Returns the relative path including .md extension.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "period": { "type": "string", "enum": ["daily", "weekly", "monthly", "quarterly", "yearly"], "description": "Period type" },
                    "date":   { "type": "string", "description": "ISO date string YYYY-MM-DD (default: today)" }
                },
                "required": ["period"]
            }
        },
        {
            "name": "get_vault_outline",
            "description": "Get a tree view of the vault structure with note titles extracted from frontmatter. Capped at 300 nodes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "depth": { "type": "integer", "description": "Max depth to traverse (default 2, max 5)" }
                },
                "required": []
            }
        },
        {
            "name": "get_note_structure",
            "description": "Get the heading outline and frontmatter key names of a note without loading full content into context.",
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

    // ── New tests for v0.2.1 ─────────────────────────────────────────────────

    #[test]
    fn momentjs_to_chrono_basic() {
        assert_eq!(momentjs_to_chrono("YYYY-MM-DD"), "%Y-%m-%d");
        assert_eq!(momentjs_to_chrono("YYYY-[W]WW"), "%Y-[W]%W");
        assert_eq!(momentjs_to_chrono("YYYY-MM"), "%Y-%m");
        assert_eq!(momentjs_to_chrono("YYYY"), "%Y");
    }

    #[test]
    fn format_date_daily() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap();
        let result = format_date_momentjs(date, "YYYY-MM-DD");
        assert_eq!(result, "2025-03-15");
    }

    #[test]
    fn format_date_monthly() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap();
        let result = format_date_momentjs(date, "YYYY-MM");
        assert_eq!(result, "2025-03");
    }

    #[test]
    fn format_date_yearly() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap();
        let result = format_date_momentjs(date, "YYYY");
        assert_eq!(result, "2025");
    }

    #[test]
    fn format_date_quarterly_q1() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        let result = format_date_momentjs(date, "YYYY-[Q]Q");
        assert_eq!(result, "2025-1");
    }

    #[test]
    fn format_date_quarterly_q4() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 12, 1).unwrap();
        let result = format_date_momentjs(date, "YYYY-[Q]Q");
        assert_eq!(result, "2025-4");
    }

    #[test]
    fn quarter_computation() {
        assert_eq!(quarter_from_month(1),  1);
        assert_eq!(quarter_from_month(3),  1);
        assert_eq!(quarter_from_month(4),  2);
        assert_eq!(quarter_from_month(6),  2);
        assert_eq!(quarter_from_month(7),  3);
        assert_eq!(quarter_from_month(9),  3);
        assert_eq!(quarter_from_month(10), 4);
        assert_eq!(quarter_from_month(12), 4);
    }

    #[test]
    fn default_formats_correct() {
        assert_eq!(default_format("daily"),     "YYYY-MM-DD");
        assert_eq!(default_format("weekly"),    "YYYY-[W]WW");
        assert_eq!(default_format("monthly"),   "YYYY-MM");
        assert_eq!(default_format("quarterly"), "YYYY-[Q]Q");
        assert_eq!(default_format("yearly"),    "YYYY");
    }

    #[test]
    fn extract_frontmatter_keys_basic() {
        let content = "---\ntitle: Test\ntags:\n  - rust\ndate: 2025-01-01\n---\nBody";
        let keys = extract_frontmatter_keys(content);
        assert!(keys.contains(&"title".to_string()));
        assert!(keys.contains(&"date".to_string()));
    }

    #[test]
    fn extract_headings_basic() {
        let content = "# Overview\n\nSome text\n\n## Section 1\n\nContent\n\n### Subsection\n";
        let headings = extract_headings(content);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[0]["level"], 1);
        assert_eq!(headings[0]["text"], "Overview");
        assert_eq!(headings[1]["level"], 2);
        assert_eq!(headings[2]["level"], 3);
    }

    #[test]
    fn extract_headings_skips_frontmatter() {
        let content = "---\ntitle: My Note\n---\n\n# Real Heading\n";
        let headings = extract_headings(content);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0]["text"], "Real Heading");
    }

    // ── Origin security regression tests ─────────────────────────────────────

    #[test]
    fn origin_ok_tauri_scheme_allowed_obsidian() {
        use axum::http::HeaderMap;
        let mut h = HeaderMap::new();
        h.insert("origin", "tauri://localhost".parse().unwrap());
        assert!(origin_ok(&h));
    }

    #[test]
    fn origin_ok_rejects_http_localhost_obsidian() {
        use axum::http::HeaderMap;
        let mut h = HeaderMap::new();
        h.insert("origin", "http://127.0.0.1:50000".parse().unwrap());
        assert!(!origin_ok(&h));
    }

    #[test]
    fn origin_ok_no_origin_allowed_obsidian() {
        use axum::http::HeaderMap;
        let h = HeaderMap::new();
        assert!(origin_ok(&h));
    }

    // ── find_heading_section CRLF regression tests ───────────────────────────

    #[test]
    fn find_heading_section_crlf() {
        // Section must work correctly with CRLF line endings.
        // "## Sub" is a child heading (level 2 < 1 is false), so it is part of
        // the "# Heading One" section — the section only ends at the next same-or-higher
        // heading (level <= 1).  The important regression check is that byte offsets
        // are valid UTF-8 slices and that content belonging to the section is present.
        let content = "# Heading One\r\nContent line\r\n## Sub\r\nMore\r\n# Other\r\n";
        let result = find_heading_section(content, "Heading One");
        assert!(result.is_some());
        let (_, start, end) = result.unwrap();
        assert!(start <= end);
        assert!(end <= content.len());
        let section = &content[start..end]; // must not panic (OOB or invalid UTF-8)
        assert!(section.contains("Content line"));
        // Section must end before the peer "# Other" heading
        assert!(!section.contains("# Other"));
    }

    #[test]
    fn find_heading_section_eof_no_trailing_newline() {
        // When file ends without trailing newline, must not panic (OOB bug regression)
        let content = "# Heading\nContent without trailing newline";
        let result = find_heading_section(content, "Heading");
        assert!(result.is_some());
        let (_, start, end) = result.unwrap();
        assert!(end <= content.len()); // must not exceed content length
        let _ = &content[start..end]; // must not panic
    }

    #[test]
    fn find_heading_section_crlf_byte_offsets() {
        // Verify byte offsets are correct with CRLF — section content must be valid UTF-8 slice
        let content = "# H1\r\nLine A\r\nLine B\r\n# H2\r\nOther\r\n";
        let result = find_heading_section(content, "H1");
        assert!(result.is_some());
        let (_, start, end) = result.unwrap();
        assert!(start <= end);
        assert!(end <= content.len());
        let section = &content[start..end];
        assert!(section.contains("Line A"));
        assert!(section.contains("Line B"));
        assert!(!section.contains("# H2"));
    }
}
