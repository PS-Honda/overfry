use std::net::TcpListener;

use crate::{error::AppError, models::PortConfig};

/// Probe whether a TCP port is free on 127.0.0.1.
pub fn is_port_free(port: u16) -> bool {
    TcpListener::bind(format!("127.0.0.1:{port}")).is_ok()
}

/// Find the first free port in `base..=max` not already in `assignments`.
pub fn next_free_port(
    base: u16,
    max: u16,
    assignments: &std::collections::HashMap<String, u16>,
    exclude: Option<u16>,
) -> Option<u16> {
    let used: std::collections::HashSet<u16> = assignments.values().copied()
        .chain(exclude)
        .collect();
    (base..=max).find(|p| !used.contains(p) && is_port_free(*p))
}

/// Validate all stored port assignments on startup.
/// Returns list of (connection_id, old_port, new_port) for reassigned ports.
pub fn validate_and_reassign(cfg: &mut PortConfig) -> Vec<(String, u16, u16)> {
    let mut reassigned = vec![];
    let ids: Vec<String> = cfg.assignments.keys().cloned().collect();
    for id in ids {
        let stored = cfg.assignments[&id];
        if !is_port_free(stored) {
            // Port taken by OS process — find next free
            if let Some(new_port) = next_free_port(cfg.base, cfg.max, &cfg.assignments, Some(stored)) {
                cfg.assignments.insert(id.clone(), new_port);
                reassigned.push((id, stored, new_port));
            }
        }
    }
    reassigned
}

/// Assign a port to a new connection.
/// If `requested` is Some and free and not already assigned → use it.
/// Otherwise scan for next free port.
pub fn assign_port(
    cfg: &mut PortConfig,
    connection_id: &str,
    requested: Option<u16>,
) -> Result<u16, AppError> {
    if let Some(req) = requested {
        let already_used = cfg.assignments.values().any(|&p| p == req);
        if !already_used && is_port_free(req) && req >= cfg.base && req <= cfg.max {
            cfg.assignments.insert(connection_id.to_string(), req);
            return Ok(req);
        }
    }
    let port = next_free_port(cfg.base, cfg.max, &cfg.assignments, None)
        .ok_or_else(|| AppError::PortConflict("no free ports in range 50000-59999".into()))?;
    cfg.assignments.insert(connection_id.to_string(), port);
    Ok(port)
}

/// Release a port when connection is deleted.
pub fn release_port(cfg: &mut PortConfig, connection_id: &str) {
    cfg.assignments.remove(connection_id);
}

/// Suggest next free port (used by UI's port field validation).
pub fn suggest_port(cfg: &PortConfig, requested: Option<u16>) -> u16 {
    if let Some(req) = requested {
        let already_used = cfg.assignments.values().any(|&p| p == req);
        if !already_used && is_port_free(req) && req >= cfg.base && req <= cfg.max {
            return req;
        }
    }
    next_free_port(cfg.base, cfg.max, &cfg.assignments, None)
        .unwrap_or(cfg.base)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_cfg(assignments: Vec<(&str, u16)>) -> PortConfig {
        PortConfig {
            base: 50000, max: 59999,
            assignments: assignments.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    #[test]
    fn assign_uses_requested_when_free() {
        let mut cfg = make_cfg(vec![]);
        // Find a port we know is actually free on this machine before testing
        let free = next_free_port(50000, 59999, &cfg.assignments, None).unwrap();
        let port = assign_port(&mut cfg, "conn-1", Some(free)).unwrap();
        assert_eq!(port, free);
    }

    #[test]
    fn assign_skips_already_assigned() {
        let mut cfg = make_cfg(vec![("conn-1", 50000)]);
        // Port 50000 already assigned — should get next free
        let port = assign_port(&mut cfg, "conn-2", Some(50000)).unwrap();
        assert_ne!(port, 50000);
        assert!(port >= 50000 && port <= 59999);
    }

    #[test]
    fn release_removes_assignment() {
        let mut cfg = make_cfg(vec![("conn-1", 50001)]);
        release_port(&mut cfg, "conn-1");
        assert!(!cfg.assignments.contains_key("conn-1"));
    }

    #[test]
    fn suggest_returns_requested_when_free() {
        let cfg = make_cfg(vec![]);
        assert_eq!(suggest_port(&cfg, Some(50005)), 50005);
    }

    #[test]
    fn suggest_skips_assigned() {
        let cfg = make_cfg(vec![("x", 50000)]);
        let s = suggest_port(&cfg, Some(50000));
        assert_ne!(s, 50000);
    }
}
