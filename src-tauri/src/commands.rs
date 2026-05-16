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
        AuditEntry, AuthMethod, Connection, ConnectionStatus, ConnectionType, ConnectionView,
        CreateConnectionRequest, PortConfig,
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

    let mut conns = s.load_connections().map_err(|e| e.to_string())?;
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

    let was_running = gs.lock().await.is_registered(&existing.mcp_path).await;

    // Stop if running
    if was_running {
        gs.lock().await.unregister(&existing.mcp_path).await;
    }

    let mcp_path = normalise_mcp_path(req.mcp_path, &req.name);
    let now = Utc::now();
    let updated = Connection {
        id:              existing.id,
        name:            req.name,
        connection_type: req.connection_type,
        root_paths:      req.root_paths,
        auth_config:     req.auth_config,
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
        let router = build_router(&updated, &app)?;
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
    gs:  State<'_, Mutex<GlobalServer>>,
    _id: String,
) -> CmdResult<String> {
    // Look up mcp_path by id from store isn't available here without store state;
    // frontend tracks status via events. Return based on global server being alive.
    let running = gs.lock().await.is_running();
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

    // Save to store
    {
        let s = store.0.lock().unwrap();
        s.save_port_config(&PortConfig { port }).map_err(|e| e.to_string())?;
    }

    // Restart server on new port
    gs.lock().await.restart_on_port(port).await.map_err(|e| e.to_string())?;

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
