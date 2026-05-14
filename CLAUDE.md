# CLAUDE.md

## Overview

**Overfry** — Multi-Vault MCP Bridge. Cross-platform desktop app (Windows + macOS) built with Tauri v2 + Rust + Bulma CSS.

Each "connection" = 1 MCP server = 1 URL Claude pastes in.

**Connection types:**
- `Filesystem` — generic local folder → MCP file tools
- `ObsidianFilesystem` — Obsidian vault, no plugin needed (v0.2)
- `RemoteProxy` — remote API (Outline, Notion) or Obsidian Local REST API preset

**Transport:** MCP Streamable HTTP (2025-03-26 spec) — single `/mcp` endpoint.

---

## Rules Hierarchy Priority

1. **CLAUDE.md** (highest authority)
2. `.chief/_rules`
3. `.chief/milestone-X/_goal` (lowest authority)

---

## Development Commands

- Dev: `cargo tauri dev` (from repo root)
- Build: `cargo tauri build`
- Rust tests: `cargo test -p overfry`
- Check: `cargo check`
- Lint: `cargo clippy -- -D warnings`

---

## Architecture Overview

### Tech Stack

- **Backend:** Rust, Tauri v2, axum 0.8, tokio (full)
- **Frontend:** HTML + Bulma CSS 1.0 (local, no CDN), vanilla JS ES modules
- **MCP:** Streamable HTTP over axum — POST + GET on `/mcp` per connection
- **Persistence:** tauri-plugin-store (JSON)

### Key Architectural Patterns

- Each connection = independent axum HTTP server on `127.0.0.1:<port>` spawned via `tauri::async_runtime::spawn`
- Port range 50000–59999, persisted per connection UUID, validated on startup
- 5-layer security: Tauri scope → MCP Roots → soft-canonicalize → symlink depth → Origin header
- Atomic writes: `tempfile::NamedTempFile::persist()` (same-volume, cross-platform)
- Audit log: NDJSON append-only file + 500-entry in-memory ring

### Directory Structure

```
src-tauri/src/
  main.rs              — Tauri builder
  lib.rs               — mod declarations
  commands.rs          — #[tauri::command] IPC
  models.rs            — Connection, AuditEntry, PortConfig
  error.rs             — AppError (thiserror + IntoResponse + Serialize)
  server_manager.rs    — start/stop axum server tasks
  filesystem_server.rs — Filesystem MCP tools
  obsidian_fs_server.rs— ObsidianFilesystem tools (v0.2)
  proxy_server.rs      — RemoteProxy with header injection
  port_manager.rs      — port lifecycle
  security.rs          — validate_path, check_symlink_depth
  store.rs             — typed tauri-plugin-store wrapper
  audit.rs             — NDJSON log + ring buffer

src/
  index.html           — app shell
  assets/              — bulma-1.0.min.css (local)
  styles/app.css
  js/                  — main.js, api.js, dashboard.js, modal.js, audit.js
```

### Important Development Rules

- `PathBuf` everywhere — never string path concatenation
- Bind all MCP servers to `127.0.0.1` only, never `0.0.0.0`
- No `delete_file` implementation — stub returns Error(-32603)
- All file writes must be atomic via `NamedTempFile::persist()`
- validate_path called before every file operation (no exceptions)
- No `unwrap()` in production paths — use `?` or explicit error handling
- No CDN references in HTML — CSP blocks external sources
- Tauri events for live UI updates (no polling): `connection-status-changed`, `audit-entry-added`, `port-reassigned`
