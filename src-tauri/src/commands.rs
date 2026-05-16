use chrono::Utc;
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    audit::AuditState,
    error::AppError,
    filesystem_server,
    global_server::GlobalServer,
    models::{
        AuditEntry, AuthConfig, AuthMethod, Connection, ConnectionStatus, ConnectionType,
        ConnectionView, CreateConnectionRequest, PortConfig,
    },
    oauth,
    obsidian_fs_server,
    proxy_server,
    store::StoreState,
};

type CmdResult<T> = Result<T, String>;

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Validate and normalise an mcp_path from user input.
fn normalise_mcp_path(raw: Option<String>, name: &str) -> String {
    let p = raw.unwrap_or_default();
    let p = p.trim().to_string();
    if p.starts_with('/') && !p.contains(' ') && p.len() <= 64 {
        p
    } else {
        // auto-slug from name
        let slug = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        format!("/mcp/{slug}")
    }
}

/// Merge secret fields from `existing` into `incoming` when incoming fields are blank.
/// This prevents an edit (which never sends secrets from the frontend) from wiping them.
fn merge_auth_secrets(existing: Option<&AuthConfig>, incoming: Option<AuthConfig>) -> Option<AuthConfig> {
    let mut inc = incoming?;
    if let Some(ex) = existing {
        if inc.token.is_empty() {
            inc.token = ex.token.clone();
        }
        if inc.client_secret.is_empty() {
            inc.client_secret = ex.client_secret.clone();
        }
        if inc.access_token.is_empty() {
            inc.access_token = ex.access_token.clone();
        }
        if inc.refresh_token.is_empty() {
            inc.refresh_token = ex.refresh_token.clone();
        }
    }
    Some(inc)
}

/// Return true if `path` is already taken by another connection.
/// Pass `exclude_id` to ignore a specific connection (used during updates).
fn path_is_taken(conns: &[Connection], path: &str, exclude_id: Option<Uuid>) -> bool {
    conns.iter().any(|c| {
        c.mcp_path == path && (exclude_id != Some(c.id))
    })
}

/// Build the axum Router for a connection (does NOT bind a port).
fn build_router(
    conn: &Connection,
    app: &AppHandle,
) -> CmdResult<axum::Router> {
    match conn.connection_type {
        ConnectionType::Filesystem => Ok(filesystem_server::create_router(
            conn.root_paths.clone(),
            conn.id,
            app.clone(),
            conn.mcp_path.clone(),
        )),
        ConnectionType::ObsidianFilesystem => Ok(obsidian_fs_server::create_router(
            conn.root_paths.clone(),
            conn.id,
            app.clone(),
            conn.mcp_path.clone(),
        )),
        ConnectionType::RemoteProxy => {
            let auth = conn.auth_config.clone()
                .ok_or("RemoteProxy connection missing auth_config")?;
            proxy_server::create_router(conn.id, auth, app.clone(), conn.mcp_path.clone())
                .map_err(|e| e.to_string())
        }
    }
}

// ── Connection CRUD ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_connections(store: State<StoreState>) -> CmdResult<Vec<ConnectionView>> {
    let s = store.0.lock().unwrap();
    let conns = s.load_connections().map_err(|e| e.to_string())?;
    Ok(conns.into_iter().map(ConnectionView::from).collect())
}

#[tauri::command]
pub fn create_connection(
    store: State<StoreState>,
    req: CreateConnectionRequest,
) -> CmdResult<ConnectionView> {
    let s = store.0.lock().unwrap();

    let mcp_path = normalise_mcp_path(req.mcp_path, &req.name);

    let mut conns = s.load_connections().map_err(|e| e.to_string())?;

    // Fix 2: enforce mcp_path uniqueness
    if path_is_taken(&conns, &mcp_path, None) {
        return Err(format!("MCP path '{mcp_path}' is already in use by another connection"));
    }

    let now = Utc::now();
    let conn = Connection {
        id:              Uuid::new_v4(),
        name:            req.name,
        connection_type: req.connection_type,
        root_paths:      req.root_paths,
        auth_config:     req.auth_config,
        status:          ConnectionStatus::Stopped,
        mcp_path,
        created_at:      now,
        updated_at:      now,
    };

    conns.push(conn.clone());
    s.save_connections(&conns).map_err(|e| e.to_string())?;

    Ok(ConnectionView::from(conn))
}

