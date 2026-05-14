# TODO — Milestone 3: Filesystem MCP (Read)

- [ ] task-1: Implement security.rs (validate_path with soft-canonicalize, check_symlink_depth)
- [ ] task-2: Implement filesystem_server.rs axum router skeleton (Streamable HTTP, initialize handshake, tools/list)
- [ ] task-3: Implement list_directory tool (moka cache 30s TTL)
- [ ] task-4: Implement read_file tool (UTF-8 + base64 fallback, 10MB guard)
- [ ] task-5: Implement server_manager.rs (start/stop/status, tauri::async_runtime::spawn)
- [ ] task-6: Implement commands.rs start_server/stop_server/get_server_status + JS Start/Stop buttons
- [ ] task-7: Integration test — Claude Desktop connects, lists files, reads file content
