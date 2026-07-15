//! Liveness watchdog — if the daemon's tokio runtime wedges, kill the process rather than leave a
//! zombie holding the exclusive lock.
//!
//! # Why a heartbeat *task* plus a watchdog *thread*
//!
//! A wedge means the async runtime can no longer make progress (a deadlock, a synchronous infinite
//! loop on every worker). We detect it with two independent parts:
//!
//! - a **tokio task** that beats a counter on a timer — it runs iff the runtime is healthy;
//! - a dedicated **OS thread** (outside the runtime, so a wedged runtime cannot stall it) that
//!   aborts the process if the counter stops advancing for `timeout`.
//!
//! # The disk-progress deferral (#1231)
//!
//! A long SurrealDB statement on slow storage looks the same as a wedge from the counter's side —
//! no beat. So before aborting, the thread checks whether the `.selene/` files are still growing;
//! if they are, it defers (up to `10× timeout` of continuous silence) rather than kill real work.
//!
//! Opt out with `SELENE_NO_WATCHDOG=1`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const NO_WATCHDOG_ENV: &str = "SELENE_NO_WATCHDOG";
const TIMEOUT_ENV: &str = "SELENE_WATCHDOG_TIMEOUT_MS";
const DEFAULT_TIMEOUT_MS: u64 = 60_000;
/// Even with disk progress, `10× timeout` of continuous no-beat silence is a wedge.
const PROGRESS_CAP_MULTIPLIER: u32 = 10;

/// A heartbeat the daemon beats from a tokio task. Cloneable (it's just a shared counter).
#[derive(Clone)]
pub struct Heartbeat {
    ticks: Arc<AtomicU64>,
}

impl Heartbeat {
    /// Record liveness. Called on a timer from a tokio task; a stalled runtime stops calling it.
    pub fn beat(&self) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }
}

/// Spawn the watchdog OS thread and return the [`Heartbeat`] the daemon should beat — or `None` if
/// watching is disabled. The thread is detached and lives for the process.
pub fn spawn(db_dir: &Path) -> Option<Heartbeat> {
    if matches!(std::env::var(NO_WATCHDOG_ENV).ok().as_deref(), Some(v) if v != "0" && !v.is_empty()) {
        return None;
    }
    let timeout = Duration::from_millis(
        std::env::var(TIMEOUT_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_TIMEOUT_MS),
    );
    // Heartbeat interval: brisk but not busy — min(2s, max(50ms, timeout/5)).
    let interval = timeout
        .checked_div(5)
        .unwrap_or(Duration::from_millis(50))
        .clamp(Duration::from_millis(50), Duration::from_secs(2));

    let hb = Heartbeat { ticks: Arc::new(AtomicU64::new(0)) };
    let ticks = hb.ticks.clone();
    let db_dir = db_dir.to_path_buf();

    std::thread::Builder::new()
        .name("selene-watchdog".into())
        .spawn(move || watch(ticks, db_dir, timeout, interval))
        .ok()?;
    Some(hb)
}

/// The watchdog loop, on its own OS thread.
fn watch(ticks: Arc<AtomicU64>, db_dir: PathBuf, timeout: Duration, interval: Duration) {
    let mut last_tick = ticks.load(Ordering::Relaxed);
    let mut silence = Duration::ZERO;
    let mut fingerprint = dir_fingerprint(&db_dir);

    loop {
        std::thread::sleep(interval);
        let t = ticks.load(Ordering::Relaxed);
        if t != last_tick {
            last_tick = t;
            silence = Duration::ZERO;
            continue;
        }
        silence += interval;
        if silence < timeout {
            continue;
        }
        let now = dir_fingerprint(&db_dir);
        let progressed = now != fingerprint;
        fingerprint = now;
        if should_abort(silence, timeout, progressed) {
            eprintln!(
                "[selene daemon] watchdog: no heartbeat for {silence:?} and no disk progress; aborting"
            );
            std::process::abort();
        }
    }
}

/// The kill decision, factored out so it can be tested without aborting the test process. Abort when
/// silence has reached the timeout AND either there is no disk progress, or the `10×` hard cap is hit.
fn should_abort(silence: Duration, timeout: Duration, progressed: bool) -> bool {
    if silence < timeout {
        return false;
    }
    let cap = timeout.saturating_mul(PROGRESS_CAP_MULTIPLIER);
    !progressed || silence >= cap
}

/// A cheap fingerprint of the `.selene/` tree: (total bytes, newest mtime nanos). A change in either
/// means the DB is still writing — i.e. real work, not a wedge. Errors read as "no change".
fn dir_fingerprint(dir: &Path) -> (u64, u128) {
    let mut total = 0u64;
    let mut newest = 0u128;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(e.path());
            } else {
                total = total.saturating_add(meta.len());
                if let Some(nanos) = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                {
                    newest = newest.max(nanos);
                }
            }
        }
    }
    (total, newest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn beat_advances_the_counter() {
        let hb = Heartbeat { ticks: Arc::new(AtomicU64::new(0)) };
        hb.beat();
        hb.beat();
        assert_eq!(hb.ticks.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn abort_decision_table() {
        let t = Duration::from_secs(60);
        // Below timeout: never abort.
        assert!(!should_abort(Duration::from_secs(30), t, false));
        // At timeout, no progress: abort.
        assert!(should_abort(t, t, false));
        // At timeout, but disk progressing: defer.
        assert!(!should_abort(t, t, true));
        // Progressing but past the 10x cap: abort anyway.
        assert!(should_abort(Duration::from_secs(600), t, true));
    }

    #[test]
    fn fingerprint_changes_when_a_file_grows() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "x").unwrap();
        let a = dir_fingerprint(tmp.path());
        std::fs::write(tmp.path().join("a"), "xxxxxx").unwrap();
        let b = dir_fingerprint(tmp.path());
        assert_ne!(a, b, "a size change moves the fingerprint");
    }
}
