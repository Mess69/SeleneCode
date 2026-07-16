//! The daemon's FileWatcher — keep the warm graph fresh without a manual `selene sync`.
//!
//! The daemon already owns the store and can sync against its warm handle ([`super::run_sync`]).
//! This adds the trigger: a recursive `notify` watch on the project root that, after a quiet
//! debounce window, re-indexes whatever changed. The re-index lands on the very handle the query
//! cache serves from, so the next tool call sees fresh code.
//!
//! # The one hazard that would wreck this: the feedback loop
//!
//! A sync **writes to `.selene/`** (RocksDB files). The watch is recursive, so those writes would
//! fire more events, which would trigger another sync, forever. [`relevant`] drops every event
//! under `.selene/` and `.git/` — that filter is not an optimization, it is what makes the watcher
//! terminate. Everything else is left to `sync`'s own change detection (a touched non-source file
//! simply yields a no-op sync), so the filter can stay coarse and obviously-correct.
//!
//! Opt out with `SELENE_NO_WATCH=1` (a daemon then only syncs when a `selene sync` is routed to it).

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// Debounce window: coalesce a burst of edits (a branch switch, a formatter run) into one sync.
const DEBOUNCE: Duration = Duration::from_millis(2000);

const NO_WATCH_ENV: &str = "SELENE_NO_WATCH";

/// Is this changed path worth a sync? Drop the data dirs (feedback-loop guard); accept everything
/// else and let `sync` decide whether any *source* file actually changed.
fn relevant(path: &Path) -> bool {
    !path.components().any(|c| {
        let s = c.as_os_str();
        s == ".selene" || s == ".git"
    })
}

/// Watch `root` and auto-sync the daemon's warm store on debounced changes. Runs until the daemon
/// exits (the returned future never resolves in normal operation). A no-op if `SELENE_NO_WATCH=1`
/// or the watch cannot be installed (the daemon still serves; it just won't auto-sync).
pub async fn watch_and_sync(root: PathBuf) {
    if matches!(std::env::var(NO_WATCH_ENV).ok().as_deref(), Some(v) if v != "0" && !v.is_empty()) {
        eprintln!("[selene daemon] file watching disabled (SELENE_NO_WATCH)");
        return;
    }

    // notify's event callback runs on its own OS thread; bridge into async with an unbounded
    // channel (send is sync and thread-safe).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PathBuf>>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                let _ = tx.send(event.paths);
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[selene daemon] file watching unavailable: {e}");
                return;
            }
        };
    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        eprintln!("[selene daemon] could not watch {}: {e}", root.display());
        return;
    }
    eprintln!("[selene daemon] watching {} for changes", root.display());

    loop {
        // Block until the first RELEVANT event of a new burst. Irrelevant events (the sync's own
        // writes under `.selene/`, `.git/` churn) are dropped here — this is what breaks the
        // feedback loop: a sync's writes can never *start* a burst.
        loop {
            match rx.recv().await {
                Some(paths) if paths.iter().any(|p| relevant(p)) => break,
                Some(_) => continue,
                None => return, // channel closed → watcher dropped → stop
            }
        }

        // Debounce with a deadline. Only a RELEVANT event extends the window; the sync's own
        // `.selene/` writes arrive here too but must NOT keep resetting it, or it would never
        // settle. When the deadline passes with no fresh relevant activity, flush.
        let mut deadline = tokio::time::Instant::now() + DEBOUNCE;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(paths)) => {
                    if paths.iter().any(|p| relevant(p)) {
                        deadline = tokio::time::Instant::now() + DEBOUNCE;
                    }
                    // irrelevant → ignore, deadline unchanged
                }
                Ok(None) => return, // channel closed
                Err(_) => break,    // quiet window elapsed → flush
            }
        }

        // Flush: re-index against the warm store. Errors are logged, never fatal — the daemon
        // keeps serving even if one sync fails.
        flush(&root).await;
    }
}

/// Re-index `root` against the daemon's warm store handle (no second `open`, so no lock fight).
async fn flush(root: &Path) {
    let store = match crate::handlers::warm_store_for_root(root).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[selene daemon] auto-sync: {e}");
            return;
        }
    };
    match selene_sync::sync_project_with_store(root, store).await {
        Ok(stats) if stats.changed > 0 || stats.removed > 0 => {
            eprintln!(
                "[selene daemon] auto-sync: {} changed, {} removed",
                stats.changed, stats.removed
            );
        }
        Ok(_) => {} // no source file actually changed — silent
        Err(e) => eprintln!("[selene daemon] auto-sync failed: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dirs_are_filtered_source_files_are_not() {
        assert!(
            !relevant(Path::new("/p/.selene/daemon.pid")),
            ".selene/ is the feedback-loop guard"
        );
        assert!(!relevant(Path::new("/p/.git/index")), ".git/ is noise");
        assert!(
            relevant(Path::new("/p/src/a.ts")),
            "a real source edit passes"
        );
        assert!(
            relevant(Path::new("/p/README.md")),
            "sync's own detection handles non-source"
        );
    }
}
