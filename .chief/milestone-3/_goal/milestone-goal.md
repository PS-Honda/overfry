# Milestone 3 Goal — Filesystem MCP (Read)

## Objective

Implement a working MCP server for local folders that exposes read-only tools: `list_directory` and `read_file`. The server must implement MCP Streamable HTTP (2025-03-26 spec). Claude Desktop must be able to connect via the generated URL and successfully call both tools.

## Success Criteria

- [ ] `start_server` Tauri command spawns axum MCP server on assigned port
- [ ] `stop_server` gracefully shuts down server
- [ ] MCP initialize handshake succeeds
- [ ] `tools/list` returns `list_directory` and `read_file`
- [ ] `list_directory` returns directory entries (cached, moka 30s TTL)
- [ ] `read_file` returns file content (UTF-8 or base64 for binary)
- [ ] Claude Desktop successfully connects and lists files
- [ ] Status dot on card updates to green (Running) / red (Error) via Tauri event
- [ ] `security::validate_path` called for every operation — path traversal rejected

## Out of Scope

- Write tools (write_file, append_file, move_file) — Milestone 4
- Audit log — Milestone 4
- Rate limiting — Milestone 4

## Notes

- Use `tauri::async_runtime::spawn` not `tokio::spawn`
- Bind to `127.0.0.1:<port>` only
- Origin header validation required from day 1 (DNS rebinding)