#[tauri::command]
pub async fn update_connection(
    app:    AppHandle,
    store:  State<'_, StoreState>,
    gs:     State<'_, Mutex<GlobalServer>>,
    id:     String,
    req:    CreateConnectionRequest,
) -> CmdResult<ConnectionView> {
    // Find existing
    let existing = {
        let s = store.0.lock().unwrap();
        s.load_connections().map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .ok_or_else(|| AppError::ConnectionNotFound.to_string())?
    };

    // Fix 6: hold the lock across is_registered + unregister to eliminate TOCTOU
    let was_running = {
        let gs_lock = gs.lock().await;
        let running = gs_lock.is_registered(&existing.mcp_path).await;
        if running {
            gs_lock.unregister(&existing.mcp_path).await;
        }
        running
    };

    let mcp_path = normalise_mcp_path(req.mcp_path, &req.name);

    // Fix 2: enforce mcp_path uniqueness (exclude this connection from the check)
    {
        let s = store.0.lock().unwrap();
        let conns = s.load_connections().map_err(|e| e.to_string())?;
        let parsed_id = Uuid::parse_str(&id).map_err(|e| e.to_string())?;
        if path_is_taken(&conns, &mcp_path, Some(parsed_id)) {
            return Err(format!("MCP path '{mcp_path}' is already in use by another connection"));
        }
    }

    // Fix 1: merge secrets from existing so the edit never wipes them
    let merged_auth = merge_auth_secrets(existing.auth_config.as_ref(), req.auth_config);

    let now = Utc::now();
    let updated = Connection {
        id:              existing.id,
        name:            req.name,
        connection_type: req.connection_type,
        root_paths:      req.root_paths,
        auth_config:     merged_auth,
        status:          ConnectionStatus::Stopped,
        mcp_path,
        created_at:      existing.created_at,
        updated_at:      now,
    };

    // Save
    {
        let s = store.0.lock().unwrap();
        let mut conns = s.load_connections().map_err(|e| e.to_string())?;
        if let Some(pos) = conns.iter().position(|c| c.id.to_string() == id) {
            conns[pos] = updated.clone();
        }
        s.save_connections(&conns).map_err(|e| e.to_string())?;
    }

    // Restart if was running
    if was_running {
        // Fix 5: emit Stopped event on build_router failure so frontend card updates
        let router = match build_router(&updated, &app) {
            Ok(r) => r,
            Err(e) => {
                let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Stopped"}));
                return Err(e);
            }
        };
        {
            let s  = store.0.lock().unwrap();
            let mut conns = s.load_connections().map_err(|e| e.to_string())?;
            if let Some(c) = conns.iter_mut().find(|c| c.id.to_string() == id) {
                c.status = ConnectionStatus::Running;
                c.updated_at = Utc::now();
            }
            s.save_connections(&conns).map_err(|e| e.to_string())?;
        }
        gs.lock().await.register(updated.mcp_path.clone(), router).await;
        let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Running"}));
    } else {
        let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Stopped"}));
    }

    // Re-fetch to get current status
    let s = store.0.lock().unwrap();
    let conns = s.load_connections().map_err(|e| e.to_string())?;
    let conn = conns.into_iter().find(|c| c.id.to_string() == id)
        .ok_or_else(|| AppError::ConnectionNotFound.to_string())?;
    Ok(ConnectionView::from(conn))
}

