//! PRD §5.3 benchmark gate — bulk load (inline-FTS and deferred-FTS modes) +
//! deep traversal + FTS, measured on the in-memory (`kv-mem`) and the
//! compiled-in on-disk SurrealDB backend (`kv-rocksdb` by default;
//! `kv-surrealkv` when that feature is enabled, since `open()` prefers it).
//!
//! Run with the generator feature on:
//!
//! ```bash
//! cargo bench -p selene-db --features bench-support          # kv-mem + kv-rocksdb
//! cargo bench -p selene-db --features bench-support,kv-surrealkv  # kv-mem + surrealkv
//! ```
//!
//! `SELENE_BENCH_ONLY=bulk|query` runs one section; `SELENE_BULK_REPEATS=n`
//! overrides the full-load repeat count (the §5.3 post-fix re-measure uses 1).
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

    /// [`BULK_REPEATS`], overridable via `SELENE_BULK_REPEATS` (≥ 1). The
    /// §5.3 post-fix re-measure runs a single repeat per backend/mode — the
    /// results doc records it as 1-repeat.
    fn bulk_repeats() -> usize {
        std::env::var("SELENE_BULK_REPEATS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n: &usize| *n > 0)
            .unwrap_or(BULK_REPEATS)
    }
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

    /// One deferred-FTS full load (`bulk_load_begin` → nodes → edges →
    /// `bulk_load_finish`), returning `(total, nodes, edges, fts_build)`
    /// wall times. `total` starts before `bulk_load_begin` so the mode's
    /// whole cost — including the post-load index build — is in one number.
    async fn load_deferred(
        store: &SurrealStore,
        nodes: &[Node],
        edges: &[Edge],
    ) -> (Duration, Duration, Duration, Duration) {
        let start = Instant::now();
        store.bulk_load_begin().await.expect("bulk_load_begin");
        let t = Instant::now();
        store.insert_nodes(nodes).await.expect("insert_nodes");
        let nodes_dt = t.elapsed();
        let t = Instant::now();
        let inserted = store.insert_edges(edges).await.expect("insert_edges");
        assert!(inserted > 0, "edges must load");
        let edges_dt = t.elapsed();
        let t = Instant::now();
        store.bulk_load_finish().await.expect("bulk_load_finish");
        (start.elapsed(), nodes_dt, edges_dt, t.elapsed())
    }

    // ---------------------------------------------------------------
    // bulk_load: median of bulk_repeats() full 100k-node / ~500k-edge loads,
    // measured manually (see BULK_REPEATS), in both modes — inline FTS
    // (apply_schema only) and deferred FTS (bulk_load_begin/finish). Prints
    // per-backend rows/s + nodes/s the gate doc quotes directly.
    // ---------------------------------------------------------------
    fn bulk_load(runtime: &Runtime) {
        let repeats = bulk_repeats();
        let (nodes, edges, _) = SyntheticGraph::generate_with_landmarks(SEED, BULK_NODES);
        let rows = (nodes.len() + edges.len()) as f64;
        println!(
            "[bulk_load] graph: {} nodes, {} edges ({:.2} edges/node), {} rows total; \
             {repeats} repeat(s) per backend/mode",
            nodes.len(),
            edges.len(),
            edges.len() as f64 / nodes.len() as f64,
            rows as u64
        );

        // In-memory backend, inline-FTS then deferred-FTS.
        let mem_times = runtime.block_on(async {
            let mut times = Vec::new();
            for _ in 0..repeats {
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
        let mem_deferred = runtime.block_on(async {
            let mut times = Vec::new();
            for _ in 0..repeats {
                let store = SurrealStore::in_memory().await.expect("in_memory");
                times.push(load_deferred(&store, &nodes, &edges).await);
            }
            times
        });
        report_bulk_deferred("kv-mem", &mem_deferred, nodes.len(), rows);

        // On-disk backend (rocksdb / surrealkv), fresh tempdir per load.
        #[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
        if let Some(label) = disk_label() {
            let disk_times = runtime.block_on(async {
                let mut times = Vec::new();
                for _ in 0..repeats {
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
            let disk_deferred = runtime.block_on(async {
                let mut times = Vec::new();
                for _ in 0..repeats {
                    let tmp = tempfile::tempdir().expect("tempdir");
                    let dir = tmp.path().join(selene_db::DATABASE_DIRNAME);
                    let store = SurrealStore::open(&dir).await.expect("open disk store");
                    times.push(load_deferred(&store, &nodes, &edges).await);
                    drop(store);
                }
                times
            });
            report_bulk_deferred(label, &disk_deferred, nodes.len(), rows);
        }
    }

    /// Print each deferred-mode repeat's phase split, then the median totals.
    /// `nodes/s` is quoted over the node-insert phase alone (the §5.3 docs'
    /// deferred-mode metric — the phase FTS deferral actually accelerates);
    /// `rows/s` stays over the full load for comparability with
    /// [`report_bulk`].
    fn report_bulk_deferred(
        backend: &str,
        times: &[(Duration, Duration, Duration, Duration)],
        num_nodes: usize,
        rows: f64,
    ) {
        for (i, (total, n, e, fts)) in times.iter().enumerate() {
            println!(
                "[bulk_load] {backend}/deferred repeat {}: nodes={:.3}s ({:.0} nodes/s) \
                 edges={:.3}s fts_build={:.3}s total={:.3}s",
                i + 1,
                n.as_secs_f64(),
                num_nodes as f64 / n.as_secs_f64(),
                e.as_secs_f64(),
                fts.as_secs_f64(),
                total.as_secs_f64(),
            );
        }
        let mut totals: Vec<f64> = times.iter().map(|(t, ..)| t.as_secs_f64()).collect();
        totals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut node_phases: Vec<f64> = times.iter().map(|(_, n, ..)| n.as_secs_f64()).collect();
        node_phases.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_total = totals[totals.len() / 2];
        let median_nodes = node_phases[node_phases.len() / 2];
        println!(
            "[bulk_load] {backend}/deferred median: total={median_total:.3}s => {:.0} rows/s \
             ({:.0} nodes/s over the node phase)",
            rows / median_total,
            num_nodes as f64 / median_nodes,
        );
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
