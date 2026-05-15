use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::Mutex,
};

use crate::models::AuditEntry;

const RING_CAPACITY: usize = 500;

pub struct AuditLog {
    ring:     Mutex<VecDeque<AuditEntry>>,
    log_path: PathBuf,
}

impl AuditLog {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            ring:     Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            log_path,
        }
    }

    pub fn append(&self, entry: AuditEntry) {
        // Write NDJSON line
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{line}");
            }
        }
        // Push to ring
        let mut ring = self.ring.lock().unwrap();
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        let ring = self.ring.lock().unwrap();
        ring.iter().rev().take(limit).cloned().collect()
    }
}

/// Tauri managed state wrapper for `AuditLog`.
pub struct AuditState(pub AuditLog);
