use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
};
use crate::models::AuditEntry;

const RING_CAPACITY: usize = 500;
const MAX_LOG_SIZE:  u64   = 5 * 1024 * 1024;  // 5 MB
const CHECK_EVERY:   usize = 100;

pub struct AuditLog {
    ring:         Mutex<VecDeque<AuditEntry>>,
    log_path:     PathBuf,
    append_count: Mutex<usize>,
}

impl AuditLog {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            ring:         Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            log_path,
            append_count: Mutex::new(0),
        }
    }

    pub fn append(&self, entry: AuditEntry) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{line}");
            }
        }
        let mut ring = self.ring.lock().unwrap();
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
        drop(ring);

        let mut count = self.append_count.lock().unwrap();
        *count += 1;
        if *count % CHECK_EVERY == 0 {
            drop(count);
            self.maybe_rotate();
        }
    }

    fn maybe_rotate(&self) {
        let size = fs::metadata(&self.log_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if size <= MAX_LOG_SIZE {
            return;
        }
        // Rotate: .2 → delete, .1 → .2, current → .1
        let p2 = self.rotated_path(2);
        let p1 = self.rotated_path(1);
        let _ = fs::remove_file(&p2);
        let _ = fs::rename(&p1, &p2);
        let _ = fs::rename(&self.log_path, &p1);
        // Next append will create fresh log_path
    }

    fn rotated_path(&self, n: u32) -> PathBuf {
        let stem = self.log_path.file_stem().unwrap_or_default().to_string_lossy();
        let ext  = self.log_path.extension().unwrap_or_default().to_string_lossy();
        self.log_path.with_file_name(format!("{stem}.{n}.{ext}"))
    }

    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        let ring = self.ring.lock().unwrap();
        ring.iter().rev().take(limit).cloned().collect()
    }
}

/// Tauri managed state wrapper for `AuditLog`.
pub struct AuditState(pub AuditLog);
