use std::{collections::HashMap, sync::Mutex};
use tokio::sync::oneshot;
use crate::error::AppError;

struct ServerHandle {
    shutdown_tx: oneshot::Sender<()>,
    join_handle: tauri::async_runtime::JoinHandle<()>,
}

pub struct ServerManager(Mutex<HashMap<String, ServerHandle>>);

impl ServerManager {
    pub fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    pub fn register(&self, id: String, shutdown_tx: oneshot::Sender<()>, join_handle: tauri::async_runtime::JoinHandle<()>) -> Result<(), AppError> {
        let mut map = self.0.lock().unwrap();
        if map.contains_key(&id) {
            return Err(AppError::ServerAlreadyRunning);
        }
        map.insert(id, ServerHandle { shutdown_tx, join_handle });
        Ok(())
    }

    pub async fn shutdown(&self, id: &str) -> Result<(), AppError> {
        let handle = self.0.lock().unwrap().remove(id)
            .ok_or(AppError::ServerNotFound)?;
        let _ = handle.shutdown_tx.send(());
        // Wait up to 2s for task to exit (ensures port is released)
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle.join_handle,
        ).await;
        Ok(())
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.0.lock().unwrap().contains_key(id)
    }
}
