//! Minimal OAuth2 client_credentials server for protecting the public tunnel endpoint.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::store::OAuthCredentials;

// ── PKCE helper ───────────────────────────────────────────────────────────────

fn verify_pkce(verifier: &str, challenge: &str) -> bool {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash) == challenge
}

// ── State ─────────────────────────────────────────────────────────────────────

const TOKEN_TTL: Duration = Duration::from_secs(86400); // 24 hours

#[allow(dead_code)]
struct AuthCode {
    code_challenge: String,
    redirect_uri:   String,
    client_id:      String,
    expires_at:     Instant,
}

#[derive(Clone)]
pub struct IncomingAuthState {
    pub credentials: Arc<RwLock<OAuthCredentials>>,
    tokens:          Arc<RwLock<HashMap<String, Instant>>>,
    auth_codes:      Arc<RwLock<HashMap<String, AuthCode>>>,
    local_enabled:   Arc<AtomicBool>,  // user preference, persisted
    tunnel_active:   Arc<AtomicBool>,  // runtime, set by tunnel.rs
}

impl IncomingAuthState {
    pub fn new(credentials: OAuthCredentials, local_enabled: bool) -> Self {
        Self {
            credentials:   Arc::new(RwLock::new(credentials)),
            tokens:        Arc::new(RwLock::new(HashMap::new())),
            auth_codes:    Arc::new(RwLock::new(HashMap::new())),
            local_enabled: Arc::new(AtomicBool::new(local_enabled)),
            tunnel_active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Issue a new token, purge expired ones.
    pub async fn issue_token(&self) -> String {
        use rand::Rng;
        let token_bytes: [u8; 32] = rand::thread_rng().gen();
        let token = token_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let expiry = Instant::now() + TOKEN_TTL;

        let mut tokens = self.tokens.write().await;
        tokens.retain(|_, exp| *exp > Instant::now());
        tokens.insert(token.clone(), expiry);
        token
    }

    /// Check whether a Bearer token is currently valid.
    pub async fn validate_token(&self, token: &str) -> bool {
        let tokens = self.tokens.read().await;
        tokens.get(token).map(|exp| *exp > Instant::now()).unwrap_or(false)
    }

    /// Issue a single-use authorization code (10-minute TTL).
    pub async fn issue_auth_code(&self, client_id: &str, code_challenge: &str, redirect_uri: &str) -> String {
        use rand::Rng;
        let code_bytes: [u8; 16] = rand::thread_rng().gen();
        let code = code_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();

        let mut codes = self.auth_codes.write().await;
        codes.retain(|_, v| v.expires_at > Instant::now());
        codes.insert(code.clone(), AuthCode {
            code_challenge: code_challenge.to_string(),
            redirect_uri:   redirect_uri.to_string(),
            client_id:      client_id.to_string(),
            expires_at:     Instant::now() + Duration::from_secs(600),
        });
        code
    }

    /// Consume an auth code and return an access token if valid.
    pub async fn exchange_code(&self, code: &str, code_verifier: &str, redirect_uri: &str) -> Option<String> {
        let mut codes = self.auth_codes.write().await;
        let entry = codes.remove(code)?;
        if entry.expires_at < Instant::now() {
            return None;
        }
        if entry.redirect_uri != redirect_uri {
            return None;
        }
        if !verify_pkce(code_verifier, &entry.code_challenge) {
            return None;
        }
        drop(codes);
        Some(self.issue_token().await)
    }

    /// Replace credentials and invalidate all existing tokens.
    pub async fn rotate_credentials(&self, new_creds: OAuthCredentials) {
        let mut creds = self.credentials.write().await;
        *creds = new_creds;
        drop(creds);
        let mut tokens = self.tokens.write().await;
        tokens.clear();
    }

    pub fn should_authenticate(&self) -> bool {
        self.local_enabled.load(Ordering::Relaxed)
            || self.tunnel_active.load(Ordering::Relaxed)
    }

    pub fn set_local_enabled(&self, v: bool) {
        self.local_enabled.store(v, Ordering::Relaxed);
    }

    pub fn set_tunnel_active(&self, v: bool) {
        self.tunnel_active.store(v, Ordering::Relaxed);
    }

    pub fn is_tunnel_forced(&self) -> bool {
        self.tunnel_active.load(Ordering::Relaxed)
    }

    pub fn local_enabled(&self) -> bool {
        self.local_enabled.load(Ordering::Relaxed)
    }
}

pub struct IncomingAuthStateHandle(pub Arc<IncomingAuthState>);

// ── Token endpoint handler ─────────────────────────────────────────────────────

/// `POST /oauth/token` — client_credentials grant.
pub async fn token_endpoint(
    State(auth): State<Arc<IncomingAuthState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Accept both application/x-www-form-urlencoded and application/json
    let params = parse_body(&headers, &body);

    let grant_type = params.get("grant_type").map(|s| s.as_str()).unwrap_or("");
    let client_id  = params.get("client_id").map(|s| s.as_str()).unwrap_or("");

    match grant_type {
        "client_credentials" => {
            let client_secret = params.get("client_secret").map(|s| s.as_str()).unwrap_or("");
            let creds = auth.credentials.read().await;
            if client_id != creds.client_id || client_secret != creds.client_secret {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "invalid_client" })),
                ).into_response();
            }
            drop(creds);
            let token = auth.issue_token().await;
            Json(json!({
                "access_token": token,
                "token_type":   "bearer",
                "expires_in":   TOKEN_TTL.as_secs(),
            })).into_response()
        }
        "authorization_code" => {
            let code          = params.get("code").map(|s| s.as_str()).unwrap_or("");
            let code_verifier = params.get("code_verifier").map(|s| s.as_str()).unwrap_or("");
            let redirect_uri  = params.get("redirect_uri").map(|s| s.as_str()).unwrap_or("");

            match auth.exchange_code(code, code_verifier, redirect_uri).await {
                Some(token) => Json(json!({
                    "access_token": token,
                    "token_type":   "bearer",
                    "expires_in":   TOKEN_TTL.as_secs(),
                })).into_response(),
                None => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant" })),
                ).into_response(),
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported_grant_type" })),
        ).into_response(),
    }
}

