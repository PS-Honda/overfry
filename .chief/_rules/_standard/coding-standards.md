# Coding Standards

## General

- Keep functions small and single-purpose
- No `unwrap()` or `expect()` in production paths — use `?` or explicit match
- Errors must be handled, not silently swallowed
- No commented-out code in commits
- No `println!` in production — use `tracing::{info, warn, error, debug}`

## Naming

- Files: snake_case
- Rust types/traits: PascalCase
- Functions/variables: snake_case
- Tauri commands: snake_case (maps to camelCase in JS via `invoke`)

## Imports

- Group: std → third-party → crate-internal (`use crate::...`)
- No wildcard imports except `use serde::{Serialize, Deserialize}`
- Prefer `use` at module top, not inline

## Rust-Specific

- `PathBuf` for all paths — never `String` concatenation for paths
- `async fn` in axum handlers — always `Send + Sync`
- `Arc<T>` for shared state across tasks
- `tokio::sync::RwLock` for read-heavy shared state, `Mutex` for write-heavy
- All tauri::command functions must return `Result<T, String>` or `Result<T, AppError>`
- Bind HTTP servers to `127.0.0.1` only — never `0.0.0.0`

## Security Rules (NEVER BYPASS)

- Call `security::validate_path()` before every file read/write/list operation
- Atomic writes only: `NamedTempFile::new_in(parent).persist(target)`
- `delete_file` tool: always return `AppError::OperationNotPermitted("delete")`
- Origin header check in every axum router (DNS rebinding guard)

## Testing

- Unit tests in same file under `#[cfg(test)]` module
- Test file-naming for integration: `tests/<module>_integration.rs`
- Security tests MUST cover: null bytes, `..` traversal, symlink loops, UNC paths (Windows)

## Frontend (JS)

- ES modules only (`type="module"`)
- No framework — vanilla JS
- All Tauri API calls via `api.js` wrapper (no direct `__TAURI__` calls in components)
- No inline `<style>` — use `app.css`
- No CDN references — Bulma must be local (`assets/bulma-1.0.min.css`)
