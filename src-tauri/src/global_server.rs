//! Single-port global HTTP server.
//!
//! All MCP connections share one axum server on a configurable port (default 51552).
//! Each connection registers its Router keyed by `mcp_path`; a catch-all fallback
//! dispatches incoming requests by URI path.

use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode},
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
    pub dispatch: DispatchTable,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: Option<tauri::async_runtime::JoinHandle<()>>,
    port: u16,
    auth_state: Option<Arc<crate::incoming_auth::IncomingAuthState>>,
}

impl GlobalServer {
    pub fn new(port: u16) -> Self {
        Self {
            dispatch: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: None,
            handle: None,
            port,
            auth_state: None,
        }
    }

    pub fn set_auth(&mut self, auth: Arc<crate::incoming_auth::IncomingAuthState>) {
        self.auth_state = Some(auth);
    }

    /// Bind and start listening (plain HTTP).
    pub async fn start(&mut self) -> Result<(), AppError> {
        if self.shutdown_tx.is_some() {
            return Ok(());
        }

        let dispatch = self.dispatch.clone();
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(AppError::Io)?;
        tracing::info!("global MCP server listening on http://127.0.0.1:{}", self.port);

        let (tx, rx) = oneshot::channel::<()>();

        let app = if let Some(auth) = self.auth_state.clone() {
            use axum::{middleware, routing::{get, post}};
            use crate::incoming_auth::token_endpoint;

            let system_routes = Router::new()
                .route("/.well-known/oauth-authorization-server", get(discovery_handler))
                .route("/oauth/authorize", get(authorize_handler))
                .route("/oauth/token", post(token_endpoint))
                .with_state(auth.clone());

            let mcp_routes = Router::new()
                .fallback(dispatch_handler)
                .with_state(dispatch)
                .layer(middleware::from_fn_with_state(
                    auth.clone(),
                    auth_middleware,
                ));

            system_routes.merge(mcp_routes)
        } else {
            Router::new()
                .fallback(dispatch_handler)
                .with_state(dispatch)
        };

        let handle = tauri::async_runtime::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { let _ = rx.await; })
                .await
                .ok();
        });

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

    /// Stop, change port, restart.
    pub async fn restart_on_port(&mut self, new_port: u16) -> Result<(), AppError> {
        self.stop().await;
        self.port = new_port;
        self.start().await
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

// ── Auth middleware ────────────────────────────────────────────────────────────

async fn auth_middleware(
    State(auth): State<Arc<crate::incoming_auth::IncomingAuthState>>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    if let Some(err) = crate::incoming_auth::check_bearer(&auth, req.headers()).await {
        return err;
    }
    next.run(req).await
}

// ── Catch-all dispatcher ──────────────────────────────────────────────────────

async fn dispatch_handler(
    State(dispatch): State<DispatchTable>,
    req: Request<Body>,
) -> Response {
    route_request(dispatch, req).await
}

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

// ── OAuth discovery + authorize handlers ──────────────────────────────────────

async fn discovery_handler(
    headers: HeaderMap,
    State(_auth): State<Arc<crate::incoming_auth::IncomingAuthState>>,
) -> axum::Json<serde_json::Value> {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:51552");
    let scheme = if host.contains('.') && !host.starts_with("127.") && !host.starts_with("localhost") {
        "https"
    } else {
        "http"
    };
    let base = format!("{scheme}://{host}");
    axum::Json(serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "client_credentials"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_post"]
    }))
}

#[derive(serde::Deserialize)]
struct AuthorizeParams {
    client_id:             String,
    redirect_uri:          String,
    state:                 Option<String>,
    code_challenge:        String,
    code_challenge_method: Option<String>,
    response_type:         String,
}

async fn authorize_handler(
    State(auth): State<Arc<crate::incoming_auth::IncomingAuthState>>,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    if params.response_type != "code" {
        return (StatusCode::BAD_REQUEST, "unsupported_response_type").into_response();
    }
    if let Some(ref method) = params.code_challenge_method {
        if method != "S256" {
            return (StatusCode::BAD_REQUEST, "unsupported_code_challenge_method").into_response();
        }
    }
    let creds = auth.credentials.read().await;
    if params.client_id != creds.client_id {
        return (StatusCode::UNAUTHORIZED, "invalid_client").into_response();
    }
    drop(creds);
    let code = auth.issue_auth_code(&params.client_id, &params.code_challenge, &params.redirect_uri).await;
    let mut location = format!("{}?code={}", params.redirect_uri, code);
    if let Some(state) = params.state {
        location.push_str(&format!("&state={state}"));
    }
    axum::response::Redirect::temporary(&location).into_response()
}
