//! [`SurrealStore`] — the embedded-SurrealDB backend: open/init, the
//! idempotent schema apply, and bulk-load mode. The `GraphStore` operations
//! (nodes, edges, files, unresolved refs, traversals, search) are inherent
//! methods in the per-section modules (`src/nodes.rs`, `src/edges.rs`, …);
//! `impl GraphStore for SurrealStore` is the pure-delegation
//! `src/store_impl.rs` (Task 10).
//!
//! Namespace/database are fixed at `selene`/`graph`. On disk the store lives in
//! a directory named [`DATABASE_DIRNAME`]; callers place that under a project's
//! `.selene/`.

// `Path` (the `open` signature) is used only by the on-disk constructors;
// gate its import to a disk backend so a mem-only build stays warning-clean.
// `Duration` is engine-independent: `bulk_load_finish`'s index-build poll
// uses it on every backend (the reopen backoff also does, on disk builds).
#[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
use std::path::Path;
use std::time::Duration;

use surrealdb::Surreal;
use surrealdb::engine::local::Db;
#[cfg(feature = "kv-mem")]
use surrealdb::engine::local::Mem;
#[cfg(all(feature = "kv-rocksdb", not(feature = "kv-surrealkv")))]
use surrealdb::engine::local::RocksDb;
#[cfg(feature = "kv-surrealkv")]
use surrealdb::engine::local::SurrealKv;

use crate::schema;
use crate::{Error, Result};

/// Directory name of the on-disk database, placed by callers under `.selene/`
/// (e.g. `<project>/.selene/graph.db`). SurrealKV/RocksDB treat this path as
/// their storage folder.
pub const DATABASE_DIRNAME: &str = "graph.db";

/// SurrealDB namespace every store selects.
const NAMESPACE: &str = "selene";
/// SurrealDB database every store selects.
const DATABASE: &str = "graph";
/// How often `bulk_load_finish` re-polls a `CONCURRENTLY` index build. 100 ms
/// is negligible against the builds it waits on (7.6 s for 100k nodes) while
/// keeping the poll from hammering the engine mid-build.
const INDEX_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on one index's `bulk_load_finish` wait. All four builds
/// together measured 7.6 s on a 100k-node corpus (§5.3), so ten minutes is
/// ~80x headroom even for far larger repos; past it the build is assumed
/// wedged and surfaces as [`Error::IndexBuild`] instead of hanging the
/// caller forever.
const MAX_INDEX_BUILD_WAIT: Duration = Duration::from_secs(600);

/// The embedded-SurrealDB [`crate::GraphStore`] backend.
///
/// Holds a `Surreal<Db>` client over an `engine::local` datastore (in-memory or
/// on-disk depending on the constructor). Cheap to `clone` conceptually (the
/// client is `Arc`-backed) but not `Clone` here yet — nothing needs it.
#[derive(Debug)]
pub struct SurrealStore {
    db: Surreal<Db>,
}

impl SurrealStore {
    /// A fresh in-memory store (`kv-mem`). Data lives only for the process;
    /// used by fast tests and ephemeral tooling. Namespace/database are already
    /// selected; call [`Self::apply_schema`] before use.
    #[cfg(feature = "kv-mem")]
    pub async fn in_memory() -> Result<Self> {
        let db = Surreal::new::<Mem>(()).await?;
        db.use_ns(NAMESPACE).use_db(DATABASE).await?;
        Ok(Self { db })
    }

    /// Open (creating if absent) an on-disk store at `dir`, using the SurrealKV
    /// engine. Namespace/database are already selected; call
    /// [`Self::apply_schema`] before use.
    ///
    /// **Not** the default disk backend since the §5.3 gate run (2026-07-12):
    /// SurrealKV measured 15.3x slower than RocksDB on the bulk write path
    /// (2,161.8 s vs 141.7 s per 100k-node load — see
    /// `docs/benchmarks/2026-07-phase1-db-gate.md`), so the `kv-surrealkv`
    /// feature is now opt-in. When it *is* compiled, this variant is the one
    /// that exists — enabling the feature is an explicit choice (e.g. to open
    /// an existing SurrealKV store), and it takes preference over RocksDB.
    /// With only `kv-rocksdb` (the default), the RocksDB variant below is
    /// compiled instead; with neither disk feature, `open` is not compiled at
    /// all.
    #[cfg(feature = "kv-surrealkv")]
    pub async fn open(dir: &Path) -> Result<Self> {
        let db = connect_disk_with_lock_retry(|| Surreal::new::<SurrealKv>(dir)).await?;
        db.use_ns(NAMESPACE).use_db(DATABASE).await?;
        Ok(Self { db })
    }

