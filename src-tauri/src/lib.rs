mod audit;
mod commands;
mod error;
mod filesystem_server;
mod models;
mod obsidian_fs_server;
mod port_manager;
mod proxy_server;
mod security;
mod server_manager;
mod store;
mod tls;

use std::sync::Mutex;

use audit::{AuditLog, AuditState};
use commands::*;
use models::ConnectionStatus;
use server_manager::ServerManager;
use store::{AppStore, StoreState};
use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize tracing (only in debug; release uses Tauri's logger)
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

            // Set up audit log in the app log directory
            let log_dir = app.path().app_log_dir()?;
            std::fs::create_dir_all(&log_dir)?;
            let audit_path = log_dir.join("audit.ndjson");
            app.manage(AuditState(AuditLog::new(audit_path)));

            let store = AppStore::new(app.handle().clone());
            app.manage(StoreState(Mutex::new(store)));
            app.manage(ServerManager::new());

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(state) = handle.try_state::<StoreState>() {
                    let s = state.0.lock().unwrap();

                    // Reset all connection statuses to Stopped (server not running after restart)
                    if let Ok(mut conns) = s.load_connections() {
                        let dirty = conns.iter().any(|c| c.status != ConnectionStatus::Stopped);
                        if dirty {
                            for c in conns.iter_mut() {
                                c.status = ConnectionStatus::Stopped;
                            }
                            let _ = s.save_connections(&conns);
                        }
                    }

                    // Validate port assignments; emit events for any that were reassigned
                    if let Ok(mut cfg) = s.load_port_config() {
                        let reassigned = port_manager::validate_and_reassign(&mut cfg);
                        if !reassigned.is_empty() {
                            let _ = s.save_port_config(&cfg);
                            for (id, old, new) in reassigned {
                                let _ = handle.emit(
                                    "port-reassigned",
                                    serde_json::json!({"id": id, "old_port": old, "new_port": new}),
                                );
                            }
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_connections,
            create_connection,
            delete_connection,
            suggest_port,
            start_server,
            stop_server,
            get_server_status,
            pick_folder,
            get_audit_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
