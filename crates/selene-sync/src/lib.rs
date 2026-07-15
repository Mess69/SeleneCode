//! `selene-sync` — incremental sync: re-index only what changed.
//!
//! A full `index` re-parses and re-resolves everything. `sync` does the same thing the extraction
//! orchestrator already knows how to do per-file — but only for the files that actually changed
//! since the last index, so the cost is proportional to the diff, not the repo.
//!
//! **Change detection is two-tier, cheap-first.** The scan lists every current file; a file whose
//! on-disk `mtime` is not newer than the last index AND is already in the graph is skipped without
//! being read (the common case — most files don't change between syncs). Only the survivors are
//! read and content-hashed, and only a *different* hash counts as changed — an `mtime` touch with
//! identical bytes is not a re-index. Files gone from disk but still in the graph are deleted.
//!
//! The re-index itself is `Indexer::index_files`, which runs the single-file REPLACE protocol
//! (snapshot cross-file incoming edges → delete → re-insert → re-attach), so a change to one file
//! never orphans edges pointing into it. After the touched files are re-extracted, resolution runs
//! over the store's pending queue to bind the new references.

pub mod hooks;
pub mod worktree;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use selene_core::hash_content;
use selene_db::SurrealStore;
use selene_extract::{Indexer, ScanOverrides, scan_directory};

/// What a sync did — for the CLI to report.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
}

impl SyncStats {
    /// A sync that touched nothing — the common, cheap case.
    pub fn is_noop(&self) -> bool {
        self.changed == 0 && self.removed == 0
    }
}

/// Sync the index at `root`'s `.selene/` to the current file tree. `root` must already be indexed.
///
/// Opens the store itself — the CLI path, used when **no** daemon holds the exclusive lock.
pub async fn sync_project(root: &Path) -> Result<SyncStats> {
    let dir = root.join(".selene");
    anyhow::ensure!(
        dir.exists(),
        "not indexed: {} has no .selene/",
        root.display()
    );
    let store = SurrealStore::open(&dir).await.context("open index")?;
    store.apply_schema().await.context("apply schema")?;
    sync_project_with_store(root, store).await
}

/// Sync using an **already-open** store — the daemon path. The daemon holds the exclusive RocksDB
/// lock, so a second `SurrealStore::open` in any process (even this one) would deadlock on it;
/// instead the daemon hands its warm handle straight in. Because that handle is the very one the
/// query cache serves from, the re-index is visible to subsequent tool calls the instant it lands.
pub async fn sync_project_with_store(root: &Path, store: SurrealStore) -> Result<SyncStats> {
    // The graph's current view: path -> content hash.
    let indexed: HashMap<String, String> = store
        .all_files()
        .await
        .context("read indexed files")?
        .into_iter()
        .map(|f| (f.path, f.content_hash))
        .collect();
    let last_indexed_ms = store.last_indexed_at().await.ok().flatten().unwrap_or(0);

    // The disk's current view.
    let current = scan_directory(root, &ScanOverrides::default()).context("scan project")?;
    let current_set: std::collections::HashSet<&String> = current.iter().collect();

    // --- classify -------------------------------------------------------------------------------
    let mut changed: Vec<String> = Vec::new();
    let mut unchanged = 0usize;
    for rel in &current {
        let abs = root.join(rel);
        // Cheap tier: an unchanged mtime on an already-indexed file skips the read entirely.
        let mtime_ms = std::fs::metadata(&abs)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(i64::MAX);
        let known = indexed.get(rel);
        if known.is_some() && mtime_ms <= last_indexed_ms {
            unchanged += 1;
            continue;
        }
        // Expensive tier: read + hash, and only a DIFFERENT hash is a real change.
        match std::fs::read_to_string(&abs) {
            Ok(text) => {
                let h = hash_content(&text);
                if known == Some(&h) {
                    unchanged += 1;
                } else {
                    changed.push(rel.clone());
                }
            }
            // Unreadable (binary, permissions) — if it was indexed, treat as changed so the
            // orchestrator can decide (it skips oversized/binary); if new, skip it.
            Err(_) if known.is_some() => changed.push(rel.clone()),
            Err(_) => {}
        }
    }

    // Files the graph has but the disk no longer does.
    let removed: Vec<String> = indexed
        .keys()
        .filter(|p| !current_set.contains(p))
        .cloned()
        .collect();

    // --- apply ----------------------------------------------------------------------------------
    for path in &removed {
        store
            .delete_file(path)
            .await
            .with_context(|| format!("delete {path}"))?;
    }

    let stats = SyncStats {
        changed: changed.len(),
        removed: removed.len(),
        unchanged,
    };

    if !changed.is_empty() {
        let indexer = Indexer::new(root.to_path_buf(), store);
        indexer.index_files(&changed).await;
        let store = indexer.into_store();
        // Bind the references the re-indexed files produced (they went into the store's pending
        // queue via `replace_file_extraction`). The store-based resolve path, not the in-memory
        // one — an incremental sync's refs live in the store, not in a fresh IndexResult.
        selene_resolve::resolve_and_persist_batched(&store, root, None)
            .await
            .context("resolve after sync")?;
    } else if !removed.is_empty() {
        // Deletions can leave dangling refs too; re-bind.
        selene_resolve::resolve_and_persist_batched(&store, root, None)
            .await
            .context("resolve after deletions")?;
    }

    Ok(stats)
}