#[tauri::command]
pub async fn delete_connection(
    store: State<'_, StoreState>,
    gs:    State<'_, Mutex<GlobalServer>>,
    id:    String,
) -> CmdResult<()> {
    // Find mcp_path first for unregister
    let mcp_path = {
        let s = store.0.lock().unwrap();
        s.load_connections().map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .map(|c| c.mcp_path)
    };

    if let Some(path) = mcp_path {
        gs.lock().await.unregister(&path).await;
    }

    let s = store.0.lock().unwrap();
    let mut conns = s.load_connections().map_err(|e| e.to_string())?;
    conns.retain(|c| c.id.to_string() != id);
    s.save_connections(&conns).map_err(|e| e.to_string())
}

// ── Server lifecycle ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_server(
    app:   AppHandle,
    store: State<'_, StoreState>,
    gs:    State<'_, Mutex<GlobalServer>>,
    id:    String,
) -> CmdResult<()> {
    let conn = {
        let s = store.0.lock().unwrap();
        s.load_connections()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .ok_or_else(|| AppError::ConnectionNotFound.to_string())?
    };

    {
        let gs_lock = gs.lock().await;
        if gs_lock.is_registered(&conn.mcp_path).await {
            return Err(AppError::ServerAlreadyRunning.to_string());
        }
    }

    let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Starting"}));

    let router = build_router(&conn, &app)?;

    gs.lock().await.register(conn.mcp_path.clone(), router).await;

    // Update status in store
    {
        let s = store.0.lock().unwrap();
        let mut conns = s.load_connections().map_err(|e| e.to_string())?;
        if let Some(c) = conns.iter_mut().find(|c| c.id.to_string() == id) {
            c.status = ConnectionStatus::Running;
            c.updated_at = Utc::now();
        }
        s.save_connections(&conns).map_err(|e| e.to_string())?;
    }

    let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Running"}));
    Ok(())
}

#[tauri::command]
pub async fn stop_server(
    app:   AppHandle,
    store: State<'_, StoreState>,
    gs:    State<'_, Mutex<GlobalServer>>,
    id:    String,
) -> CmdResult<()> {
    let mcp_path = {
        let s = store.0.lock().unwrap();
        s.load_connections()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .map(|c| c.mcp_path)
            .ok_or_else(|| AppError::ConnectionNotFound.to_string())?
    };

    gs.lock().await.unregister(&mcp_path).await;

    {
        let s = store.0.lock().unwrap();
        let mut conns = s.load_connections().map_err(|e| e.to_string())?;
        if let Some(c) = conns.iter_mut().find(|c| c.id.to_string() == id) {
            c.status = ConnectionStatus::Stopped;
            c.updated_at = Utc::now();
        }
        s.save_connections(&conns).map_err(|e| e.to_string())?;
    }

    let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Stopped"}));
    Ok(())
}

#[tauri::command]
pub async fn get_server_status(
    store: State<'_, StoreState>,
    gs:    State<'_, Mutex<GlobalServer>>,
    id:    String,
) -> CmdResult<String> {
    // Fix 3: look up connection by id and check its specific route
    let mcp_path = {
        let s = store.0.lock().unwrap();
        s.load_connections().map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .ok_or_else(|| AppError::ConnectionNotFound.to_string())?
            .mcp_path
    };
    let running = gs.lock().await.is_registered(&mcp_path).await;
    Ok(if running { "Running".into() } else { "Stopped".into() })
}

// ── Global port ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_global_port(store: State<'_, StoreState>) -> CmdResult<u16> {
    let s = store.0.lock().unwrap();
    let cfg = s.load_port_config().map_err(|e| e.to_string())?;
    Ok(cfg.port)
}

#[tauri::command]
pub async fn set_global_port(
    app:   AppHandle,
    store: State<'_, StoreState>,
    gs:    State<'_, Mutex<GlobalServer>>,
    port:  u16,
) -> CmdResult<()> {
    if port < 1024 {
        return Err("Port must be >= 1024".to_string());
    }

    // Fix 4: restart first — only save if successful to avoid bricking on bad port
    gs.lock().await.restart_on_port(port).await.map_err(|e| e.to_string())?;

    {
        let s = store.0.lock().unwrap();
        s.save_port_config(&PortConfig { port }).map_err(|e| e.to_string())?;
    }

    let _ = app.emit("global-port-changed", json!({"port": port}));
    Ok(())
}

