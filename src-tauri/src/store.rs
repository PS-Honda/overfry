use std::sync::{Arc, Mutex};

use serde_json::Value;
use tauri::AppHandle;
use tauri_plugin_store::{Store, StoreExt};

use crate::{error::AppError, models::{Connection, PortConfig}};

const KEY_CONNECTIONS:       &str = "connections";
const KEY_PORT_CONFIG:       &str = "port_config";
const KEY_OAUTH_CREDENTIALS: &str = "oauth_credentials";
const STORE_FILE:            &str = "overfry-store.json";

// ── OAuthCredentials ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthCredentials {
    pub client_id:     String,
    pub client_secret: String,
}

impl OAuthCredentials {
    pub fn generate() -> Self {
        use rand::Rng;
        let client_id     = uuid::Uuid::new_v4().to_string();
        let secret_bytes: [u8; 32] = rand::thread_rng().gen();
        let client_secret = secret_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        Self { client_id, client_secret }
    }
}

pub struct AppStore {
    app: AppHandle,
}

impl AppStore {
    pub fn new(app: AppHandle) -> Self { Self { app } }

    fn store(&self) -> Result<Arc<Store<tauri::Wry>>, AppError> {
        self.app
            .store(STORE_FILE)
            .map_err(|e| AppError::Store(e.to_string()))
    }

    pub fn load_connections(&self) -> Result<Vec<Connection>, AppError> {
        let store = self.store()?;
        match store.get(KEY_CONNECTIONS) {
            Some(Value::Array(arr)) => {
                let conns: Vec<Connection> = arr
                    .into_iter()
                    .filter_map(|v| serde_json::from_value(v).ok())
                    .collect();
                Ok(conns)
            }
            _ => Ok(vec![]),
        }
    }

    pub fn save_connections(&self, conns: &[Connection]) -> Result<(), AppError> {
        let store = self.store()?;
        let val = serde_json::to_value(conns).map_err(|e| AppError::Other(e.to_string()))?;
        store.set(KEY_CONNECTIONS, val);
        store.save().map_err(|e| AppError::Store(e.to_string()))
    }

    pub fn load_port_config(&self) -> Result<PortConfig, AppError> {
        let store = self.store()?;
        match store.get(KEY_PORT_CONFIG) {
            Some(v) => serde_json::from_value(v).map_err(|e| AppError::Other(e.to_string())),
            None     => Ok(PortConfig::default()),
        }
    }

    pub fn save_port_config(&self, cfg: &PortConfig) -> Result<(), AppError> {
        let store = self.store()?;
        let val = serde_json::to_value(cfg).map_err(|e| AppError::Other(e.to_string()))?;
        store.set(KEY_PORT_CONFIG, val);
        store.save().map_err(|e| AppError::Store(e.to_string()))
    }

    pub fn load_oauth_credentials(&self) -> Result<OAuthCredentials, AppError> {
        let store = self.store()?;
        if let Some(v) = store.get(KEY_OAUTH_CREDENTIALS) {
            return serde_json::from_value(v.clone()).map_err(|e| AppError::Other(e.to_string()));
        }
        Ok(OAuthCredentials::generate())
    }

    pub fn save_oauth_credentials(&self, creds: &OAuthCredentials) -> Result<(), AppError> {
        let store = self.store()?;
        let val = serde_json::to_value(creds).map_err(|e| AppError::Other(e.to_string()))?;
        store.set(KEY_OAUTH_CREDENTIALS, val);
        store.save().map_err(|e| AppError::Store(e.to_string()))
    }
}

// Thread-safe wrapper managed by Tauri
pub struct StoreState(pub Mutex<AppStore>);
