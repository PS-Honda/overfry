# Milestone 4 Goal — Filesystem MCP (Write) + Security

## Objective

Add write tools to the Filesystem MCP server and harden all security layers. After this milestone, Claude can read and write files safely, and all attack vectors are covered.

## Success Criteria

- [ ] `write_file` tool: atomic write via `NamedTempFile::persist()`, path validated
- [ ] `append_file` tool: append content to existing file
- [ ] `move_file` tool: validates src+dst within roots, dst must not exist
- [ ] `search_files` tool: regex-based walk via `ignore` crate, respects .gitignore
- [ ] `delete_file` tool: always returns Error(-32603) "Deletion not supported"
- [ ] Audit log: every tool call → AuditEntry written to NDJSON + in-memory ring
- [ ] `audit-entry-added` Tauri event emitted → audit panel shows live entries
- [ ] Rate limiting: 100 req/s per connection (tower middleware)
- [ ] File size guard: read_file rejects files > 10MB
- [ ] Security unit tests pass: `../../etc/passwd`, null bytes, symlink loops, UNC paths

## Out of Scope

- RemoteProxy (Milestone 5)
- ObsidianFilesystem tools (Milestone 6)

## Notes

- `NamedTempFile::new_in(parent_dir)` — same volume ensures atomic persist on Windows
- `danger_accept_invalid_certs` only for Obsidian preset (127.0.0.1), not for generic proxy
