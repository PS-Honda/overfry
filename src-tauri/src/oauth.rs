//! OAuth 2.0 Authorization Code + PKCE flow for RemoteProxy connections.
//!
//! Flow:
//! 1. Generate PKCE verifier + challenge
//! 2. Spin up temporary callback server on 127.0.0.1:59876
//! 3. Open browser to {auth_url}?... with PKCE params
//! 4. Receive GET /callback?code=AUTH_CODE
//! 5. Exchange code for tokens via POST to token_url
//! 6. Return (access_token, refresh_token)

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::error::AppError;

const CALLBACK_PORT: u16 = 59876;
const CALLBACK_PATH: &str = "/callback";

// ── PKCE ───────────────────────────────────────────────────────────────────────

pub struct PkceChallenge {
    pub verifier:  String,
    pub challenge: String,
}

pub fn generate_pkce() -> PkceChallenge {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(digest);

    PkceChallenge { verifier, challenge }
}

// ── Token response ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token:  String,
    #[serde(default)]
    pub refresh_token: String,
}

// ── Callback server ────────────────────────────────────────────────────────────

#[derive(Clone)]
struct CallbackState {
    tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<String>>>>,
}

#[derive(Deserialize)]
struct CallbackParams {
    code:  Option<String>,
    error: Option<String>,
}

async fn handle_callback(
    State(state): State<CallbackState>,
    Query(params): Query<CallbackParams>,
) -> impl IntoResponse {
    let mut lock = state.tx.lock().await;
    if let Some(tx) = lock.take() {
        if let Some(code) = params.code {
            let _ = tx.send(code);
            Html("<html><body><h2>Authorization successful!</h2><p>You can close this tab.</p></body></html>")
        } else {
            let err = params.error.unwrap_or_else(|| "unknown error".to_string());
            let _ = tx.send(format!("__error__{err}"));
            Html("<html><body><h2>Authorization failed.</h2><p>You can close this tab.</p></body></html>")
        }
    } else {
        Html("<html><body><p>Already handled.</p></body></html>")
    }
}

// ── Full OAuth flow ────────────────────────────────────────────────────────────

pub struct OAuthParams<'a> {
    pub auth_url:      &'a str,
    pub token_url:     &'a str,
    pub client_id:     &'a str,
    pub client_secret: &'a str,
    pub scopes:        &'a str,
}

pub async fn run_oauth_flow(params: OAuthParams<'_>) -> Result<(String, String), AppError> {
    let pkce = generate_pkce();
    let redirect_uri = format!("http://127.0.0.1:{CALLBACK_PORT}{CALLBACK_PATH}");

    // Build auth URL with query params (reqwest::Url handles encoding)
    let mut auth_url = reqwest::Url::parse(params.auth_url)
        .map_err(|e| AppError::Upstream(format!("invalid auth URL: {e}")))?;

    {
        let mut q = auth_url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", params.client_id);
        q.append_pair("redirect_uri", &redirect_uri);
        q.append_pair("code_challenge", &pkce.challenge);
        q.append_pair("code_challenge_method", "S256");
        if !params.scopes.is_empty() {
            q.append_pair("scope", params.scopes);
        }
    }

    // Set up callback server
    let (code_tx, code_rx) = oneshot::channel::<String>();
    let cb_state = CallbackState {
        tx: Arc::new(tokio::sync::Mutex::new(Some(code_tx))),
    };

    let router = Router::new()
        .route(CALLBACK_PATH, get(handle_callback))
        .with_state(cb_state);

    let bind_addr = format!("127.0.0.1:{CALLBACK_PORT}");
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| AppError::Upstream(format!("cannot bind callback port {CALLBACK_PORT}: {e}")))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async { let _ = shutdown_rx.await; })
            .await
            .ok();
    });

    // Open browser
    open::that(auth_url.as_str())
        .map_err(|e| AppError::Upstream(format!("cannot open browser: {e}")))?;

    // Wait for code — 2-minute timeout
    let code = tokio::time::timeout(std::time::Duration::from_secs(120), code_rx)
        .await
        .map_err(|_| AppError::Upstream("OAuth flow timed out (120 s)".to_string()))?
        .map_err(|_| AppError::Upstream("OAuth callback channel closed".to_string()))?;

    // Shut down callback server
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await;

    if let Some(err_msg) = code.strip_prefix("__error__") {
        return Err(AppError::Upstream(format!("OAuth error: {err_msg}")));
    }

    exchange_code(
        params.token_url,
        params.client_id,
        params.client_secret,
        &code,
        &redirect_uri,
        &pkce.verifier,
    )
    .await
}

async fn exchange_code(
    token_url:     &str,
    client_id:     &str,
    client_secret: &str,
    code:          &str,
    redirect_uri:  &str,
    verifier:      &str,
) -> Result<(String, String), AppError> {
    let client = reqwest::Client::new();
    let resp = client
        .post(token_url)
        .form(&[
            ("grant_type",    "authorization_code"),
            ("code",          code),
            ("redirect_uri",  redirect_uri),
            ("client_id",     client_id),
            ("client_secret", client_secret),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("token exchange request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Upstream(format!("token exchange failed ({status}): {body}")));
    }

    let token: TokenResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("token response parse failed: {e}")))?;

    Ok((token.access_token, token.refresh_token))
}

// ── Token refresh ──────────────────────────────────────────────────────────────

pub async fn refresh_access_token(
    token_url:     &str,
    client_id:     &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<(String, String), AppError> {
    let client = reqwest::Client::new();
    let resp = client
        .post(token_url)
        .form(&[
            ("grant_type",    "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id",     client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("refresh request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Upstream(format!("token refresh failed ({status}): {body}")));
    }

    let token: TokenResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("refresh response parse failed: {e}")))?;

    // Keep old refresh token if provider didn't rotate it
    let new_refresh = if token.refresh_token.is_empty() {
        refresh_token.to_string()
    } else {
        token.refresh_token
    };

    Ok((token.access_token, new_refresh))
}
