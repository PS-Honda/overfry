use std::{collections::HashMap, sync::Mutex};
use tokio::sync::oneshot;
use crate::error::AppError;

struct ServerHandle {
    shutdown_tx: oneshot::Sender<()>,
}

pub struct ServerManager(Mutex<HashMap<String, ServerHandle>>);

impl ServerManager {
    pub fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    pub fn register(&self, id: &str, shutdown_tx: oneshot::Sender<()>) -> Result<(), AppError> {
        let mut map = self.0.lock().unwrap();
        if map.contains_key(id) {
            return Err(AppError::ServerAlreadyRunning);
        }
        map.insert(id.to_string(), ServerHandle { shutdown_tx });
        Ok(())
    }

    pub fn shutdown(&self, id: &str) -> Result<(), AppError> {
        let handle = self.0.lock().unwrap().remove(id)
            .ok_or(AppError::ServerNotFound)?;
        let _ = handle.shutdown_tx.send(());
        Ok(())
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.0.lock().unwrap().contains_key(id)
    }
}
