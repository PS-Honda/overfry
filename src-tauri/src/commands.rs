use std::sync::Mutex;

use chrono::Utc;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{AuditEntry, Connection, ConnectionStatus, ConnectionType, CreateConnectionRequest},
    port_manager,
    store::StoreState,
};

type CmdResult<T> = Result<T, String>;

// ── Connection CRUD ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_connections(store: State<StoreState>) -> CmdResult<Vec<Connection>> {
    let s = store.0.lock().unwrap();
    s.load_connections().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_connection(
    store: State<StoreState>,
    req: CreateConnectionRequest,
) -> CmdResult<Connection> {
    let s = store.0.lock().unwrap();

    let mut port_cfg = s.load_port_config().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4();
    let port = port_manager::assign_port(&mut port_cfg, &id.to_string(), req.port)
        .map_err(|e| e.to_string())?;
    s.save_port_config(&port_cfg).map_err(|e| e.to_string())?;

    let now = Utc::now();
    let conn = Connection {
        id,
        name:            req.name,
        connection_type: req.connection_type,
        port,
        root_paths:      req.root_paths,
        auth_config:     req.auth_config,
        status:          ConnectionStatus::Stopped,
        created_at:      now,
        updated_at:      now,
    };

    let mut conns = s.load_connections().map_err(|e| e.to_string())?;
    conns.push(conn.clone());
    s.save_connections(&conns).map_err(|e| e.to_string())?;

    Ok(conn)
}

#[tauri::command]
pub fn delete_connection(store: State<StoreState>, id: String) -> CmdResult<()> {
    let s = store.0.lock().unwrap();

    let mut conns = s.load_connections().map_err(|e| e.to_string())?;
    conns.retain(|c| c.id.to_string() != id);
    s.save_connections(&conns).map_err(|e| e.to_string())?;

    let mut port_cfg = s.load_port_config().map_err(|e| e.to_string())?;
    port_manager::release_port(&mut port_cfg, &id);
    s.save_port_config(&port_cfg).map_err(|e| e.to_string())
}

// ── Port ───────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn suggest_port(store: State<StoreState>, requested: Option<u16>) -> CmdResult<u16> {
    let s = store.0.lock().unwrap();
    let cfg = s.load_port_config().map_err(|e| e.to_string())?;
    Ok(port_manager::suggest_port(&cfg, requested))
}

// ── Server lifecycle (stubs — filled in Milestone 3) ──────────────────────────

#[tauri::command]
pub fn start_server(_id: String) -> CmdResult<String> {
    Err("MCP server not implemented yet (Milestone 3)".into())
}

#[tauri::command]
pub fn stop_server(_id: String) -> CmdResult<()> {
    Err("MCP server not implemented yet (Milestone 3)".into())
}

#[tauri::command]
pub fn get_server_status(_id: String) -> CmdResult<String> {
    Ok("Stopped".into())
}

// ── Folder picker ──────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let path = app.dialog().file().blocking_pick_folder();
    Ok(path.map(|p| p.to_string()))
}

// ── Audit log (stub — Milestone 4) ────────────────────────────────────────────

#[tauri::command]
pub fn get_audit_log(_limit: Option<usize>) -> CmdResult<Vec<AuditEntry>> {
    Ok(vec![])
}
