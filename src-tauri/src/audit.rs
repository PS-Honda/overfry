use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};
use crate::models::AuditEntry;

const RING_CAPACITY: usize = 500;
const MAX_LOG_SIZE:  u64   = 5 * 1024 * 1024;  // 5 MB
const CHECK_EVERY:   usize = 100;

pub struct AuditLog {
    ring:         Mutex<VecDeque<AuditEntry>>,
    log_path:     PathBuf,
    /// Using AtomicUsize so only one thread per CHECK_EVERY appends triggers rotation.
    append_count: AtomicUsize,
}

impl AuditLog {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            ring:         Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            log_path,
            append_count: AtomicUsize::new(0),
        }
    }

    pub fn append(&self, entry: AuditEntry) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{line}");
            }
        }

        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(entry);
        }

        // fetch_add returns the value *before* incrementing.
        // Only the thread that gets the exact multiple triggers rotation,
        // preventing two simultaneous threads from both rotating.
        let prev = self.append_count.fetch_add(1, Ordering::Relaxed);
        if prev % CHECK_EVERY == 0 {
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
        if let Err(e) = fs::remove_file(&p2) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("audit rotation: failed to remove .2: {e}");
            }
        }
        if let Err(e) = fs::rename(&p1, &p2) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("audit rotation: failed to rename .1 → .2: {e}");
            }
        }
        if let Err(e) = fs::rename(&self.log_path, &p1) {
            tracing::warn!("audit rotation: failed to rename current → .1: {e}");
        }
        // Next append will create fresh log_path
    }

    fn rotated_path(&self, n: u32) -> PathBuf {
        let stem = self.log_path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext  = self.log_path.extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.log_path.with_file_name(format!("{stem}.{n}.{ext}"))
    }

    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        match self.ring.lock() {
            Ok(ring) => ring.iter().rev().take(limit).cloned().collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Tauri managed state wrapper for `AuditLog`.
pub struct AuditState(pub AuditLog);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuditEntry, AuditResult};
    use uuid::Uuid;

    fn make_entry() -> AuditEntry {
        AuditEntry {
            id:            Uuid::new_v4(),
            connection_id: Uuid::new_v4(),
            tool_name:     "test_tool".to_string(),
            path:          None,
            timestamp:     chrono::Utc::now(),
            session_id:    None,
            result:        AuditResult::Ok,
        }
    }

    #[test]
    fn audit_append_no_panic_on_bad_path() {
        // AuditLog with a non-existent path must not panic — it should gracefully degrade
        let log = AuditLog::new(std::path::PathBuf::from(
            "/nonexistent/path/that/does/not/exist/audit.ndjson",
        ));
        // Must not panic even with bad path
        log.append(make_entry());
    }

    #[test]
    fn audit_append_count_increments() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.ndjson");
        let log = AuditLog::new(path);
        assert_eq!(log.append_count.load(Ordering::Relaxed), 0);
        log.append(make_entry());
        assert_eq!(log.append_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn audit_ring_stores_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.ndjson");
        let log = AuditLog::new(path);
        log.append(make_entry());
        log.append(make_entry());
        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn audit_ring_caps_at_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.ndjson");
        let log = AuditLog::new(path);
        // Fill ring beyond RING_CAPACITY (500); it should not grow past 500
        for _ in 0..=RING_CAPACITY {
            log.append(make_entry());
        }
        let recent = log.recent(1000);
        assert!(recent.len() <= RING_CAPACITY);
    }
}
