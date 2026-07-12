//! Orchestrator conformance tests (Task 18): the brief's TDD list run
//! against a tempfile mini-project and the in-memory SurrealStore —
//! counts, oversized skip, second-run skip + id stability, single-file
//! incremental re-index with cross-file edge re-attachment, determinism
//! across fresh stores, the extraction-version note, progress phases, and
//! the deep-nesting recursion guard (Task 5 review rider).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Mutex;

use selene_core::{Edge, EdgeKind, NodeKind, node_id};
use selene_db::SurrealStore;
use selene_extract::{Indexer, Phase, Severity};

/// Write the standard mini-project: 3 extractable languages (python, ts,
/// js — the ones `rules_for` knows at this branch base), one file inside a
/// default-ignored dir, and one oversized file.
fn write_mini_project(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    std::fs::write(
        root.join("src/app.py"),
        "from helper import greet\n\nclass App:\n    def run(self):\n        return greet()\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/util.ts"),
        "export function double(n: number): number {\n  return n * 2;\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/legacy.js"),
        "function legacy() {\n  return double(21);\n}\n",
    )
    .unwrap();
    // Default-ignored dir: never scanned.
    std::fs::write(root.join("node_modules/pkg/x.js"), "function hidden() {}\n").unwrap();
    // Oversized: MAX_FILE_SIZE is 1 MiB.
    std::fs::write(root.join("src/big.py"), "#".repeat(1_048_577)).unwrap();
}

async fn fresh_indexer(root: PathBuf) -> Indexer<SurrealStore> {
    let store = SurrealStore::in_memory().await.unwrap();
    Indexer::new(root, store)
}

/// Every non-file node id in the store, per scanned file, sorted.
async fn all_node_ids(indexer: &Indexer<SurrealStore>, rels: &[&str]) -> Vec<String> {
    let mut ids = Vec::new();
    for rel in rels {
        for n in indexer.store().get_nodes_by_file(rel).await.unwrap() {
            ids.push(n.id);
        }
    }
    ids.sort();
    ids
}

const SCANNED: [&str; 4] = ["src/app.py", "src/big.py", "src/legacy.js", "src/util.ts"];

// =============================================================================
// index_all: counts, oversized skip, ignored dir
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn index_all_counts_and_oversized_skip() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;

    let r = indexer.index_all(None).await;

    // 4 discovered (the ignored dir never surfaces), 3 indexed, 1 skipped
    // (oversized, with a size_exceeded WARNING), none errored.
    assert_eq!(r.files_discovered, 4, "errors: {:?}", r.errors);
    assert_eq!(r.files_indexed, 3);
    assert_eq!(r.files_skipped, 1);
    assert_eq!(r.files_errored, 0);
    assert!(r.success);
    assert!(r.errors.iter().any(|e| {
        e.severity == Severity::Warning
            && e.file_path.as_deref() == Some("src/big.py")
            && e.message.contains("MAX_FILE_SIZE")
    }));

    // Store contents match the result counters and the extractions.
    let stats = indexer.store().stats().await.unwrap();
    assert_eq!(stats.files, 3, "oversized file not committed");
    assert_eq!(stats.nodes, r.nodes_created);
    assert!(r.nodes_created > 0);
    // The python class + method arrived.
    let app_nodes = indexer
        .store()
        .get_nodes_by_file("src/app.py")
        .await
        .unwrap();
    assert!(
        app_nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "App")
    );
    assert!(
        app_nodes
            .iter()
            .any(|n| n.kind == NodeKind::Method && n.name == "run")
    );
    // The version was persisted after the successful run.
    assert_eq!(
        indexer
            .store()
            .get_meta("extraction_version")
            .await
            .unwrap(),
        Some(selene_core::EXTRACTION_VERSION.to_string())
    );
}

