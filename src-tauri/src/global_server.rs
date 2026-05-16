//! Single-port global HTTP/HTTPS server.
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
use hyper_util::rt::TokioIo;
use tokio::sync::{oneshot, RwLock};
use tower::ServiceExt;

use crate::error::AppError;

pub const DEFAULT_PORT: u16 = 51552;

// ── Dispatch table ─────────────────────────────────────────────────────────────

/// mcp_path → Router for that connection.
pub type DispatchTable = Arc<RwLock<HashMap<String, Router>>>;

// ── GlobalServer ───────────────────────────────────────────────────────────────

pub struct GlobalServer {
    pub dispatch: DispatchTable,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: Option<tauri::async_runtime::JoinHandle<()>>,
    port: u16,
    tls_config: Option<Arc<rustls::ServerConfig>>,
}

impl GlobalServer {
    #[allow(dead_code)]
    pub fn new(port: u16) -> Self {
        Self {
            dispatch: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: None,
            handle: None,
            port,
            tls_config: None,
        }
    }

    pub fn new_with_tls(port: u16, tls: Option<Arc<rustls::ServerConfig>>) -> Self {
        Self {
            dispatch: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: None,
            handle: None,
            port,
            tls_config: tls,
        }
    }

    /// Returns true if this server is configured for HTTPS.
    pub fn is_https(&self) -> bool {
        self.tls_config.is_some()
    }

    /// Bind and start listening. Uses stored TLS config if present.
    pub async fn start(&mut self) -> Result<(), AppError> {
        self.start_with_tls(self.tls_config.clone()).await
    }

    /// Bind and start listening. Pass `Some(tls_config)` for HTTPS, `None` for HTTP.
    pub async fn start_with_tls(
        &mut self,
        tls_config: Option<Arc<rustls::ServerConfig>>,
    ) -> Result<(), AppError> {
        if self.shutdown_tx.is_some() {
            return Ok(());
        }

        // Store the config so restart_on_port can re-use it.
        self.tls_config = tls_config.clone();

        let dispatch = self.dispatch.clone();
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(AppError::Io)?;

        let (tx, rx) = oneshot::channel::<()>();
        let scheme = if tls_config.is_some() { "https" } else { "http" };
        tracing::info!(
            "global MCP server listening on {}://127.0.0.1:{}",
            scheme,
            self.port
        );

        let handle = if let Some(cfg) = tls_config {
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
            tauri::async_runtime::spawn(async move {
                tokio::pin!(rx);
                loop {
                    tokio::select! {
                        _ = &mut rx => break,
                        result = listener.accept() => {
                            let Ok((stream, _)) = result else { continue };
                            let acceptor = acceptor.clone();
                            let dispatch = dispatch.clone();
                            tokio::spawn(async move {
                                let Ok(tls_stream) = acceptor.accept(stream).await else { return };
                                let io = TokioIo::new(tls_stream);
                                let svc = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                                    let dispatch = dispatch.clone();
                                    async move {
                                        Ok::<Response<Body>, std::convert::Infallible>(
                                            dispatch_handler_inner(dispatch, req).await
                                        )
                                    }
                                });
                                let _ = hyper::server::conn::http1::Builder::new()
                                    .serve_connection(io, svc)
                                    .await;
                            });
                        }
                    }
                }
            })
        } else {
            // Plain HTTP via axum
            let app = Router::new()
                .fallback(dispatch_handler)
                .with_state(dispatch);
            tauri::async_runtime::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async { let _ = rx.await; })
                    .await
                    .ok();
            })
        };

        self.shutdown_tx = Some(tx);
        self.handle = Some(handle);
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

    /// Stop, change port, restart. Registered routes and TLS config are preserved.
    pub async fn restart_on_port(&mut self, new_port: u16) -> Result<(), AppError> {
        let tls = self.tls_config.clone();
        self.stop().await;
        self.port = new_port;
        self.start_with_tls(tls).await
    }

    #[allow(dead_code)]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.shutdown_tx.is_some()
    }

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

// ── Catch-all dispatcher (axum HTTP path) ─────────────────────────────────────

async fn dispatch_handler(
    State(dispatch): State<DispatchTable>,
    req: Request<Body>,
) -> Response {
    route_request(dispatch, req).await
}

/// Shared dispatch logic for a `Request<Body>` — called from both HTTP and HTTPS paths.
async fn route_request(dispatch: DispatchTable, req: Request<Body>) -> Response {
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

/// Entry point for TLS path — converts hyper::body::Incoming to axum Body first.
async fn dispatch_handler_inner(
    dispatch: DispatchTable,
    req: Request<hyper::body::Incoming>,
) -> Response {
    let (parts, incoming) = req.into_parts();
    let body = Body::new(incoming);
    let req = Request::from_parts(parts, body);
    route_request(dispatch, req).await
}
