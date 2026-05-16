//! Single-port global HTTP server.
//!
//! All MCP connections share one axum server on a configurable port (default 51552).
//! Each connection registers its Router keyed by `mcp_path`; a catch-all fallback
//! dispatches incoming requests by URI path.

use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use tokio::sync::{oneshot, RwLock};
use tower::ServiceExt;

use crate::error::AppError;

pub const DEFAULT_PORT: u16 = 51552;

// ── Dispatch table ─────────────────────────────────────────────────────────────

/// mcp_path → Router for that connection.
pub type DispatchTable = Arc<RwLock<HashMap<String, Router>>>;

// ── GlobalServer ───────────────────────────────────────────────────────────────

pub struct GlobalServer {
    pub dispatch:    DispatchTable,
    shutdown_tx:     Option<oneshot::Sender<()>>,
    handle:          Option<tauri::async_runtime::JoinHandle<()>>,
    port:            u16,
}

impl GlobalServer {
    pub fn new(port: u16) -> Self {
        Self {
            dispatch:    Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: None,
            handle:      None,
            port,
        }
    }

    /// Bind and start listening. No-op if already running.
    pub async fn start(&mut self) -> Result<(), AppError> {
        if self.shutdown_tx.is_some() {
            return Ok(());
        }

        let dispatch = self.dispatch.clone();
        let addr     = format!("127.0.0.1:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(AppError::Io)?;

        let app = Router::new()
            .fallback(dispatch_handler)
            .with_state(dispatch);

        let (tx, rx) = oneshot::channel::<()>();
        let handle = tauri::async_runtime::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { let _ = rx.await; })
                .await
                .ok();
        });

        self.shutdown_tx = Some(tx);
        self.handle      = Some(handle);
        tracing::info!("global MCP server listening on http://127.0.0.1:{}", self.port);
        Ok(())
    }

    /// Gracefully stop. Waits up to 2 s for the task to exit.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), h).await;
        }
    }

    /// Stop, change port, restart. Registered routes are preserved.
    pub async fn restart_on_port(&mut self, new_port: u16) -> Result<(), AppError> {
        self.stop().await;
        self.port = new_port;
        self.start().await
    }

    #[allow(dead_code)]
    pub fn port(&self) -> u16 { self.port }

    pub fn is_running(&self) -> bool { self.shutdown_tx.is_some() }

    /// Register a connection's router (called on Start).
    pub async fn register(&self, path: String, router: Router) {
        self.dispatch.write().await.insert(path, router);
    }

    /// Remove a connection's router (called on Stop).
    pub async fn unregister(&self, path: &str) {
        self.dispatch.write().await.remove(path);
    }

    /// True if the path has an active router registered.
    pub async fn is_registered(&self, path: &str) -> bool {
        self.dispatch.read().await.contains_key(path)
    }
}

// ── Catch-all dispatcher ───────────────────────────────────────────────────────

async fn dispatch_handler(
    State(dispatch): State<DispatchTable>,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();
    let router = dispatch.read().await.get(&path).cloned();
    match router {
        Some(r) => r
            .oneshot(req)
            .await
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        None => {
            tracing::debug!("no handler registered for path: {path}");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}