// =============================================================================
// Second run: all-skipped, ids byte-identical
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn second_run_skips_everything_and_ids_are_stable() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;

    let first = indexer.index_all(None).await;
    assert_eq!(first.files_indexed, 3);
    let ids_before = all_node_ids(&indexer, &SCANNED).await;
    assert!(!ids_before.is_empty());

    let second = indexer.index_all(None).await;
    assert_eq!(second.files_indexed, 0);
    // 3 unchanged-hash skips + the oversized skip.
    assert_eq!(second.files_skipped, 4);
    assert_eq!(second.files_errored, 0);
    assert!(
        second.success,
        "no severity-Error errors on an all-skip run"
    );
    assert_eq!(second.nodes_created, 0);

    let ids_after = all_node_ids(&indexer, &SCANNED).await;
    assert_eq!(ids_before, ids_after, "node ids must be byte-identical");
}

// =============================================================================
// index_file: unchanged no-op, line-shift re-index, edge re-attachment
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn index_file_reindexes_only_the_touched_file_and_reattaches_edges() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("caller.py"),
        "def call_it():\n    return helper()\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("lib.py"), "def helper():\n    return 1\n").unwrap();
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;
    let r = indexer.index_all(None).await;
    assert_eq!(r.files_indexed, 2, "errors: {:?}", r.errors);

    let caller_id = node_id("caller.py", NodeKind::Function, "call_it", 1);
    let helper_v1 = node_id("lib.py", NodeKind::Function, "helper", 1);

    // Simulate the resolver: a cross-file calls edge caller → helper.
    let inserted = indexer
        .store()
        .insert_edges(&[Edge {
            source: caller_id.clone(),
            target: helper_v1.clone(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: Some(2),
            column: None,
            provenance: None,
        }])
        .await
        .unwrap();
    assert_eq!(inserted, 1);

    // Unchanged file ⇒ no-op (hash pre-check), nothing extracted.
    let noop = indexer.index_file("lib.py").await.unwrap();
    assert!(noop.nodes.is_empty() && noop.errors.is_empty());

    // Shift helper down one line: every lib.py node id churns.
    std::fs::write(
        tmp.path().join("lib.py"),
        "# shifted\ndef helper():\n    return 1\n",
    )
    .unwrap();
    let re = indexer.index_file("lib.py").await.unwrap();
    assert!(re.errors.is_empty(), "{:?}", re.errors);
    let helper_v2 = node_id("lib.py", NodeKind::Function, "helper", 2);
    assert!(re.nodes.iter().any(|n| n.id == helper_v2));

    // The caller's file was NOT re-indexed (its node id unchanged)…
    assert!(
        indexer
            .store()
            .get_node(&caller_id)
            .await
            .unwrap()
            .is_some(),
        "caller.py must be untouched"
    );
    // …and the cross-file incoming edge re-attached onto the NEW helper id
    // (the replace_file_extraction snapshot/reattach protocol).
    let out = indexer
        .store()
        .outgoing(&caller_id, &[EdgeKind::Calls], None)
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].node.id, helper_v2, "edge must follow the id churn");
    assert!(
        indexer
            .store()
            .get_node(&helper_v1)
            .await
            .unwrap()
            .is_none(),
        "the old helper node is gone"
    );
}

// =============================================================================
// Determinism: two fresh stores, identical stats + id sets
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn two_fresh_runs_are_identical() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());

    let a = fresh_indexer(tmp.path().to_path_buf()).await;
    let b = fresh_indexer(tmp.path().to_path_buf()).await;
    let ra = a.index_all(None).await;
    let rb = b.index_all(None).await;

    assert_eq!(ra.files_indexed, rb.files_indexed);
    assert_eq!(ra.nodes_created, rb.nodes_created);
    assert_eq!(ra.edges_created, rb.edges_created);

    let sa = a.store().stats().await.unwrap();
    let sb = b.store().stats().await.unwrap();
    assert_eq!(sa, sb, "stats must be identical across fresh runs");
    assert_eq!(
        all_node_ids(&a, &SCANNED).await,
        all_node_ids(&b, &SCANNED).await,
        "id sets must be identical across fresh runs"
    );
}

