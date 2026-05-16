mod audit;
mod commands;
mod error;
mod filesystem_server;
mod global_server;
mod models;
mod oauth;
mod obsidian_fs_server;
mod proxy_server;
mod security;
mod store;

use std::sync::Mutex;

use audit::{AuditLog, AuditState};
use commands::*;
use global_server::GlobalServer;
use models::ConnectionStatus;
use store::{AppStore, StoreState};
use tauri::Manager;
use tokio::sync::Mutex as AsyncMutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(debug_assertions)]
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Must install crypto provider before any TLS operation
            let _ = rustls::crypto::ring::default_provider().install_default();

            // Audit log
            let log_dir = app.path().app_log_dir()?;
            std::fs::create_dir_all(&log_dir)?;
            let audit_path = log_dir.join("audit.ndjson");
            app.manage(AuditState(AuditLog::new(audit_path)));

            // Store
            let store = AppStore::new(app.handle().clone());
            app.manage(StoreState(Mutex::new(store)));

            // Global server — read saved port, create (not started yet)
            let global_port = {
                let s = app.state::<StoreState>();
                let s = s.0.lock().unwrap();
                s.load_port_config().unwrap_or_default().port
            };
            app.manage(AsyncMutex::new(GlobalServer::new(global_port)));

            // Async startup tasks
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Start global MCP server
                if let Some(gs_state) = handle.try_state::<AsyncMutex<GlobalServer>>() {
                    if let Err(e) = gs_state.lock().await.start().await {
                        tracing::error!("failed to start global server: {e}");
                    }
                }

                // Reset all connection statuses to Stopped
                if let Some(state) = handle.try_state::<StoreState>() {
                    let s = state.0.lock().unwrap();
                    if let Ok(mut conns) = s.load_connections() {
                        let dirty = conns.iter().any(|c| c.status != ConnectionStatus::Stopped);
                        if dirty {
                            for c in conns.iter_mut() {
                                c.status = ConnectionStatus::Stopped;
                            }
                            let _ = s.save_connections(&conns);
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_connections,
            create_connection,
            update_connection,
            delete_connection,
            start_server,
            stop_server,
            get_server_status,
            get_global_port,
            set_global_port,
            start_oauth_flow,
            pick_folder,
            get_audit_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