    /// Open (creating if absent) an on-disk store at `dir`, using RocksDB —
    /// the **default** disk backend (§5.3 gate, 2026-07-12: fastest bulk load
    /// of the disk engines measured, with traversal reads equivalent to
    /// kv-mem's). Compiled only when `kv-rocksdb` is on and `kv-surrealkv` is
    /// off (so exactly one `open` exists); see the surrealkv variant above
    /// for the preference rationale and the shared documented behavior.
    #[cfg(all(feature = "kv-rocksdb", not(feature = "kv-surrealkv")))]
    pub async fn open(dir: &Path) -> Result<Self> {
        let db = connect_disk_with_lock_retry(|| Surreal::new::<RocksDb>(dir)).await?;
        db.use_ns(NAMESPACE).use_db(DATABASE).await?;
        Ok(Self { db })
    }

    /// Apply the v1 schema, idempotently. Every DDL statement is
    /// `DEFINE ... IF NOT EXISTS`, so re-running is a no-op. On first apply the
    /// current [`schema::SCHEMA_VERSION`] is seeded into `meta:schema_version`;
    /// a later run never overwrites an existing version.
    ///
    /// **Refuses a future store** ([`Error::SchemaTooNew`]): if the stored
    /// version is *greater* than this build's [`schema::SCHEMA_VERSION`], no
    /// DDL is run — against a newer schema every `IF NOT EXISTS` statement
    /// would silently no-op and this (older) build would then read/write
    /// shapes it does not understand. The guard runs *before* the DDL so a
    /// future store is left byte-untouched.
    ///
    /// Returns `Err` if *any* schema statement fails: the whole program is run
    /// as one query and validated with `surrealdb::Response::check`, which
    /// surfaces the first per-statement error (unique-index and other runtime
    /// errors hide behind an `Ok` from `query().await` otherwise — see the
    /// Task 1 spike).
    pub async fn apply_schema(&self) -> Result<()> {
        if let Some(stored) = self.schema_version().await?
            && stored > schema::SCHEMA_VERSION
        {
            return Err(Error::SchemaTooNew {
                stored,
                supported: schema::SCHEMA_VERSION,
            });
        }

        self.db.query(schema::all_ddl()).await?.check()?;

        if self.schema_version().await?.is_none() {
            let seed = format!(
                "CREATE meta:schema_version SET value = '{}'",
                schema::SCHEMA_VERSION
            );
            self.db.query(seed).await?.check()?;
        }
        Ok(())
    }

