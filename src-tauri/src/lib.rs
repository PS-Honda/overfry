mod audit;
mod commands;
mod cors;
mod error;
mod filesystem_server;
mod global_server;
mod models;
mod oauth;
mod obsidian_fs_server;
mod proxy_server;
mod security;
mod store;
mod tls;

use std::sync::{Arc, Mutex};

use audit::{AuditLog, AuditState};
use commands::*;
use global_server::GlobalServer;
use models::ConnectionStatus;
use store::{AppStore, StoreState};
use tauri::Manager;
use tokio::sync::Mutex as AsyncMutex;

// ── TLS setup helper ──────────────────────────────────────────────────────────

fn setup_tls(data_dir: &std::path::Path) -> Result<rustls::ServerConfig, crate::error::AppError> {
    let (ca_cert, ca_key) = crate::tls::generate_ca()?;
    let (leaf_cert_pem, leaf_key_pem) = crate::tls::generate_leaf(&ca_cert, &ca_key)?;
    crate::tls::persist_certs(
        data_dir,
        &ca_cert.pem(),
        &ca_key.serialize_pem(),
        &leaf_cert_pem,
        &leaf_key_pem,
    )?;
    crate::tls::install_ca(data_dir)?;
    crate::tls::make_tls_config(&leaf_cert_pem, &leaf_key_pem)
}

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

            // Global server port
            let global_port = {
                let s = app.state::<StoreState>();
                let s = s.0.lock().unwrap();
                s.load_port_config().unwrap_or_default().port
            };

            // TLS setup — done synchronously in setup() so it can show a dialog.
            let tls_config: Option<Arc<rustls::ServerConfig>> = {
                let data_dir = app.path().app_data_dir()?;

                if let Some(certs) = crate::tls::load_certs(&data_dir) {
                    // Existing certs found — load them.
                    match crate::tls::make_tls_config(&certs.leaf_cert_pem, &certs.leaf_key_pem) {
                        Ok(cfg) => {
                            tracing::info!("loaded existing TLS certs from disk");
                            Some(Arc::new(cfg))
                        }
                        Err(e) => {
                            tracing::warn!("failed to load stored TLS certs: {e} — falling back to HTTP");
                            None
                        }
                    }
                } else {
                    // No certs — ask for consent.
                    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
                    let approved = app
                        .dialog()
                        .message(
                            "Overfry will generate a private Certificate Authority (CA) and \
                            install it into your system's certificate trust store.\n\n\
                            This allows Claude.ai and other MCP clients to connect securely \
                            over HTTPS.\n\n\
                            \u{2022} The CA is generated locally on your machine\n\
                            \u{2022} It only signs certificates for 127.0.0.1 and localhost\n\
                            \u{2022} It is never shared with any external server\n\n\
                            You may be prompted by your operating system to confirm this action.\n\n\
                            Proceed?",
                        )
                        .title("Enable Secure HTTPS Connection")
                        .buttons(MessageDialogButtons::OkCancel)
                        .blocking_show();

                    if approved {
                        match setup_tls(&data_dir) {
                            Ok(cfg) => {
                                tracing::info!("TLS setup complete — HTTPS enabled");
                                Some(Arc::new(cfg))
                            }
                            Err(e) => {
                                tracing::error!("TLS setup failed: {e} — falling back to HTTP");
                                let _ = app
                                    .dialog()
                                    .message(format!(
                                        "HTTPS setup failed: {e}\n\n\
                                        The app will use HTTP instead. \
                                        Claude.ai connections may not work."
                                    ))
                                    .title("HTTPS Setup Failed")
                                    .blocking_show();
                                None
                            }
                        }
                    } else {
                        tracing::info!("user declined TLS setup — using HTTP");
                        None
                    }
                }
            };

            app.manage(AsyncMutex::new(GlobalServer::new_with_tls(
                global_port,
                tls_config,
            )));

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
            get_tls_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
