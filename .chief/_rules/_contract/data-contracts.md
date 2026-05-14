# Global Data Contracts

## MCP Transport

- Protocol: MCP Streamable HTTP (2025-03-26 spec)
- Endpoint: `POST /mcp` (RPC) + `GET /mcp` (SSE stream)
- Session: `Mcp-Session-Id` header (UUID)
- Origin validation: reject requests with non-localhost Origin

## Connection Model

```rust
Connection {
  id:              Uuid,
  name:            String,
  connection_type: ConnectionType,  // Filesystem | ObsidianFilesystem | RemoteProxy
  port:            u16,             // 50000–59999
  root_paths:      Vec<PathBuf>,    // empty for RemoteProxy
  auth_config:     Option<AuthConfig>,
  status:          ConnectionStatus,
  created_at:      DateTime<Utc>,
  updated_at:      DateTime<Utc>,
}
```

## Port Contract

- Range: 50000–59999
- Persisted in store as `HashMap<Uuid, u16>`
- Validated on every app startup
- User may override via modal input field

## Audit Contract

Every MCP tool call produces one AuditEntry:
```
{ id, connection_id, tool_name, path?, timestamp, session_id?, result: Ok|Denied|Error }
```
Written to `{app_data_dir}/audit.ndjson` (append-only) + in-memory ring (cap 500).

## Tauri IPC Events (Rust → JS)

- `connection-status-changed`: `{ id: string, status: ConnectionStatus }`
- `audit-entry-added`: `AuditEntry`
- `port-reassigned`: `{ id: string, old_port: number, new_port: number }`

## Filesystem Tool Policy

- `delete_file`: always Error(-32603) — never implemented
- `move_file`: dst must not exist; both paths must be within roots
- Max read size: 10MB per file
- All writes: atomic via `NamedTempFile::persist()`
