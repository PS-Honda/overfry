mod audit;
mod commands;
mod cors;
mod error;
mod filesystem_server;
mod global_server;
mod incoming_auth;
mod models;
mod oauth;
mod obsidian_fs_server;
mod proxy_server;
mod security;
mod store;
mod tunnel;

use std::sync::{Arc, Mutex};

use audit::{AuditLog, AuditState};
use commands::*;
use global_server::GlobalServer;
use incoming_auth::IncomingAuthStateHandle;
use models::ConnectionStatus;
use store::{AppStore, StoreState};
use tauri::Manager;
use tokio::sync::Mutex as AsyncMutex;
use tunnel::{TunnelManager, TunnelState};

// ── App entry point ───────────────────────────────────────────────────────────

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
            // Audit log
            let log_dir = app.path().app_log_dir()?;
            std::fs::create_dir_all(&log_dir)?;
            let audit_path = log_dir.join("audit.ndjson");
            app.manage(AuditState(AuditLog::new(audit_path)));

            // Store
            let store = AppStore::new(app.handle().clone());
            app.manage(StoreState(Mutex::new(store)));

            // Global server port
            let global_port = {
                let s = app.state::<StoreState>();
                let s = s.0.lock().unwrap();
                s.load_port_config().unwrap_or_default().port
            };

            // Load or generate OAuth credentials for incoming tunnel auth
            let oauth_creds = {
                let s = app.state::<StoreState>();
                let s = s.0.lock().unwrap();
                let creds = s.load_oauth_credentials().unwrap_or_else(|_| {
                    crate::store::OAuthCredentials::generate()
                });
                let _ = s.save_oauth_credentials(&creds);
                creds
            };

            let auth_settings = {
                let s = app.state::<StoreState>();
                let s = s.0.lock().unwrap();
                s.load_auth_settings().unwrap_or_default()
            };

            let auth_state = Arc::new(incoming_auth::IncomingAuthState::new(oauth_creds, auth_settings.local_enabled));
            app.manage(IncomingAuthStateHandle(auth_state.clone()));

            // Build GlobalServer with auth
            let mut gs = GlobalServer::new(global_port);
            gs.set_auth(auth_state);
            app.manage(AsyncMutex::new(gs));

            app.manage(TunnelState(Mutex::new(TunnelManager::new())));

            // Async startup tasks
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Start global MCP server
                if let Some(gs_state) = handle.try_state::<AsyncMutex<GlobalServer>>() {
                    if let Err(e) = gs_state.lock().await.start().await {
                        tracing::error!("failed to start global server: {e}");
                    }
                }

                // Auto-start Cloudflare tunnel
                tunnel::tunnel_start(&handle, global_port);

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
            get_oauth_credentials,
            rotate_oauth_secret,
            get_auth_status,
            set_local_auth_enabled,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
