#![recursion_limit = "1024"]
//! EXPERIMENT (not product code): the writes are 20× slower than the database can go. Which write?
//!
//! Measured 2026-07-14. Two expensive hypotheses died before this one, and killing them is what
//! made it visible:
//!
//! 1. **"RocksDB fsyncs on every commit."** True (`Sync mode: every transaction commit`), and it
//!    costs ~12% — noise. Worse, the knob is UNREACHABLE: the `surrealdb` SDK builds the
//!    datastore via `Datastore::builder()` and **never calls `.with_config(..)`**
//!    (`engine/local/native.rs:131`), while `Builder::new()` starts from `ConfigMap::empty()`.
//!    So in-process, EVERY `SURREAL_*` variable is inert — `SURREAL_DATASTORE_SYNC=never`
//!    provably changed nothing (the log still said `every`). See `probe_sync_mode`.
//! 2. **"SurrealDB/RocksDB is just slow at writing."** No: it inserts django-scale data
//!    (19 061 nodes + 46 942 edges) in **1.4 s**. Our persist takes **29.5 s**.
//!
//! The database is 20× faster than what we ask of it. So the cost is in HOW we ask.
//!
//! # The three shapes, and why the obvious batch is a trap
//!
//! - **A — production today.** One statement per resolved reference:
//!   `DELETE unresolved_ref WHERE fromNodeId=$f AND referenceName=$n AND referenceKind=$k`,
//!   52 358 of them on django, concatenated CHUNK at a time into one round trip. The round trips
//!   are already batched; the *statements* are not. Each one is an index probe.
//! - **B — the obvious batch.** `WHERE [fromNodeId, referenceName, referenceKind] IN [...]`, one
//!   statement per chunk. It keeps the key as the exact 3-field tuple (no hashing — incident #760
//!   lost data to a 2-field key, so a concatenated/hashed key is forbidden). **But an expression
//!   over an array cannot use the composite index**, so each chunk degrades to a full table scan.
//!   A first cut of this probe ran B at django scale and was still going after **30 minutes**.
//!   The obvious batch is not merely slower — it is quadratic. That is the finding, not a bug.
//! - **C — the tuple AS the primary key.** Make the record id the exact 3-tuple *array*
//!   (`unresolved_ref:['fn:1','sym_1','calls']`). An array record id is **not a hash** — it is the
//!   tuple itself, so #760's collision risk does not apply. Deletes become primary-key lookups and
//!   batch as `DELETE $ids`.
//!
//! Bounded on purpose (`SAMPLE` keys, not all 52 358) and **prints each variant as it lands** — a
//! probe that only prints at the end tells you nothing until it finishes, which is how the first
//! cut of this file burned 30 minutes and reported zero numbers.
//!
//!   cargo run --release -p selene-db --example probe_persist
use std::time::Instant;
use surrealdb_core::cnf::ConfigMap;
use surrealdb_core::dbs::Session;
use surrealdb_core::kvs::Datastore;
use surrealdb_types::Value;

/// django resolved this many refs. We measure a bounded slice and extrapolate to it.
const REFS: usize = 52_358;
/// Keys actually exercised per variant. Small enough that even the quadratic shape finishes.
const SAMPLE: usize = 4_000;
/// `unresolved.rs`'s CHUNK — statements (or ids) per round trip.
const CHUNK: usize = 500;

const FIELDS: &str = "
DEFINE FIELD IF NOT EXISTS fromNodeId ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS referenceName ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS referenceKind ON unresolved_ref TYPE string;
DEFINE INDEX IF NOT EXISTS unresolved_from_node ON unresolved_ref FIELDS fromNodeId;
DEFINE INDEX IF NOT EXISTS unresolved_ref_name ON unresolved_ref FIELDS referenceName;
DEFINE INDEX IF NOT EXISTS unresolved_key ON unresolved_ref FIELDS fromNodeId, referenceName, referenceKind;
";

fn key(i: usize) -> (String, String, String) {
    (format!("fn:{}", i % 400), format!("sym_{i}"), "calls".to_string())
}

async fn open(tag: &str) -> Result<(Datastore, Session), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("selene-pp-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let ds = Datastore::builder()
        .with_config(ConfigMap::empty()) // exactly what the SDK gives us today
        .build_with_path(&format!("rocksdb://{}", dir.display()))
        .await?;
    let ses = Session::owner().with_ns("selene").with_db("graph");
    run(&ds, &ses, "DEFINE NAMESPACE selene; USE NS selene; DEFINE DATABASE graph;").await?;
    run(&ds, &ses, &format!("DEFINE TABLE IF NOT EXISTS unresolved_ref SCHEMAFULL;{FIELDS}")).await?;
    Ok((ds, ses))
}

async fn run(ds: &Datastore, ses: &Session, sql: &str) -> Result<(), Box<dyn std::error::Error>> {
    // `execute` reports per-statement errors INSIDE the responses, not as Err. An earlier cut
    // swallowed "namespace does not exist" and clocked 66 000 rows in 0.16 s (412 k rows/s,
    // against a Phase-1 benchmark of 706 nodes/s). A write that wrote nothing is not a fast write.
    for r in ds.execute(sql, ses, None).await? {
        r.result?;
    }
    Ok(())
}

