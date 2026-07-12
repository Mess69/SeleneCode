//! [`SurrealStore`] — the embedded-SurrealDB backend: open/init and the
//! idempotent schema apply. The `GraphStore` operations (nodes, edges, files,
//! unresolved refs, traversals, search) are added as inherent methods in later
//! tasks, and `impl GraphStore for SurrealStore` is wired in Task 10 once they
//! all exist.
//!
//! Namespace/database are fixed at `selene`/`graph`. On disk the store lives in
//! a directory named [`DATABASE_DIRNAME`]; callers place that under a project's
//! `.selene/`.

// `Path` (the `open` signature) and `Duration` (the reopen backoff) are used
// only by the on-disk constructors; gate their imports to a disk backend so a
// mem-only build stays warning-clean.
#[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
use std::path::Path;
#[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
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
    /// Compiled when the `kv-surrealkv` feature is on (the default disk
    /// backend). With only `kv-rocksdb`, the RocksDB variant below is compiled
    /// instead; with neither disk feature, `open` is not compiled at all.
    #[cfg(feature = "kv-surrealkv")]
    pub async fn open(dir: &Path) -> Result<Self> {
        let db = connect_disk_with_lock_retry(|| Surreal::new::<SurrealKv>(dir)).await?;
        db.use_ns(NAMESPACE).use_db(DATABASE).await?;
        Ok(Self { db })
    }

    /// Open (creating if absent) an on-disk store at `dir`, using RocksDB.
    /// Compiled only when `kv-rocksdb` is on and `kv-surrealkv` is off (so
    /// exactly one `open` exists). See [`Self::open`]'s surrealkv variant for
    /// the documented behavior.
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
    /// Returns `Err` if *any* schema statement fails: the whole program is run
    /// as one query and validated with [`surrealdb::Response::check`], which
    /// surfaces the first per-statement error (unique-index and other runtime
    /// errors hide behind an `Ok` from `query().await` otherwise — see the
    /// Task 1 spike).
    pub async fn apply_schema(&self) -> Result<()> {
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

    /// Escape hatch to the underlying SurrealDB handle.
    ///
    /// Not part of the stable surface: the pipeline crates
    /// (`selene-graph`/`selene-mcp`/`selene-cli`) depend on [`crate::GraphStore`],
    /// never on SurrealDB directly. This exists only so this crate's own
    /// integration tests can drive raw SurrealQL before the typed
    /// insert/traversal methods land in later tasks; `#[doc(hidden)]` keeps it
    /// out of the rendered API.
    #[doc(hidden)]
    pub fn db(&self) -> &Surreal<Db> {
        &self.db
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
