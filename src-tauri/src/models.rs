use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Connection type ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum ConnectionType {
    Filesystem,
    ObsidianFilesystem,
    RemoteProxy,
}

// ── Connection status ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum ConnectionStatus {
    Starting,
    Running,
    Stopped,
    #[serde(rename = "Error")]
    Error(String),
}

impl Default for ConnectionStatus {
    fn default() -> Self { Self::Stopped }
}

// ── Auth method ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum AuthMethod {
    #[default]
    #[serde(rename = "token")]
    Token,
    #[serde(rename = "oauth")]
    OAuth,
}

// ── Auth config ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub base_url:         String,
    #[serde(default)]
    pub token:            String,
    #[serde(default)]
    pub extra_headers:    HashMap<String, String>,
    /// "outline" | "obsidian" | "notion" | "custom"
    #[serde(default)]
    pub preset:           Option<String>,

    // Auth method (defaults to Token for backward compat)
    #[serde(default)]
    pub auth_method:      AuthMethod,

    // OAuth fields — only used when auth_method = OAuth
    #[serde(default)]
    pub client_id:        String,
    #[serde(default)]
    pub client_secret:    String,
    #[serde(default)]
    pub oauth_auth_url:   String,
    #[serde(default)]
    pub oauth_token_url:  String,
    #[serde(default)]
    pub oauth_scopes:     String,

    // Stored after OAuth flow completes
    #[serde(default)]
    pub access_token:     String,
    #[serde(default)]
    pub refresh_token:    String,
}

// ── Connection ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id:              Uuid,
    pub name:            String,
    pub connection_type: ConnectionType,
    pub root_paths:      Vec<PathBuf>,
    pub auth_config:     Option<AuthConfig>,
    #[serde(default)]
    pub status:          ConnectionStatus,
    #[serde(default = "default_mcp_path")]
    pub mcp_path:        String,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

fn default_mcp_path() -> String { "/mcp".to_string() }

// ── Global port config ─────────────────────────────────────────────────────────

/// Single global port used by all connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortConfig {
    pub port: u16,
}

impl Default for PortConfig {
    fn default() -> Self { Self { port: crate::global_server::DEFAULT_PORT } }
}

// ── Audit ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AuditResult {
    Ok,
    Denied(String),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id:            Uuid,
    pub connection_id: Uuid,
    pub tool_name:     String,
    pub path:          Option<String>,
    pub timestamp:     DateTime<Utc>,
    pub session_id:    Option<String>,
    pub result:        AuditResult,
}

// ── Connection view (token-scrubbed projection for frontend) ───────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AuthConfigView {
    pub base_url:      String,
    pub preset:        Option<String>,
    pub auth_method:   AuthMethod,
    pub is_authorized: bool,
    // token / client_secret / access_token intentionally omitted
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionView {
    pub id:              Uuid,
    pub name:            String,
    pub connection_type: ConnectionType,
    pub root_paths:      Vec<PathBuf>,
    pub status:          ConnectionStatus,
    pub auth_config:     Option<AuthConfigView>,
    pub mcp_path:        String,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

impl From<Connection> for ConnectionView {
    fn from(c: Connection) -> Self {
        ConnectionView {
            id:              c.id,
            name:            c.name,
            connection_type: c.connection_type,
            root_paths:      c.root_paths,
            status:          c.status,
            auth_config:     c.auth_config.map(|a| AuthConfigView {
                base_url:      a.base_url,
                preset:        a.preset,
                auth_method:   a.auth_method,
                is_authorized: !a.access_token.is_empty(),
            }),
            mcp_path:        c.mcp_path,
            created_at:      c.created_at,
            updated_at:      c.updated_at,
        }
    }
}

// ── IPC request types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateConnectionRequest {
    pub name:            String,
    pub connection_type: ConnectionType,
    pub root_paths:      Vec<PathBuf>,
    pub auth_config:     Option<AuthConfig>,
    pub mcp_path:        Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> { Utc::now() }

    fn make_conn(connection_type: ConnectionType, auth_config: Option<AuthConfig>) -> Connection {
        Connection {
            id:              Uuid::new_v4(),
            name:            "Test".to_string(),
            connection_type,
            root_paths:      vec![],
            status:          ConnectionStatus::Stopped,
            auth_config,
            mcp_path:        "/mcp".to_string(),
            created_at:      now(),
            updated_at:      now(),
        }
    }

    fn make_auth(token: &str) -> AuthConfig {
        AuthConfig {
            base_url:       "https://example.com".to_string(),
            token:          token.to_string(),
            extra_headers:  HashMap::new(),
            preset:         None,
            auth_method:    AuthMethod::Token,
            client_id:      String::new(),
            client_secret:  String::new(),
            oauth_auth_url:  String::new(),
            oauth_token_url: String::new(),
            oauth_scopes:   String::new(),
            access_token:   String::new(),
            refresh_token:  String::new(),
        }
    }

    #[test]
    fn connection_view_omits_token() {
        let conn = make_conn(ConnectionType::RemoteProxy, Some(make_auth("super-secret-token")));
        let view = ConnectionView::from(conn);
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains("super-secret-token"),
            "token must not appear in ConnectionView JSON: {json}"
        );
        assert!(json.contains("https://example.com"), "base_url must be present");
    }

    #[test]
    fn connection_view_filesystem_no_auth() {
        let mut conn = make_conn(ConnectionType::Filesystem, None);
        conn.root_paths = vec![std::path::PathBuf::from("/tmp/vault")];
        conn.status = ConnectionStatus::Running;
        let view = ConnectionView::from(conn);
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("Test"));
        assert!(!json.contains("\"token\":"));
    }

    #[test]
    fn connection_view_auth_config_view_has_no_token_field() {
        let auth_view = AuthConfigView {
            base_url:      "https://outline.example.com".to_string(),
            preset:        Some("outline".to_string()),
            auth_method:   AuthMethod::Token,
            is_authorized: false,
        };
        let json = serde_json::to_string(&auth_view).unwrap();
        assert!(!json.contains("\"token\":"), "AuthConfigView must have no token field: {json}");
        assert!(json.contains("outline.example.com"));
    }

    #[test]
    fn serde_defaults_tolerate_missing_mcp_path() {
        // Old stored JSON without mcp_path should deserialize with "/mcp" default
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "Old Connection",
            "connection_type": "Filesystem",
            "root_paths": [],
            "status": "Stopped",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        }"#;
        let conn: Connection = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(conn.mcp_path, "/mcp");
    }

    #[test]
    fn serde_defaults_tolerate_old_port_field() {
        // Old stored JSON WITH port field should deserialize fine (serde ignores unknown fields)
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000002",
            "name": "Old Connection",
            "connection_type": "Filesystem",
            "port": 50001,
            "root_paths": [],
            "status": "Stopped",
            "mcp_path": "/my-vault",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        }"#;
        let conn: Connection = serde_json::from_str(json).expect("must deserialize with legacy port field");
        assert_eq!(conn.mcp_path, "/my-vault");
    }
}