/// Validate incoming Bearer token. Returns None if valid, Some(Response) if rejected.
pub async fn check_bearer(auth: &Arc<IncomingAuthState>, headers: &HeaderMap) -> Option<Response> {
    if !auth.should_authenticate() {
        return None; // auth disabled — pass through
    }

    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim());

    match token {
        Some(t) if auth.validate_token(t).await => None, // valid
        _ => Some((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "unauthorized",
                "message": "Valid Bearer token required. Obtain one via POST /oauth/token"
            })),
        ).into_response()),
    }
}

// ── Body parser ───────────────────────────────────────────────────────────────

fn parse_body(headers: &HeaderMap, body: &Bytes) -> HashMap<String, String> {
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if ct.contains("application/json") {
        if let Ok(v) = serde_json::from_slice::<Value>(body) {
            if let Some(obj) = v.as_object() {
                return obj
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect();
            }
        }
    }

    // Default: form-urlencoded
    let text = std::str::from_utf8(body).unwrap_or("");
    form_urlencoded_parse(text)
}

fn form_urlencoded_parse(input: &str) -> HashMap<String, String> {
    input
        .split('&')
        .filter_map(|pair| {
            let mut kv = pair.splitn(2, '=');
            let k = kv.next()?.to_string();
            let v = kv.next().unwrap_or("").to_string();
            Some((url_decode(&k), url_decode(&v)))
        })
        .collect()
}

fn url_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().unwrap_or('0');
            let h2 = chars.next().unwrap_or('0');
            if let Ok(b) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                result.push(b as char);
            }
        } else {
            result.push(c);
        }
    }
    result
}