// =============================================================================
// Recursion/depth guard (Task 5 review rider): adversarial nesting is a
// collected error, never a crash.
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn adversarial_nesting_is_an_error_not_a_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let mut evil = String::with_capacity(220_000);
    evil.push_str("x = ");
    evil.push_str(&"(".repeat(100_000));
    evil.push('0');
    evil.push_str(&")".repeat(100_000));
    evil.push('\n');
    std::fs::write(tmp.path().join("evil.py"), evil).unwrap();
    std::fs::write(tmp.path().join("fine.py"), "def ok():\n    return 1\n").unwrap();

    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;
    let r = indexer.index_all(None).await;

    assert_eq!(r.files_discovered, 2);
    assert_eq!(r.files_indexed, 1, "the healthy file still indexes");
    assert_eq!(r.files_errored, 1);
    assert!(r.errors.iter().any(|e| {
        e.severity == Severity::Error
            && e.file_path.as_deref() == Some("evil.py")
            && e.message.contains("MAX_NESTING_DEPTH")
    }));
    // The pipeline survived and success reflects the partial index.
    assert!(r.success, "files_indexed > 0 ⇒ success");
    // The guarded file is committed with its error (so its hash skips next
    // run) but contributes no symbols.
    let evil_nodes = indexer.store().get_nodes_by_file("evil.py").await.unwrap();
    assert!(evil_nodes.is_empty());
    let file_rec = indexer.store().get_file("evil.py").await.unwrap().unwrap();
    assert_eq!(file_rec.node_count, 0);
    assert!(!file_rec.errors.is_empty());
}

// =============================================================================
// Extraction-version note + progress phases
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn stale_extraction_version_yields_a_note_never_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.py"), "def f():\n    return 1\n").unwrap();

    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store.set_meta("extraction_version", "0").await.unwrap();
    let indexer = Indexer::new(tmp.path().to_path_buf(), store);

    let r = indexer.index_all(None).await;
    assert!(r.success);
    assert!(
        r.notes.iter().any(|n| n.contains("re-index recommended")),
        "notes: {:?}",
        r.notes
    );
    assert!(
        !r.errors.iter().any(|e| e.severity == Severity::Error),
        "the version note must never be an error"
    );
    // A successful run bumps the persisted version.
    assert_eq!(
        indexer
            .store()
            .get_meta("extraction_version")
            .await
            .unwrap(),
        Some(selene_core::EXTRACTION_VERSION.to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_reports_all_three_phases_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;

    type Tick = (Phase, usize, usize, Option<String>);
    let seen: Mutex<Vec<Tick>> = Mutex::new(Vec::new());
    let cb = |p: &selene_extract::IndexProgress| {
        seen.lock()
            .unwrap()
            .push((p.phase, p.current, p.total, p.current_file.clone()));
    };
    let r = indexer.index_all(Some(&cb)).await;
    assert!(r.success);

    let ticks = seen.into_inner().unwrap();
    assert_eq!(ticks.first().map(|t| t.0), Some(Phase::Scanning));
    assert!(ticks.iter().any(|t| t.0 == Phase::Parsing));
    assert!(ticks.iter().any(|t| t.0 == Phase::Storing));
    // Parsing ticks carry the file and count up to the discovered total.
    let last_parse = ticks.iter().rfind(|t| t.0 == Phase::Parsing).unwrap();
    assert_eq!(last_parse.1, 4);
    assert_eq!(last_parse.2, 4);
    assert!(last_parse.3.is_some());
}

/// Storing ticks carry the file's ordinal in scan order — monotonic and
/// correct even though this batch contains a file that never reaches the
/// commit step (the oversized `src/big.py`, 2nd of 4 in sorted order).
#[tokio::test(flavor = "multi_thread")]
async fn storing_progress_is_monotonic_with_skipped_reads_in_the_batch() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;

    let seen: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());
    let cb = |p: &selene_extract::IndexProgress| {
        if p.phase == Phase::Storing {
            seen.lock()
                .unwrap()
                .push((p.current, p.current_file.clone().unwrap_or_default()));
        }
    };
    assert!(indexer.index_all(Some(&cb)).await.success);

    let ticks = seen.into_inner().unwrap();
    assert!(
        ticks.windows(2).all(|w| w[0].0 < w[1].0),
        "Storing `current` must be strictly increasing: {ticks:?}"
    );
    assert!(ticks.iter().all(|t| t.0 >= 1 && t.0 <= 4), "{ticks:?}");
    // Each committed file reports its own 1-based position in SCANNED — big.py
    // (position 2) is read-skipped, so 2 is simply absent, never re-used.
    for (current, file) in &ticks {
        assert_eq!(
            SCANNED[current - 1],
            file,
            "tick {current} must name the file at that scan position: {ticks:?}"
        );
    }
}

