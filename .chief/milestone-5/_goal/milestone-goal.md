# Milestone 5 Goal — Remote Proxy + v0.1 Release

## Objective

Implement the RemoteProxy connection type that forwards MCP tool calls to remote APIs (Outline, Notion, etc.) with injected auth headers. Include the "Obsidian Local REST API" preset in the modal. This completes v0.1 which ships Filesystem + RemoteProxy.

## Success Criteria

- [ ] `proxy_server.rs` axum router proxies JSON-RPC calls to upstream base URL
- [ ] Auth header (`Authorization: Bearer {token}`) injected on every request
- [ ] Outline preset: maps `list_documents`, `get_document`, `search_documents` tools
- [ ] "Obsidian Local REST API" preset: auto-fills `https://127.0.0.1:27123`, `danger_accept_invalid_certs(true)` scoped to this client only, maps `vault_*`, `search_*`, `periodic_*`, `command_*`, `tags_list`, `open_file`
- [ ] Modal step 2b: API type dropdown (Outline, Custom, Obsidian Local REST API)
- [ ] Connection with Obsidian plugin preset works end-to-end (plugin running → Claude sees vault tools)
- [ ] Audit log records proxy calls (tool_name from JSON-RPC method)
- [ ] v0.1 release: both Filesystem and RemoteProxy types stable

## Out of Scope

- ObsidianFilesystem (Milestone 6)
- macOS signing/installer (Milestone 7)

## Notes

- `danger_accept_invalid_certs(true)` ONLY on the reqwest client scoped to Obsidian preset connections — not global
- Generic RemoteProxy uses normal TLS
- Upstream errors (4xx/5xx) must be surfaced as MCP error responses, not silently dropped
