//! PRD §5.3 benchmark gate — bulk load + deep traversal + FTS, measured on
//! both the in-memory (`kv-mem`) and on-disk (`kv-surrealkv`) SurrealDB
//! backends (and `kv-rocksdb` when that feature is enabled).
//!
//! Run with the generator feature on:
//!
//! ```bash
//! cargo bench -p selene-db --features bench-support
//! # add on-disk RocksDB too (long C++ build):
//! cargo bench -p selene-db --features bench-support,kv-rocksdb
//! ```
//!
//! Without `--features bench-support` the whole harness is a no-op `main` so
//! `cargo bench` / `clippy --benches` still compile cleanly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(not(feature = "bench-support"))]
fn main() {
    eprintln!("bulk_and_traverse requires --features bench-support; nothing to run");
}

#[cfg(feature = "bench-support")]
fn main() {
    benches::run();
}

#[cfg(feature = "bench-support")]
mod benches {
    use std::time::{Duration, Instant};

    use criterion::{BenchmarkId, Criterion};
    use selene_core::{Edge, EdgeKind, Node};
    use selene_db::SurrealStore;
    use selene_db::bench_support::{Landmarks, SyntheticGraph};
    use tokio::runtime::Runtime;

    /// Nodes in the big bulk-load graph (~5x that many edges).
    const BULK_NODES: usize = 100_000;
    /// Timed full-load repeats per backend for the bulk measurement. A single
    /// 100k+500k-row load is O(tens of seconds), so criterion's 10-sample floor
    /// would blow the wall-time budget — the brief's escape hatch is to time a
    /// full load per iteration with *fewer* samples. We report the median.
    const BULK_REPEATS: usize = 3;
    /// Nodes in the smaller graph the per-query benches load once per backend.
    const QUERY_NODES: usize = 20_000;
    const SEED: u64 = 0xC0DE_5EED;

