# Milestone 2 Goal — CRUD + Port Manager

## Objective

Implement the full connection lifecycle (create/read/update/delete) backed by persistent storage, and the port assignment system. Users can add/remove connections via the modal and see them persist across app restarts. No MCP servers yet — connections are stored but not started.

## Success Criteria

- [ ] `create_connection` Tauri command persists connection to store
- [ ] `list_connections` returns stored connections on app start
- [ ] `delete_connection` removes from store and releases port
- [ ] `suggest_port` returns next free port in 50000–59999 range
- [ ] Port conflict detection: if stored port is OS-bound on startup, re-assign and emit `port-reassigned` event
- [ ] Modal form submits → new card appears in dashboard
- [ ] Cards survive app restart (data persisted)
- [ ] `cargo test -p overfry` passes port_manager unit tests

## Out of Scope

- No actual MCP server spawning yet
- No security (validate_path) — file system not touched

## Notes

- Use `tauri-plugin-store` for JSON persistence
- `port_manager.rs` must be unit-testable without Tauri running
