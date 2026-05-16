//! Minimal OAuth2 client_credentials server for protecting the public tunnel endpoint.

use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};

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

// ── State ─────────────────────────────────────────────────────────────────────

const TOKEN_TTL: Duration = Duration::from_secs(86400); // 24 hours

#[derive(Clone)]
pub struct IncomingAuthState {
    pub credentials: Arc<RwLock<OAuthCredentials>>,
    tokens: Arc<RwLock<HashMap<String, Instant>>>,
}

impl IncomingAuthState {
    pub fn new(credentials: OAuthCredentials) -> Self {
        Self {
            credentials: Arc::new(RwLock::new(credentials)),
            tokens: Arc::new(RwLock::new(HashMap::new())),
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

    /// Replace credentials and invalidate all existing tokens.
    pub async fn rotate_credentials(&self, new_creds: OAuthCredentials) {
        let mut creds = self.credentials.write().await;
        *creds = new_creds;
        drop(creds);
        let mut tokens = self.tokens.write().await;
        tokens.clear();
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

    let grant_type    = params.get("grant_type").map(|s| s.as_str()).unwrap_or("");
    let client_id     = params.get("client_id").map(|s| s.as_str()).unwrap_or("");
    let client_secret = params.get("client_secret").map(|s| s.as_str()).unwrap_or("");

    if grant_type != "client_credentials" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported_grant_type" })),
        ).into_response();
    }

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

/// Validate incoming Bearer token. Returns None if valid, Some(Response) if rejected.
pub async fn check_bearer(auth: &Arc<IncomingAuthState>, headers: &HeaderMap) -> Option<Response> {
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
