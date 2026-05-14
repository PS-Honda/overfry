# Verification Rules

## Definition of Done

A task is complete when:
- [ ] Implementation matches task acceptance criteria
- [ ] `cargo check` passes (no compile errors)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] Relevant unit tests pass (`cargo test -p overfry`)
- [ ] No regressions in existing tests
- [ ] Security rules followed (validate_path, atomic writes, origin check)

## Verification Commands

```bash
# Type check + compile
cargo check

# Lint (warnings = errors)
cargo clippy -- -D warnings

# Tests
cargo test -p overfry

# Full Tauri dev (smoke test)
cargo tauri dev
```

## Integration Testing (tester-agent scope)

- MCP endpoint: `curl -X POST http://127.0.0.1:<port>/mcp -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}'`
- Claude Desktop: add connection URL to MCP settings, verify tools appear
- Audit log: verify entry written after each tool call
- Security: attempt `../../etc/passwd` path → must return PathTraversal error

## Milestone Release Checklist

- [ ] `tauri dev` runs without error on Windows
- [ ] Connection card shows correct status dot
- [ ] Copy URL button works
- [ ] Audit panel shows live entries
- [ ] Port reassignment notification appears when conflict detected
