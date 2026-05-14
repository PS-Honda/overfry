# TODO — Milestone 4: Filesystem MCP (Write) + Security

- [ ] task-1: Implement write_file tool (NamedTempFile::persist atomic write)
- [ ] task-2: Implement append_file tool (OpenOptions::append)
- [ ] task-3: Implement move_file tool (validate both paths, cross-device fallback)
- [ ] task-4: Implement search_files tool (ignore crate walkdir + regex)
- [ ] task-5: Implement delete_file stub (always Error -32603)
- [ ] task-6: Implement audit.rs (NDJSON append, ring buffer cap 500, audit-entry-added event)
- [ ] task-7: Add Origin header middleware + rate limit middleware (tower) + file size guard
- [ ] task-8: Security unit tests (path traversal, null bytes, symlink loops, UNC paths)
