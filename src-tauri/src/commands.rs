use chrono::Utc;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::{
    audit::AuditState,
    error::AppError,
    filesystem_server,
    models::{AuditEntry, Connection, ConnectionStatus, ConnectionType, ConnectionView, CreateConnectionRequest},
    obsidian_fs_server,
    port_manager,
    proxy_server,
    server_manager::ServerManager,
    store::StoreState,
};

type CmdResult<T> = Result<T, String>;

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

    let mut port_cfg = s.load_port_config().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4();
    let port = port_manager::assign_port(&mut port_cfg, &id.to_string(), req.port)
        .map_err(|e| e.to_string())?;
    s.save_port_config(&port_cfg).map_err(|e| e.to_string())?;

    let raw_path = req.mcp_path.unwrap_or_else(|| "/mcp".to_string());
    let mcp_path = if raw_path.starts_with('/') && !raw_path.contains(' ') && raw_path.len() <= 64 {
        raw_path
    } else {
        "/mcp".to_string()
    };
    let use_https = req.use_https.unwrap_or(false);

    let now = Utc::now();
    let conn = Connection {
        id,
        name:            req.name,
        connection_type: req.connection_type,
        port,
        root_paths:      req.root_paths,
        auth_config:     req.auth_config,
        status:          ConnectionStatus::Stopped,
        mcp_path,
        use_https,
        created_at:      now,
        updated_at:      now,
    };

    let mut conns = s.load_connections().map_err(|e| e.to_string())?;
    conns.push(conn.clone());
    s.save_connections(&conns).map_err(|e| e.to_string())?;

    Ok(ConnectionView::from(conn))
}

#[tauri::command]
pub fn delete_connection(
    store: State<StoreState>,
    server_mgr: State<ServerManager>,
    id: String,
) -> CmdResult<()> {
    // Stop server if running (ignore error — may already be stopped)
    let _ = server_mgr.shutdown(&id);

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

// ── Server lifecycle ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_server(
    app: AppHandle,
    store: State<'_, StoreState>,
    server_mgr: State<'_, ServerManager>,
    id: String,
) -> CmdResult<()> {
    let conn = {
        let s = store.0.lock().unwrap();
        s.load_connections()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id.to_string() == id)
            .ok_or_else(|| AppError::ConnectionNotFound.to_string())?
    };

    if server_mgr.is_running(&id) {
        return Err(AppError::ServerAlreadyRunning.to_string());
    }

    let _ = app.emit("connection-status-changed", json!({"id": &id, "status": "Starting"}));

    let router = match conn.connection_type {
        ConnectionType::Filesystem => {
            filesystem_server::create_router(conn.root_paths.clone(), conn.id, app.clone(), conn.mcp_path.clone())
        }
        ConnectionType::ObsidianFilesystem => {
            obsidian_fs_server::create_router(conn.root_paths.clone(), conn.id, app.clone(), conn.mcp_path.clone())
        }
        ConnectionType::RemoteProxy => {
            let auth = conn.auth_config
                .ok_or_else(|| "RemoteProxy connection missing auth_config".to_string())?;
            proxy_server::create_router(conn.id, auth, app.clone(), conn.mcp_path.clone())
                .map_err(|e| e.to_string())?
        }
    };

    let addr = format!("127.0.0.1:{}", conn.port);
    let listener = tokio::net::TcpListener::bind(&addr).await
        .map_err(|e| format!("bind {addr}: {e}"))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    server_mgr.register(&id, shutdown_tx).map_err(|e| e.to_string())?;

    let app_clone = app.clone();
    let id_clone = id.clone();
    let use_https = conn.use_https;

    if use_https {
        let data_dir = app.path().app_data_dir()
            .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?
            .join("certs");
        let (cert_pem, key_pem) = crate::tls::ensure_cert(&data_dir).map_err(|e| e.to_string())?;
        let tls_config = crate::tls::make_tls_config(&cert_pem, &key_pem).map_err(|e| e.to_string())?;
        let tls_acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls_config));

        tauri::async_runtime::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            loop {
                let (stream, _) = tokio::select! {
                    res = listener.accept() => match res {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!("TLS listener accept error: {e}");
                            break;
                        }
                    },
                    _ = &mut shutdown_rx => break,
                };
                let acceptor = tls_acceptor.clone();
                let tower_service = router.clone();
                tokio::spawn(async move {
                    if let Ok(tls_stream) = acceptor.accept(stream).await {
                        let io = hyper_util::rt::TokioIo::new(tls_stream);
                        let hyper_service = hyper::service::service_fn(move |req| {
                            use tower::Service;
                            let mut svc = tower_service.clone();
                            async move { svc.call(req).await }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, hyper_service)
                            .await;
                    }
                });
            }
        });
    } else {
        tauri::async_runtime::spawn(async move {
            let result = axum::serve(listener, router)
                .with_graceful_shutdown(async { let _ = shutdown_rx.await; })
                .await;

            if let Err(e) = result {
                tracing::error!("server {id_clone} crashed: {e}");
                if let Some(state) = app_clone.try_state::<StoreState>() {
                    let s = state.0.lock().unwrap();
                    if let Ok(mut conns) = s.load_connections() {
                        if let Some(c) = conns.iter_mut().find(|c| c.id.to_string() == id_clone) {
                            c.status = ConnectionStatus::Error(e.to_string());
                            c.updated_at = Utc::now();
                        }
                        let _ = s.save_connections(&conns);
                    }
                }
                let _ = app_clone.emit(
                    "connection-status-changed",
                    json!({"id": id_clone, "status": "Error", "message": e.to_string()}),
                );
            }
        });
    }

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
    app: AppHandle,
    store: State<'_, StoreState>,
    server_mgr: State<'_, ServerManager>,
    id: String,
) -> CmdResult<()> {
    server_mgr.shutdown(&id).map_err(|e| e.to_string())?;

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
pub fn get_server_status(server_mgr: State<ServerManager>, id: String) -> CmdResult<String> {
    Ok(if server_mgr.is_running(&id) { "Running".into() } else { "Stopped".into() })
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