async fn count(ds: &Datastore, ses: &Session) -> Result<i64, Box<dyn std::error::Error>> {
    let mut out = 0;
    for r in ds.execute("SELECT count() FROM unresolved_ref GROUP ALL;", ses, None).await? {
        if let Value::Array(a) = r.result? {
            if let Some(Value::Object(o)) = a.first() {
                if let Some(Value::Number(n)) = o.get("count") {
                    out = n.to_string().parse::<i64>().unwrap_or(0);
                }
            }
        }
    }
    Ok(out)
}

/// Seed SAMPLE rows. `keyed_id` = give each row the 3-tuple array as its record id (variant C).
async fn seed(ds: &Datastore, ses: &Session, keyed_id: bool) -> Result<(), Box<dyn std::error::Error>> {
    for c in (0..SAMPLE).step_by(CHUNK) {
        let rows: Vec<String> = (c..(c + CHUNK).min(SAMPLE))
            .map(|i| {
                let (f, n, k) = key(i);
                if keyed_id {
                    format!("{{id:['{f}','{n}','{k}'],fromNodeId:'{f}',referenceName:'{n}',referenceKind:'{k}'}}")
                } else {
                    format!("{{fromNodeId:'{f}',referenceName:'{n}',referenceKind:'{k}'}}")
                }
            })
            .collect();
        run(ds, ses, &format!("INSERT INTO unresolved_ref [{}];", rows.join(","))).await?;
    }
    Ok(())
}

fn report(label: &str, secs: f64, before: i64, after: i64) {
    let per_key = secs / SAMPLE as f64;
    let projected = per_key * REFS as f64;
    if after != 0 {
        println!("  {label:<40} {secs:>7.2}s   *** DELETED NOTHING ({before}->{after}) — INVALID ***");
    } else {
        println!(
            "  {label:<40} {secs:>7.2}s for {SAMPLE} keys  ->  {projected:>6.1}s projected at {REFS} refs"
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{SAMPLE} keys per variant, CHUNK={CHUNK}, real schema + real composite index");
    println!("(production persist on django = 29.5s for {REFS} refs)\n");

    // ---- A: production's shape — one indexed DELETE statement per key ------------------------
    let (ds, ses) = open("a").await?;
    seed(&ds, &ses, false).await?;
    let before = count(&ds, &ses).await?;
    let t = Instant::now();
    for c in (0..SAMPLE).step_by(CHUNK) {
        let mut sql = String::new();
        for i in c..(c + CHUNK).min(SAMPLE) {
            let (f, n, k) = key(i);
            sql.push_str(&format!(
                "DELETE unresolved_ref WHERE fromNodeId='{f}' AND referenceName='{n}' AND referenceKind='{k}';"
            ));
        }
        run(&ds, &ses, &sql).await?;
    }
    let secs = t.elapsed().as_secs_f64();
    report("A  production: 1 DELETE per key", secs, before, count(&ds, &ses).await?);
    drop(ds);

    // ---- B: the obvious batch — array IN. Cannot use the index. -------------------------------
    let (ds, ses) = open("b").await?;
    seed(&ds, &ses, false).await?;
    let before = count(&ds, &ses).await?;
    let t = Instant::now();
    for c in (0..SAMPLE).step_by(CHUNK) {
        let triples: Vec<String> = (c..(c + CHUNK).min(SAMPLE))
            .map(|i| {
                let (f, n, k) = key(i);
                format!("['{f}','{n}','{k}']")
            })
            .collect();
        run(
            &ds,
            &ses,
            &format!(
                "DELETE unresolved_ref WHERE [fromNodeId,referenceName,referenceKind] IN [{}];",
                triples.join(",")
            ),
        )
        .await?;
    }
    let secs = t.elapsed().as_secs_f64();
    report("B  batch: [a,b,c] IN [...]  (no index)", secs, before, count(&ds, &ses).await?);
    drop(ds);

    // ---- C: the tuple IS the record id. Primary-key delete, batched. --------------------------
    let (ds, ses) = open("c").await?;
    seed(&ds, &ses, true).await?;
    let before = count(&ds, &ses).await?;
    let t = Instant::now();
    for c in (0..SAMPLE).step_by(CHUNK) {
        let ids: Vec<String> = (c..(c + CHUNK).min(SAMPLE))
            .map(|i| {
                let (f, n, k) = key(i);
                format!("unresolved_ref:['{f}','{n}','{k}']")
            })
            .collect();
        run(&ds, &ses, &format!("DELETE [{}];", ids.join(","))).await?;
    }
    let secs = t.elapsed().as_secs_f64();
    report("C  tuple AS record id (no hash)", secs, before, count(&ds, &ses).await?);
    drop(ds);

    Ok(())
}