    fn rt() -> Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    }

    /// A freshly-schema'd in-memory store.
    async fn fresh_mem() -> SurrealStore {
        let store = SurrealStore::in_memory().await.expect("in_memory");
        store.apply_schema().await.expect("apply_schema");
        store
    }

    /// A freshly-schema'd on-disk store under `dir` (surrealkv, or rocksdb when
    /// that is the only disk backend compiled).
    #[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
    async fn fresh_disk(dir: &std::path::Path) -> SurrealStore {
        let store = SurrealStore::open(dir).await.expect("open disk store");
        store.apply_schema().await.expect("apply_schema");
        store
    }

    async fn load(store: &SurrealStore, nodes: &[Node], edges: &[Edge]) -> u64 {
        store.insert_nodes(nodes).await.expect("insert_nodes");
        store.insert_edges(edges).await.expect("insert_edges")
    }

    /// Which disk backend label is compiled in, if any.
    fn disk_label() -> Option<&'static str> {
        #[cfg(feature = "kv-surrealkv")]
        {
            Some("kv-surrealkv")
        }
        #[cfg(all(feature = "kv-rocksdb", not(feature = "kv-surrealkv")))]
        {
            Some("kv-rocksdb")
        }
        #[cfg(not(any(feature = "kv-surrealkv", feature = "kv-rocksdb")))]
        {
            None
        }
    }

    // ---------------------------------------------------------------
    // bulk_load: median of BULK_REPEATS full 100k-node / ~500k-edge loads,
    // measured manually (see BULK_REPEATS). Prints per-backend rows/s +
    // nodes/s the gate doc quotes directly.
    // ---------------------------------------------------------------
    fn bulk_load(runtime: &Runtime) {
        let (nodes, edges, _) = SyntheticGraph::generate_with_landmarks(SEED, BULK_NODES);
        let rows = (nodes.len() + edges.len()) as f64;
        println!(
            "[bulk_load] graph: {} nodes, {} edges ({:.2} edges/node), {} rows total",
            nodes.len(),
            edges.len(),
            edges.len() as f64 / nodes.len() as f64,
            rows as u64
        );

        // In-memory backend.
        let mem_times = runtime.block_on(async {
            let mut times = Vec::new();
            for _ in 0..BULK_REPEATS {
                let store = fresh_mem().await;
                let start = Instant::now();
                let inserted = load(&store, &nodes, &edges).await;
                let dt = start.elapsed();
                assert!(inserted > 0, "edges must load");
                times.push(dt);
            }
            times
        });
        report_bulk("kv-mem", &mem_times, nodes.len(), rows);

        // On-disk backend (surrealkv / rocksdb), fresh tempdir per load.
        #[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
        if let Some(label) = disk_label() {
            let disk_times = runtime.block_on(async {
                let mut times = Vec::new();
                for _ in 0..BULK_REPEATS {
                    let tmp = tempfile::tempdir().expect("tempdir");
                    let dir = tmp.path().join(selene_db::DATABASE_DIRNAME);
                    let store = fresh_disk(&dir).await;
                    let start = Instant::now();
                    let inserted = load(&store, &nodes, &edges).await;
                    let dt = start.elapsed();
                    assert!(inserted > 0, "edges must load");
                    times.push(dt);
                    drop(store);
                }
                times
            });
            report_bulk(label, &disk_times, nodes.len(), rows);
        }
    }

    /// Print the median full-load time + derived throughput for one backend.
    fn report_bulk(backend: &str, times: &[Duration], num_nodes: usize, rows: f64) {
        let mut sorted: Vec<f64> = times.iter().map(Duration::as_secs_f64).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        let best = sorted[0];
        println!(
            "[bulk_load] {backend:<13} loads(s)={:?} median={median:.3}s best={best:.3}s \
             => {:.0} rows/s ({:.0} nodes/s) [median]",
            sorted.iter().map(|s| format!("{s:.3}")).collect::<Vec<_>>(),
            rows / median,
            num_nodes as f64 / median,
        );
    }

    // ---------------------------------------------------------------
    // Per-query benches, on a 20k-node graph loaded once per backend.
    // ---------------------------------------------------------------
    fn queries(c: &mut Criterion, runtime: &Runtime) {
        let (nodes, edges, lm) = SyntheticGraph::generate_with_landmarks(SEED, QUERY_NODES);
        eprintln!(
            "[queries] graph: {} nodes, {} edges; hub={}, deep {}->{}",
            nodes.len(),
            edges.len(),
            lm.hub_id,
            lm.deep_head_id,
            lm.deep_tail_id
        );

        // Pre-load an in-memory store once and bench against it.
        let mem = runtime.block_on(async {
            let store = fresh_mem().await;
            let _ = load(&store, &nodes, &edges).await;
            store
        });
        query_group(c, runtime, "kv-mem", &mem, &lm);

        // Pre-load a disk store once (kept alive in its tempdir for the group).
        #[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
        if let Some(label) = disk_label() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let dir = tmp.path().join(selene_db::DATABASE_DIRNAME);
            let disk = runtime.block_on(async {
                let store = fresh_disk(&dir).await;
                let _ = load(&store, &nodes, &edges).await;
                store
            });
            query_group(c, runtime, label, &disk, &lm);
            drop(disk);
        }
    }

    /// Every per-query bench for one already-loaded backend.
    fn query_group(
        c: &mut Criterion,
        runtime: &Runtime,
        backend: &str,
        store: &SurrealStore,
        lm: &Landmarks,
    ) {
        let mut group = c.benchmark_group("query");
        group.sample_size(30);
        group.warm_up_time(Duration::from_secs(1));
        group.measurement_time(Duration::from_secs(5));

        group.bench_function(BenchmarkId::new("callers_d1", backend), |b| {
            b.to_async(runtime)
                .iter(|| async { store.callers(&lm.hub_id, 1).await.expect("callers") })
        });
        group.bench_function(BenchmarkId::new("callers_d3", backend), |b| {
            b.to_async(runtime)
                .iter(|| async { store.callers(&lm.hub_id, 3).await.expect("callers") })
        });
        group.bench_function(BenchmarkId::new("impact_d3", backend), |b| {
            b.to_async(runtime).iter(|| async {
                store
                    .impact_radius(&lm.deep_tail_id, 3)
                    .await
                    .expect("impact")
            })
        });
        group.bench_function(BenchmarkId::new("impact_d5", backend), |b| {
            b.to_async(runtime).iter(|| async {
                store
                    .impact_radius(&lm.deep_tail_id, 5)
                    .await
                    .expect("impact")
            })
        });
        group.bench_function(BenchmarkId::new("find_path", backend), |b| {
            b.to_async(runtime).iter(|| async {
                store
                    .find_path(&lm.deep_head_id, &lm.deep_tail_id, &[EdgeKind::Calls])
                    .await
                    .expect("find_path")
            })
        });
        group.bench_function(BenchmarkId::new("search_fts", backend), |b| {
            let term = vec![lm.fts_term.clone()];
            b.to_async(runtime)
                .iter(|| async { store.search_fts(&term, &[], &[], 20, 0).await.expect("fts") })
        });
        group.finish();
    }

    pub fn run() {
        let runtime = rt();
        // Bulk load is measured manually (fewer, timed full loads) — see
        // `bulk_load`. The per-query gate numbers stay on criterion.
        //
        // `SELENE_BENCH_ONLY=bulk|query` runs one section — full runs exceed
        // supervised background-command windows, so the gate is collected in
        // two shorter passes.
        let only = std::env::var("SELENE_BENCH_ONLY").unwrap_or_default();
        if only != "query" {
            bulk_load(&runtime);
        }
        if only != "bulk" {
            let mut c = Criterion::default().configure_from_args();
            queries(&mut c, &runtime);
            c.final_summary();
        }
    }
}