// ── OAuth flow ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_oauth_flow(
    app:           AppHandle,
    store:         State<'_, StoreState>,
    connection_id: String,
) -> CmdResult<()> {
    let conn = {
        let s = store.0.lock().unwrap();
        s.load_connections()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == connection_id)
            .ok_or_else(|| AppError::ConnectionNotFound.to_string())?
    };

    let auth = conn.auth_config
        .ok_or("Connection has no auth_config")?;

    if auth.auth_method != AuthMethod::OAuth {
        return Err("Connection is not using OAuth auth method".to_string());
    }

    let params = oauth::OAuthParams {
        auth_url:      &auth.oauth_auth_url,
        token_url:     &auth.oauth_token_url,
        client_id:     &auth.client_id,
        client_secret: &auth.client_secret,
        scopes:        &auth.oauth_scopes,
    };

    let (access_token, refresh_token) = oauth::run_oauth_flow(params)
        .await
        .map_err(|e| e.to_string())?;

    {
        let s = store.0.lock().unwrap();
        let mut conns = s.load_connections().map_err(|e| e.to_string())?;
        if let Some(c) = conns.iter_mut().find(|c| c.id.to_string() == connection_id) {
            if let Some(ref mut a) = c.auth_config {
                a.access_token  = access_token;
                a.refresh_token = refresh_token;
            }
            c.updated_at = Utc::now();
        }
        s.save_connections(&conns).map_err(|e| e.to_string())?;
    }

    let _ = app.emit("oauth-complete", json!({ "id": connection_id, "success": true }));
    Ok(())
}

// ── Folder picker ──────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let path = app.dialog().file().blocking_pick_folder();
    Ok(path.map(|p| p.to_string()))
}

