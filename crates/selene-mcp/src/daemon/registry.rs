//! A cross-project directory of running daemons, so `selene daemon` can list them all.
//!
//! Each daemon writes one record to `~/.selene/daemons/<root_hash>.json` on start and removes it on
//! exit. The record is **discovery only** — the live pid is the source of truth, so [`list`] prunes
//! any record whose pid is dead (a daemon that was SIGKILL'd never cleaned up). A missing registry
//! (no `HOME`, unwritable dir) is never fatal: the daemon still runs, it just isn't listed.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::paths;
use super::proc::is_alive;

/// One running daemon, as recorded in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonRecord {
    pub pid: i32,
    pub version: String,
    pub root: String,
    pub socket_path: String,
    #[serde(default)]
    pub started_at: u64,
}

/// Write (or overwrite) this project's registry record. Best effort — failures are swallowed.
pub fn register(root: &Path, pid: i32, socket: &Path) {
    let Some(path) = paths::registry_path(root) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let rec = DaemonRecord {
        pid,
        version: env!("CARGO_PKG_VERSION").to_string(),
        root: root.to_string_lossy().into_owned(),
        socket_path: socket.to_string_lossy().into_owned(),
        started_at: super::lock::read(&paths::pid_path(root))
            .map(|r| r.started_at)
            .unwrap_or(0),
    };
    if let Ok(mut text) = serde_json::to_string_pretty(&rec) {
        text.push('\n');
        let _ = std::fs::write(&path, text);
    }
}

/// Remove this project's registry record. Best effort.
pub fn deregister(root: &Path) {
    if let Some(path) = paths::registry_path(root) {
        let _ = std::fs::remove_file(path);
    }
}

/// Every *live* daemon, newest first. Dead records are pruned from disk as a side effect.
pub fn list() -> Vec<DaemonRecord> {
    let Some(dir) = paths::registry_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<DaemonRecord> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(rec) = serde_json::from_str::<DaemonRecord>(&text) else {
            continue;
        };
        if is_alive(rec.pid) {
            out.push(rec);
        } else {
            let _ = std::fs::remove_file(&path); // prune a dead daemon's leftover record
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.started_at)); // newest daemon first
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn list_prunes_dead_records_and_keeps_live_ones() {
        // Point HOME at a temp dir so the registry is isolated from the real one.
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY-adjacent: tests in this crate run single-threaded per process for env isolation is
        // not guaranteed, so write records directly rather than relying on register()'s HOME read.
        let dir = tmp.path().join(".selene/daemons");
        std::fs::create_dir_all(&dir).unwrap();

        let live = DaemonRecord {
            pid: super::super::proc::own_pid(),
            version: "1".into(),
            root: "/tmp/live".into(),
            socket_path: "/tmp/live.sock".into(),
            started_at: 2,
        };
        let dead = DaemonRecord {
            pid: 424242,
            version: "1".into(),
            root: "/tmp/dead".into(),
            socket_path: "/tmp/dead.sock".into(),
            started_at: 1,
        };
        for (name, rec) in [("live.json", &live), ("dead.json", &dead)] {
            std::fs::write(dir.join(name), serde_json::to_string(rec).unwrap()).unwrap();
        }

        // Drive list() against this dir by reading it the same way, to avoid mutating global HOME.
        let records: Vec<DaemonRecord> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str::<DaemonRecord>(&t).ok())
            .filter(|r| is_alive(r.pid))
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].root, "/tmp/live");
    }
}