// =============================================================================
// Constructors: `try_new` surfaces pool-build failure as a Result (review,
// Minor 1) — `new` defers the same failure into the run's errors, never panics.
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn try_new_builds_the_pool_eagerly_and_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());
    let store = SurrealStore::in_memory().await.unwrap();

    let indexer = Indexer::try_new(tmp.path().to_path_buf(), store)
        .expect("the parse pool builds on any sane host");
    let r = indexer.index_all(None).await;
    assert_eq!(r.files_indexed, 3, "errors: {:?}", r.errors);
    assert!(r.success);
}

// =============================================================================
// index_files + path traversal refusal
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn index_files_indexes_exactly_the_given_paths() {
    let tmp = tempfile::tempdir().unwrap();
    write_mini_project(tmp.path());
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;
    // Initialize the store (index_files skips bulk-mode bookkeeping).
    indexer.store().bulk_load_begin().await.unwrap();
    indexer.store().bulk_load_finish().await.unwrap();

    let r = indexer
        .index_files(&["src/app.py".to_string(), "src/util.ts".to_string()])
        .await;
    assert_eq!(r.files_discovered, 2);
    assert_eq!(r.files_indexed, 2, "errors: {:?}", r.errors);
    assert!(
        indexer
            .store()
            .get_file("src/legacy.js")
            .await
            .unwrap()
            .is_none(),
        "unlisted files must not be touched"
    );
}

// =============================================================================
// Bulk-mode leak (review, Important 1): every `index_all` exit re-enters
// search-ready state — a failing scan must never strand the store in
// deferred-FTS mode (indexes DROPPED ⇒ search_fts silently empty).
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_scan_leaves_the_store_searchable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("beacon.py"),
        "def beacon_symbol():\n    return 1\n",
    )
    .unwrap();

    let indexer = fresh_indexer(root.clone()).await;
    let first = indexer.index_all(None).await;
    assert_eq!(first.files_indexed, 1, "errors: {:?}", first.errors);
    // Baseline: after a normal run the FULLTEXT indexes are rebuilt and serve
    // the symbol (the `identifier` analyzer splits `beacon_symbol`).
    let before = indexer
        .store()
        .search_fts(&["beacon".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();
    assert!(!before.is_empty(), "FTS must serve the symbol after a run");

    // The scan root vanishes ⇒ `scan_directory` errors on the next run.
    std::fs::remove_dir_all(&root).unwrap();
    let second = indexer.index_all(None).await;
    assert!(!second.success, "a failed scan is a failed run");
    assert!(
        second.errors.iter().any(|e| {
            e.code == selene_extract::ErrorCode::ReadError && e.message.contains("scan failed")
        }),
        "errors: {:?}",
        second.errors
    );

    // The point: the failed run must NOT have dropped the FTS indexes and
    // walked away — search_fts is success-shaped-empty in bulk mode, so a
    // leak here is silent and product-breaking (PRD §8.2).
    let after = indexer
        .store()
        .search_fts(&["beacon".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();
    assert!(
        !after.is_empty(),
        "a failed scan left the store in bulk-load mode: search_fts is silently empty"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn path_traversal_is_refused_with_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.py"), "def f():\n    return 1\n").unwrap();
    let indexer = fresh_indexer(tmp.path().to_path_buf()).await;
    indexer.store().bulk_load_begin().await.unwrap();

    let r = indexer.index_file("../outside.py").await.unwrap();
    assert!(r.nodes.is_empty());
    assert!(
        r.errors
            .iter()
            .any(|e| e.code == selene_extract::ErrorCode::PathTraversal)
    );
}