// ── Audit log ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_audit_log(
    audit: State<'_, AuditState>,
    limit: Option<usize>,
) -> CmdResult<Vec<AuditEntry>> {
    Ok(audit.0.recent(limit.unwrap_or(200)))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::models::{
        AuthConfig, AuthMethod, Connection, ConnectionStatus, ConnectionType,
    };

    use super::{merge_auth_secrets, normalise_mcp_path, path_is_taken};

    fn make_auth(token: &str, client_secret: &str, access_token: &str, refresh_token: &str) -> AuthConfig {
        AuthConfig {
            base_url:        "https://example.com".to_string(),
            token:           token.to_string(),
            extra_headers:   HashMap::new(),
            preset:          None,
            auth_method:     AuthMethod::Token,
            client_id:       String::new(),
            client_secret:   client_secret.to_string(),
            oauth_auth_url:  String::new(),
            oauth_token_url: String::new(),
            oauth_scopes:    String::new(),
            access_token:    access_token.to_string(),
            refresh_token:   refresh_token.to_string(),
        }
    }

    fn make_conn(id: Uuid, mcp_path: &str) -> Connection {
        Connection {
            id,
            name:            "Test".to_string(),
            connection_type: ConnectionType::Filesystem,
            root_paths:      vec![],
            auth_config:     None,
            status:          ConnectionStatus::Stopped,
            mcp_path:        mcp_path.to_string(),
            created_at:      Utc::now(),
            updated_at:      Utc::now(),
        }
    }

    // ── merge_auth_secrets tests ───────────────────────────────────────────────

    #[test]
    fn merge_preserves_existing_token_when_incoming_blank() {
        let existing = make_auth("abc", "", "", "");
        let incoming = make_auth("", "", "", "");
        let merged = merge_auth_secrets(Some(&existing), Some(incoming)).unwrap();
        assert_eq!(merged.token, "abc");
    }

    #[test]
    fn merge_uses_incoming_token_when_provided() {
        let existing = make_auth("old", "", "", "");
        let incoming = make_auth("new", "", "", "");
        let merged = merge_auth_secrets(Some(&existing), Some(incoming)).unwrap();
        assert_eq!(merged.token, "new");
    }

    #[test]
    fn merge_preserves_refresh_token() {
        let existing = make_auth("", "", "", "tok");
        let incoming = make_auth("", "", "", "");
        let merged = merge_auth_secrets(Some(&existing), Some(incoming)).unwrap();
        assert_eq!(merged.refresh_token, "tok");
    }

    #[test]
    fn merge_handles_no_existing_auth() {
        let incoming = make_auth("hello", "sec", "at", "rt");
        let merged = merge_auth_secrets(None, Some(incoming.clone())).unwrap();
        assert_eq!(merged.token, "hello");
        assert_eq!(merged.client_secret, "sec");
        assert_eq!(merged.access_token, "at");
        assert_eq!(merged.refresh_token, "rt");
    }

    #[test]
    fn merge_returns_none_when_incoming_none() {
        let existing = make_auth("abc", "", "", "");
        let result = merge_auth_secrets(Some(&existing), None);
        assert!(result.is_none());
    }

    #[test]
    fn merge_preserves_client_secret() {
        let existing = make_auth("", "secret123", "", "");
        let incoming = make_auth("", "", "", "");
        let merged = merge_auth_secrets(Some(&existing), Some(incoming)).unwrap();
        assert_eq!(merged.client_secret, "secret123");
    }

    #[test]
    fn merge_preserves_access_token() {
        let existing = make_auth("", "", "at_existing", "");
        let incoming = make_auth("", "", "", "");
        let merged = merge_auth_secrets(Some(&existing), Some(incoming)).unwrap();
        assert_eq!(merged.access_token, "at_existing");
    }

    // ── path_is_taken tests ────────────────────────────────────────────────────

    #[test]
    fn path_is_taken_detects_collision() {
        let id = Uuid::new_v4();
        let conns = vec![make_conn(id, "/mcp/vault")];
        assert!(path_is_taken(&conns, "/mcp/vault", None));
    }

    #[test]
    fn path_is_taken_no_collision_different_path() {
        let id = Uuid::new_v4();
        let conns = vec![make_conn(id, "/mcp/vault")];
        assert!(!path_is_taken(&conns, "/mcp/other", None));
    }

    #[test]
    fn path_is_taken_excludes_own_id_on_update() {
        let id = Uuid::new_v4();
        let conns = vec![make_conn(id, "/mcp/vault")];
        // When editing the same connection, same path should not be a collision
        assert!(!path_is_taken(&conns, "/mcp/vault", Some(id)));
    }

    #[test]
    fn path_is_taken_collision_from_different_id() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let conns = vec![make_conn(id1, "/mcp/vault"), make_conn(id2, "/mcp/other")];
        // id2 wants /mcp/vault — already taken by id1
        assert!(path_is_taken(&conns, "/mcp/vault", Some(id2)));
    }

    // ── normalise_mcp_path tests ───────────────────────────────────────────────

    #[test]
    fn normalise_valid_path_passes_through() {
        let result = normalise_mcp_path(Some("/mcp/vault".to_string()), "My Vault");
        assert_eq!(result, "/mcp/vault");
    }

    #[test]
    fn normalise_generates_slug_from_name_when_empty() {
        let result = normalise_mcp_path(None, "My Vault");
        assert_eq!(result, "/mcp/my-vault");
    }

    #[test]
    fn normalise_slug_from_name_with_spaces_in_path() {
        // Path with space is invalid — should auto-slug from name
        let result = normalise_mcp_path(Some("/bad path".to_string()), "My Vault");
        assert_eq!(result, "/mcp/my-vault");
    }

    #[test]
    fn normalise_slug_no_leading_slash_generates_slug() {
        // Missing leading slash → auto-slug
        let result = normalise_mcp_path(Some("noslash".to_string()), "My Vault");
        assert_eq!(result, "/mcp/my-vault");
    }
}