    /// The stored schema version, or `None` if the schema has never been
    /// applied. Reads the string `meta:schema_version.value` and parses it to a
    /// `u32`.
    ///
    /// Uses `RETURN <record>.<field>` rather than a `SELECT ... FROM meta:...`:
    /// on a store whose schema was never applied the `meta` table does not
    /// exist, and a `SELECT` from an undefined table *errors*, whereas
    /// `RETURN meta:schema_version.value` yields `NONE`. Returning `None` (not
    /// `Err`) on an uninitialized store matches the store's success-shaped-miss
    /// contract — a caller checks "is this indexed yet?" without tripping an
    /// error.
    pub async fn schema_version(&self) -> Result<Option<u32>> {
        let mut resp = self.db.query("RETURN meta:schema_version.value").await?;
        let raw: Option<String> = resp.take(0)?;
        match raw {
            Some(raw) => {
                let version = raw.parse::<u32>().map_err(|_| {
                    Error::Decode(format!("meta:schema_version value '{raw}' is not a u32"))
                })?;
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    /// Enter bulk-load mode: apply the schema (idempotently, initializing a
    /// fresh store), then drop the four FULLTEXT indexes so a large
    /// `insert_nodes` stream skips inline FTS maintenance.
    ///
    /// Why (§5.3 remediation, 100k-node synthetic graph, kv-mem, release):
    /// node insertion under the four FULLTEXT indexes runs at 803 nodes/s;
    /// without them, 4,703 nodes/s (5.9x) — full load 175.8 s → 81.9 s
    /// including the post-load rebuild
    /// (`docs/benchmarks/2026-07-phase1-db-gate.md`). Edge throughput is
    /// FTS-independent (edge tables carry no FULLTEXT index).
    ///
    /// Idempotent: safe on a fresh store (the schema apply is all
    /// `IF NOT EXISTS`) and safe to call twice (the drops are `REMOVE INDEX
    /// IF EXISTS`). Between this call and [`Self::bulk_load_finish`],
    /// `search_fts` returns `Ok(vec![])` — the missing-index query error is
    /// swallowed by the search module's FTS-swallow contract — a
    /// success-shaped miss, never `Err` (crate `isError` discipline).
    pub async fn bulk_load_begin(&self) -> Result<()> {
        self.apply_schema().await?;
        self.db
            .query(schema::remove_fts_index_ddl())
            .await?
            .check()?;
        Ok(())
    }

    /// Leave bulk-load mode: re-define the four FULLTEXT indexes with
    /// `DEFINE INDEX ... CONCURRENTLY` and poll each build to completion, so
    /// the store is search-ready when this returns.
    ///
    /// `CONCURRENTLY` is supported by embedded SurrealDB 3.2.1 and builds the
    /// four indexes in parallel — 7.6 s vs 16.0 s blocking-sequential on a
    /// 100k-node corpus, with `search_fts` results identical to an inline
    /// (non-deferred) load (`docs/benchmarks/2026-07-phase1-db-gate.md`).
    /// Total time is corpus-size-dependent; the poll interval is bounded
    /// (`INDEX_POLL_INTERVAL`), the wait itself is not.
    ///
    /// Idempotent: the DEFINEs are `IF NOT EXISTS`, and an already-built
    /// index reports `ready` (or carries no `building` entry) on the first
    /// poll — so calling twice, or on a store that never entered bulk-load
    /// mode (inline FTS from [`Self::apply_schema`]), is a no-op. A build
    /// failure surfaces as [`Error::IndexBuild`].
    pub async fn bulk_load_finish(&self) -> Result<()> {
        self.db.query(schema::fts_index_ddl(true)).await?.check()?;
        for (name, _) in schema::FTS_INDEXES {
            self.wait_index_ready(name).await?;
        }
        Ok(())
    }

    /// Poll `INFO FOR INDEX <name> ON TABLE node` until the build reports
    /// `ready` (classification table: [`index_build_state`]). The poll
    /// interval is bounded ([`INDEX_POLL_INTERVAL`]) **and** so is the total
    /// wait ([`MAX_INDEX_BUILD_WAIT`]): a build still in progress past the
    /// bound, a `status: "error"`, or a status this crate does not recognize
    /// all surface as [`Error::IndexBuild`] — never an infinite loop. Only
    /// ever called with names from [`schema::FTS_INDEXES`] — the `format!`
    /// embeds no caller-supplied text — and only after the index was DEFINEd
    /// (`INFO FOR INDEX` on an *absent* index errors, which `?` surfaces).
    async fn wait_index_ready(&self, name: &str) -> Result<()> {
        let started = std::time::Instant::now();
        loop {
            let mut resp = self
                .db
                .query(format!("INFO FOR INDEX {name} ON TABLE node"))
                .await?;
            let info: Option<serde_json::Value> = resp.take(0)?;
            match index_build_state(info.as_ref()) {
                IndexBuildState::Ready => return Ok(()),
                IndexBuildState::Failed(detail) => {
                    return Err(Error::IndexBuild(format!(
                        "FULLTEXT index '{name}': {detail}"
                    )));
                }
                IndexBuildState::InProgress => {
                    if started.elapsed() >= MAX_INDEX_BUILD_WAIT {
                        return Err(Error::IndexBuild(format!(
                            "FULLTEXT index '{name}' still building after {}s (bound: \
                             the whole 4-index build measured 7.6 s on 100k nodes) — \
                             assuming the build is wedged",
                            MAX_INDEX_BUILD_WAIT.as_secs()
                        )));
                    }
                    tokio::time::sleep(INDEX_POLL_INTERVAL).await;
                }
            }
        }
    }

    /// Escape hatch to the underlying SurrealDB handle.
    ///
    /// Not part of the stable surface: the pipeline crates
    /// (`selene-graph`/`selene-mcp`/`selene-cli`) depend on [`crate::GraphStore`],
    /// never on SurrealDB directly. Used internally by this crate's other
    /// modules (e.g. `src/nodes.rs`) to run typed queries, and by this
    /// crate's own integration tests to drive raw SurrealQL for whatever
    /// operation groups haven't landed yet; `#[doc(hidden)]` keeps it out of
    /// the rendered API.
    #[doc(hidden)]
    pub fn db(&self) -> &Surreal<Db> {
        &self.db
    }
}

/// One classified observation of a `CONCURRENTLY` index build, decoded from
/// `INFO FOR INDEX`'s `building` object by [`index_build_state`].
#[derive(Debug, PartialEq)]
enum IndexBuildState {
    /// `building.status: "ready"`, or no `building` object at all (an
    /// inline-built index has nothing to build).
    Ready,
    /// A known in-progress status — keep polling (up to
    /// [`MAX_INDEX_BUILD_WAIT`]).
    InProgress,
    /// `building.status: "error"` (engine detail attached), or a status this
    /// crate does not recognize. Both are terminal: polling an unknown
    /// status could spin forever, so it fails loudly instead.
    Failed(String),
}

/// Pure classification of one `INFO FOR INDEX` result — split from the poll
/// loop so the status table is unit-testable without a real (slow) build.
///
/// The in-progress set is the Task 9d probe's observed sequence on embedded
/// 3.2.1 — `{ building: { status: "cleaning" } }` →
/// `{ building: { status: "indexing", initial, pending, updated } }` →
/// `{ building: { status: "ready", .. } }` — plus `"started"` (the documented
/// initial status). Anything else is treated as terminal, not poll-worthy.
fn index_build_state(info: Option<&serde_json::Value>) -> IndexBuildState {
    let building = info.and_then(|i| i.get("building"));
    let status = building
        .and_then(|b| b.get("status"))
        .and_then(|s| s.as_str());
    match status {
        None | Some("ready") => IndexBuildState::Ready,
        Some("started" | "cleaning" | "indexing") => IndexBuildState::InProgress,
        Some("error") => IndexBuildState::Failed(format!(
            "build error: {}",
            building
                .and_then(|b| b.get("error"))
                .and_then(|e| e.as_str())
                .unwrap_or("(no detail)")
        )),
        Some(other) => IndexBuildState::Failed(format!(
            "unrecognized build status '{other}' — treating as terminal rather than \
             polling indefinitely"
        )),
    }
}

/// Connect to an on-disk datastore, retrying briefly on a file-lock contention.
///
/// An `engine::local` on-disk store (SurrealKV/RocksDB) holds an exclusive
/// `LOCK` file, and a just-closed store in the *same process* releases it
/// asynchronously — dropping the `Surreal` handle signals shutdown but the
/// background task frees the lock a fraction of a second later (observed
/// ~300 ms in the Task 3 reopen test). Retrying on a lock error lets an
/// immediate reopen (a test, a daemon restart) succeed; a lock genuinely held
/// by another *live* process persists past the budget, and its error is
/// surfaced unchanged. Non-lock errors fail immediately.
#[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
async fn connect_disk_with_lock_retry<F, Fut>(mut connect: F) -> Result<Surreal<Db>>
where
    F: FnMut() -> Fut,
    Fut: IntoFuture<Output = surrealdb::Result<Surreal<Db>>>,
{
    /// ~2 s total (40 × 50 ms): comfortably past the observed release time,
    /// short enough to fail fast on genuine cross-process contention.
    const MAX_ATTEMPTS: u32 = 40;
    const BACKOFF: Duration = Duration::from_millis(50);

    let mut attempt = 0;
    loop {
        attempt += 1;
        match connect().await {
            Ok(db) => return Ok(db),
            Err(err) => {
                let lock_contended = err.to_string().to_lowercase().contains("lock");
                if lock_contended && attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(BACKOFF).await;
                    continue;
                }
                return Err(err.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The status table [`wait_index_ready`] polls on, checked against every
    /// shape the Task 9d probe observed on embedded 3.2.1 plus the terminal
    /// fallbacks (unknown status must fail, not poll forever).
    #[test]
    fn index_build_state_classifies_observed_and_unknown_shapes() {
        let ready =
            json!({"building": {"status": "ready", "initial": 0, "pending": 0, "updated": 0}});
        assert_eq!(index_build_state(Some(&ready)), IndexBuildState::Ready);

        for in_progress in [
            json!({"building": {"status": "started"}}),
            json!({"building": {"status": "cleaning"}}),
            json!({"building": {"status": "indexing", "initial": 3, "pending": 1}}),
        ] {
            assert_eq!(
                index_build_state(Some(&in_progress)),
                IndexBuildState::InProgress,
                "must keep polling on {in_progress}"
            );
        }

        // No `building` object (an inline-built index) and no info at all
        // both count as ready.
        assert_eq!(index_build_state(Some(&json!({}))), IndexBuildState::Ready);
        assert_eq!(index_build_state(None), IndexBuildState::Ready);

        // An engine-reported error carries its detail through.
        let err = json!({"building": {"status": "error", "error": "boom"}});
        let IndexBuildState::Failed(detail) = index_build_state(Some(&err)) else {
            panic!("error status must be terminal");
        };
        assert!(detail.contains("boom"), "engine detail preserved: {detail}");

        // An unrecognized status is terminal, never an infinite poll.
        let odd = json!({"building": {"status": "some-future-status"}});
        assert!(matches!(
            index_build_state(Some(&odd)),
            IndexBuildState::Failed(_)
        ));
    }
}
