//! Daemon election: one atomic file creation decides who becomes the daemon.
//!
//! The pidfile is the lock. To acquire it we write the full JSON record to a private temp file
//! (`<pid>.tmp`, mode 0600) and then **hard-link** it onto the pidfile. `hard_link` is atomic and
//! fails with `EEXIST` if the target already exists — so there is never an empty-file window and
//! never a lost race: exactly one of N concurrent launchers wins the link, the rest see `EEXIST`
//! and become proxies. On a filesystem without hard links (some network/ExFAT mounts) we fall back
//! to `O_EXCL` create, which is atomic there too.
//!
//! A dead daemon leaves a stale pidfile. [`clear_stale`] is a **compare-and-delete**: it removes
//! the pidfile only if it still names the pid we expect *and* that pid is dead — so two launchers
//! racing to clear the same corpse cannot delete a fresh daemon a third launcher just elected.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::proc::is_alive;

/// The record written to the pidfile — pretty JSON so a human can read it, plus a trailing newline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: i32,
    pub version: String,
    pub socket_path: String,
    /// Milliseconds since the Unix epoch. `0` when unknown (a legacy or hand-written file).
    #[serde(default)]
    pub started_at: u64,
}

/// The outcome of trying to become the daemon.
#[derive(Debug)]
pub enum Acquire {
    /// We created the pidfile — we are the daemon.
    Acquired,
    /// Someone else holds it. The record, if it parsed (may be `None` for a corrupt/racing file).
    Taken(Option<PidRecord>),
}

/// Try to become the daemon by atomically creating `pid_path` with `record`.
pub fn acquire(pid_path: &Path, record: &PidRecord) -> std::io::Result<Acquire> {
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serialize(record);

    // Write the record to a private temp file, then hard-link it into place (atomic + exclusive).
    let tmp = pid_path.with_extension(format!("pid.{}.tmp", record.pid));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    let link_result = std::fs::hard_link(&tmp, pid_path);
    // The temp file has done its job either way.
    let _ = std::fs::remove_file(&tmp);

    match link_result {
        Ok(()) => Ok(Acquire::Acquired),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(Acquire::Taken(read(pid_path))),
        // No-hardlink filesystem: fall back to an atomic O_EXCL create straight onto the pidfile.
        Err(_) => match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pid_path)
        {
            Ok(mut f) => {
                f.write_all(text.as_bytes())?;
                Ok(Acquire::Acquired)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(Acquire::Taken(read(pid_path)))
            }
            Err(e) => Err(e),
        },
    }
}

/// Read and parse the pidfile, or `None` if it is absent or unparseable. Tolerates a bare decimal
/// pid (a legacy/hand-written file) so a corrupt record never wedges election — it reads as a
/// record with unknown version, which the caller treats as "clear if dead".
pub fn read(pid_path: &Path) -> Option<PidRecord> {
    let text = std::fs::read_to_string(pid_path).ok()?;
    if let Ok(rec) = serde_json::from_str::<PidRecord>(&text) {
        return Some(rec);
    }
    let pid: i32 = text.trim().parse().ok()?;
    Some(PidRecord { pid, version: "unknown".into(), socket_path: String::new(), started_at: 0 })
}

/// Remove the pidfile **only if** it still names `expected_dead_pid` and that pid is dead. Returns
/// whether it was cleared. A pid that differs (a fresh daemon) or is alive is left untouched.
pub fn clear_stale(pid_path: &Path, expected_dead_pid: i32) -> bool {
    match read(pid_path) {
        Some(rec) if rec.pid == expected_dead_pid && !is_alive(rec.pid) => {
            std::fs::remove_file(pid_path).is_ok()
        }
        _ => false,
    }
}

/// Release the pidfile we own — remove it only if it still names `own_pid` (never delete a
/// successor's file if ours was already cleared and replaced).
pub fn release(pid_path: &Path, own_pid: i32) -> bool {
    match read(pid_path) {
        Some(rec) if rec.pid == own_pid => std::fs::remove_file(pid_path).is_ok(),
        _ => false,
    }
}

fn serialize(record: &PidRecord) -> String {
    let mut s = serde_json::to_string_pretty(record).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::proc::own_pid;
    use super::*;

    fn rec(pid: i32) -> PidRecord {
        PidRecord {
            pid,
            version: "9.9.9".into(),
            socket_path: "/tmp/x.sock".into(),
            started_at: 1,
        }
    }

    #[test]
    fn first_acquire_wins_second_sees_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join(".selene/daemon.pid");
        assert!(matches!(acquire(&pid, &rec(own_pid())).unwrap(), Acquire::Acquired));
        match acquire(&pid, &rec(424242)).unwrap() {
            Acquire::Taken(Some(r)) => assert_eq!(r.pid, own_pid(), "the holder's record is returned"),
            other => panic!("expected Taken, got {other:?}"),
        }
    }

    #[test]
    fn read_round_trips_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join(".selene/daemon.pid");
        acquire(&pid, &rec(777)).unwrap();
        assert_eq!(read(&pid).unwrap(), rec(777));
    }

    #[test]
    fn clear_stale_removes_a_dead_holder_but_not_a_live_one() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join(".selene/daemon.pid");

        // A live holder (us) is never cleared.
        acquire(&pid, &rec(own_pid())).unwrap();
        assert!(!clear_stale(&pid, own_pid()), "a live pid is not stale");
        assert!(pid.exists());

        // A dead holder is cleared — but only when the expected pid matches.
        std::fs::remove_file(&pid).unwrap();
        acquire(&pid, &rec(424242)).unwrap(); // 424242: astronomically unlikely to be alive
        assert!(!clear_stale(&pid, 999999), "wrong expected pid → left alone");
        assert!(clear_stale(&pid, 424242), "matching dead pid → cleared");
        assert!(!pid.exists());
    }

    #[test]
    fn release_only_removes_our_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join(".selene/daemon.pid");
        acquire(&pid, &rec(own_pid())).unwrap();
        assert!(!release(&pid, 111), "not our pid → keep");
        assert!(release(&pid, own_pid()), "our pid → remove");
        assert!(!pid.exists());
    }

    #[test]
    fn read_tolerates_a_bare_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("daemon.pid");
        std::fs::create_dir_all(pid.parent().unwrap()).unwrap();
        std::fs::write(&pid, "12345\n").unwrap();
        let r = read(&pid).unwrap();
        assert_eq!(r.pid, 12345);
        assert_eq!(r.version, "unknown");
    }
}
