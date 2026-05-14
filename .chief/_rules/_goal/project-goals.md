# Global Project Goals

## Product Goal

Ship a cross-platform desktop app (Windows + macOS) that lets non-technical users expose local folders and remote APIs as MCP servers Claude can connect to via a single URL — no manual config file editing required.

## Quality

- All code passes `cargo clippy -- -D warnings` and `cargo check`
- No regressions: existing tests must not break between milestones
- Security layers never bypassed (see coding-standards.md)

## Safety

- Never expose credentials or secrets in code or logs
- All file paths validated before use (validate_path + symlink check)
- MCP servers bind to 127.0.0.1 only
- Audit log records every file operation Claude performs

## UX

- Non-IT users can add a connection and get a URL in under 2 minutes
- Status always visible on card (green/yellow/red dot)
- Copy URL is one click

## Delivery

- Milestone by milestone — no cross-milestone scope creep
- v0.1: Filesystem + RemoteProxy (includes Obsidian plugin via preset)
- v0.2: ObsidianFilesystem (no plugin needed)
- Keep changes minimal and focused per task
