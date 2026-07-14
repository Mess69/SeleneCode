#![recursion_limit = "1024"]
//! EXPERIMENT (not product code): does escaping `SyncMode::Every` actually pay?
//!
//! The head-to-head (`docs/benchmarks/2026-07-14-rust-vs-ts-speed.md`) put us 8–11× behind the
//! TS build, and the phase breakdown blamed the WRITES: DB persist is ~48% of a run while the
//! resolution ladder is 14%. SurrealDB's RocksDB logs `Sync mode: every transaction commit` —
//! an fsync per commit — where CodeGraph's SQLite runs in WAL mode.
//!
//! # Why we cannot simply set an env var
//!
//! `surrealdb`'s SDK builds the datastore like this (`engine/local/native.rs:131`):
//!
//! ```text
//! Datastore::builder()
//!     .with_query_timeout(..).with_transaction_timeout(..).with_auth(..)
//!     .build_with_path(endpoint)          // <- .with_config(..) is NEVER called
//! ```
//!
//! and `Builder::new()` starts from `config: ConfigMap::empty()`. So **in-process, through the
//! SDK, every `SURREAL_*` knob is inert** — `SURREAL_DATASTORE_SYNC=never` changes nothing, which
//! is exactly what we measured (the log still said `every`). Those variables only work through
//! the `surreal` server binary, which builds its own `ConfigMap::from_env()`.
//!
//! Reaching the knob therefore means talking to `surrealdb-core`'s `Datastore` directly
//! (`Datastore::builder().with_config(..)` + `Datastore::execute`) instead of the SDK's
//! `Surreal<Db>` client — a real refactor of `selene-db`'s query layer.
//!
//! **So prove it pays first.** Same load, three sync modes, one number each. If `never` does not
//! collapse the write time, the refactor is not worth doing and we have spent 20 minutes instead
//! of a week.
//!
//!   cargo run --release -p selene-db --example probe_sync_mode
use std::time::Instant;
use surrealdb_core::cnf::ConfigMap;
use surrealdb_core::dbs::Session;
use surrealdb_core::kvs::Datastore;

/// django-scale, rounded: 19 061 nodes / 46 942 edges.
const NODES: usize = 19_000;
const EDGES: usize = 47_000;
/// Our real persist chunk size.
const CHUNK: usize = 500;

async fn run(label: &str, sync: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("selene-syncprobe-{}", label.replace(' ', "-")));
    let _ = std::fs::remove_dir_all(&dir);

    // The one line the SDK never writes.
    let map = match sync {
        // query-string shaped: `k=v&k=v` (ConfigMap::from_config_string)
        Some(v) => ConfigMap::from_config_string(&format!("datastore_sync={v}")),
        None => ConfigMap::empty(),
    };
    let ds = Datastore::builder()
        .with_config(map)
        .build_with_path(&format!("rocksdb://{}", dir.display()))
        .await?;
    let ses = Session::owner().with_ns("selene").with_db("graph");
    // Without these the INSERTs fail with "namespace does not exist" — and `execute` reports that
    // INSIDE the response, not as an Err. The first cut of this probe swallowed it and clocked
    // 66 000 rows in 0.16 s (412 k rows/s, against a Phase-1 benchmark of 706 nodes/s). A write
    // that wrote nothing is not a fast write.
    for r in ds
        .execute(
            "DEFINE NAMESPACE selene; USE NS selene; DEFINE DATABASE graph;",
            &ses,
            None,
        )
        .await?
    {
        r.result?;
    }

    let t0 = Instant::now();
    for c in (0..NODES).step_by(CHUNK) {
        let rows: Vec<String> = (c..(c + CHUNK).min(NODES))
            .map(|i| {
                format!(
                    "{{id:'n{i}',name:'sym{i}',kind:'function',file:'src/f{}.rs'}}",
                    i % 400
                )
            })
            .collect();
        for r in ds
            .execute(
                &format!("INSERT INTO node [{}];", rows.join(",")),
                &ses,
                None,
            )
            .await?
        {
            r.result?; // `execute` reports per-statement errors INSIDE the responses, not as Err
        }
    }
    let t_nodes = t0.elapsed();

    let t1 = Instant::now();
    for c in (0..EDGES).step_by(CHUNK) {
        let rows: Vec<String> = (c..(c + CHUNK).min(EDGES))
            .map(|i| {
                let (a, b) = (i % NODES, (i * 7 + 3) % NODES);
                format!("{{id:'e{i}',src:'n{a}',dst:'n{b}',kind:'calls'}}")
            })
            .collect();
        for r in ds
            .execute(
                &format!("INSERT INTO edge [{}];", rows.join(",")),
                &ses,
                None,
            )
            .await?
        {
            r.result?;
        }
    }
    let t_edges = t1.elapsed();

    let total = t0.elapsed();
    // A write that wrote nothing is not a fast write. COUNT before believing any number.
    let cnt = ds
        .execute(
            "SELECT count() FROM node GROUP ALL; SELECT count() FROM edge GROUP ALL;",
            &ses,
            None,
        )
        .await?;
    let shown: Vec<String> = cnt
        .into_iter()
        .map(|r| format!("{:?}", r.result.map(|v| format!("{v:?}"))))
        .collect();
    println!("      rows actually in the store: {}", shown.join(" | "));
    println!(
        "  {label:<34} nodes {:>7.2}s   edges {:>7.2}s   TOTAL {:>7.2}s",
        t_nodes.as_secs_f64(),
        t_edges.as_secs_f64(),
        total.as_secs_f64()
    );
    drop(ds);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{NODES} nodes + {EDGES} edges (django-scale), chunk={CHUNK}, RocksDB on disk\n");
    // `None` = ConfigMap::empty() = EXACTLY what the SDK gives us today. This row is the baseline
    // and it must reproduce the `Sync mode: every transaction commit` we see in production.
    run("SDK today (empty cfg = every)", None).await?;
    run("datastore_sync=every (explicit)", Some("every")).await?;
    run("datastore_sync=1s (interval)", Some("1s")).await?;
    run("datastore_sync=never (OS-managed)", Some("never")).await?;
    Ok(())
}
