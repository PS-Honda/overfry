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

// ── Auth config ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub base_url:      String,
    pub token:         String,
    pub extra_headers: HashMap<String, String>,
    /// "outline" | "obsidian" | "custom"
    pub preset:        Option<String>,
}

// ── Connection ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id:              Uuid,
    pub name:            String,
    pub connection_type: ConnectionType,
    pub port:            u16,
    pub root_paths:      Vec<PathBuf>,
    pub auth_config:     Option<AuthConfig>,
    pub status:          ConnectionStatus,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

// ── Port config ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortConfig {
    pub base:        u16,
    pub max:         u16,
    pub assignments: HashMap<String, u16>, // Uuid as String → port
}

impl Default for PortConfig {
    fn default() -> Self {
        Self { base: 50000, max: 59999, assignments: HashMap::new() }
    }
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
    pub base_url: String,
    pub preset:   Option<String>,
    // token intentionally omitted
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionView {
    pub id:              Uuid,
    pub name:            String,
    pub connection_type: ConnectionType,
    pub port:            u16,
    pub root_paths:      Vec<PathBuf>,
    pub status:          ConnectionStatus,
    pub auth_config:     Option<AuthConfigView>,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

impl From<Connection> for ConnectionView {
    fn from(c: Connection) -> Self {
        ConnectionView {
            id:              c.id,
            name:            c.name,
            connection_type: c.connection_type,
            port:            c.port,
            root_paths:      c.root_paths,
            status:          c.status,
            auth_config:     c.auth_config.map(|a| AuthConfigView {
                base_url: a.base_url,
                preset:   a.preset,
            }),
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
    pub port:            Option<u16>,
    pub root_paths:      Vec<PathBuf>,
    pub auth_config:     Option<AuthConfig>,
}
