#![allow(clippy::unwrap_used, clippy::expect_used)]
//! **The resolution parity gate** — TS ⇄ Rust edge IDENTITY, tolerance 0.
//!
//! # What it compares, and why not counts
//!
//! Phase 2's gate began by comparing **counts**, and stayed green while class
//! inheritance was unwired in every language, Ruby calls were truncated to their
//! receiver (the counts were *identical*), Python emitted a phantom edge that a
//! passing test had pinned as correct, and PHP imports lacked the only spelling
//! that resolves. Counts cannot see a resolver that binds a reference to the
//! **wrong target** — and that is precisely the failure mode of a *resolver*.
//!
//! So this gate compares the **edge multiset**, on identity:
//!
//! ```text
//! (source, target, kind, provenance) + metadata.synthesizedBy
//! ```
//!
//! # Endpoints compare on SEMANTICS, never on id spelling
//!
//! A TS node id is a literal string (`func:src/a.ts:login:12`); ours is a sha256
//! hash of the same four components. Comparing ids would diff 100% of edges and
//! tell us nothing. Both engines derive their id from `(file, kind, name, line)`,
//! so *that* is the identity:
//!
//! - `"<kind>:<name>@<file>"` — an ordinary symbol.
//! - `"route:<name>@<file>:<line>"` — a route. Its `name` is `"{METHOD} {path}"`,
//!   which `routes.rs` keeps byte-identical to TS **as a wire contract, precisely
//!   so this comparison can exist**. The line is in the key because several routes
//!   legally share one file *and* one line (`get(h).post(h2)`, `resources
//!   :articles`) and are separated only by name — drop it and a deleted verb slips
//!   through, which is the failure this gate exists to catch.
//!
//! **`framework` is deliberately NOT in the route key.** A TS route node has no
//! such field (TS keeps route semantics in the id string; the indexed
//! `framework`/`route_method`/`route_path` fields are SeleneCode's Task-11
//! decision). Folding it in would compare a field we invented against a field TS
//! does not have — our own output against itself, which is a gate that gates
//! nothing. Framework detection is asserted **separately**, by
//! `frameworks_detected_agree`.
//!
//! Do not "fix" any of this by loosening the comparison. Loosening it blinds the
//! gate exactly where dispatch bridging lives.
//!
//! # Regenerating the baseline
//!
//! ```bash
//! cd ../codegraph && npx vite-node \
//!   <selene>/tools/parity/dump-ts-resolution.mjs \
//!   <selene>/crates/selene-resolve/tests/fixtures/dispatch \
//!   <selene>/crates/selene-resolve/tests/fixtures/dispatch/expected.json
//! ```
//!
//! # This gate drives the PRODUCTION driver
//!
//! [`resolve_project`] calls `resolve_and_persist_batched` — the same entry point an
//! indexer calls. It used to compose the pipeline itself, which is exactly how a gate can
//! be green over a product that has no pipeline at all: this crate shipped FOUR seams
//! whose unit tests passed while nothing invoked them, and a test-composed pipeline is
//! structurally blind to a fifth. The ordering contract now lives in `batch.rs`, where
//! production reads it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use selene_core::{EdgeKind, Node, NodeKind};
use selene_db::SurrealStore;
use selene_extract::Indexer;
use selene_resolve::StoreContext;
use selene_resolve::frameworks::detect_frameworks;
use serde::Deserialize;

// =============================================================================
// The baseline
// =============================================================================

#[derive(Debug, Deserialize)]
struct Baseline {
    #[serde(rename = "codegraphCommit")]
    codegraph_commit: String,
    projects: BTreeMap<String, ProjectBaseline>,
}

#[derive(Debug, Deserialize)]
struct ProjectBaseline {
    edges: Vec<EdgeRow>,
    nodes: usize,
    #[serde(rename = "crossFileEdges")]
    cross_file_edges: usize,
    routes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
struct EdgeRow {
    source: String,
    target: String,
    kind: String,
    provenance: String,
    #[serde(rename = "synthesizedBy")]
    synthesized_by: Option<String>,
}

impl EdgeRow {
    fn key(&self) -> String {
        format!(
            "{} -[{}|{}{}]-> {}",
            self.source,
            self.kind,
            self.provenance,
            self.synthesized_by
                .as_deref()
                .map(|s| format!("|{s}"))
                .unwrap_or_default(),
            self.target,
        )
    }
}

#[derive(Debug, Deserialize)]
struct Deviations {
    #[serde(default)]
    deviation: Vec<Deviation>,
}

/// A justified divergence. **Machine-checked from both sides**: the gate ignores the
/// edge it names, and FAILS if the entry matches no observed difference — a fixed
/// divergence must not leave a permanent whitelist behind.
#[derive(Debug, Clone, Deserialize)]
struct Deviation {
    project: String,
    /// `"rust"` — we emit it, TS does not. `"ts"` — TS emits it, we do not.
    side: String,
    edge: String,
    #[allow(dead_code)] // read by humans; its presence is the point
    reason: String,
}

fn load_deviations() -> Vec<Deviation> {
    let raw =
        std::fs::read_to_string(corpus_dir().join("deviations.toml")).expect("deviations.toml");
    toml::from_str::<Deviations>(&raw)
        .expect("deviations.toml is not valid TOML")
        .deviation
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch")
}

fn load_baseline() -> Baseline {
    let raw = std::fs::read_to_string(corpus_dir().join("expected.json"))
        .expect("expected.json — regenerate it with tools/parity/dump-ts-resolution.mjs");
    serde_json::from_str(&raw).expect("expected.json is not valid baseline JSON")
}

fn projects_on_disk() -> BTreeSet<String> {
    std::fs::read_dir(corpus_dir())
        .expect("corpus dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && !n.starts_with('_'))
        .collect()
}

// =============================================================================
// The Rust side
// =============================================================================

/// The semantic key of a node — see the module docs. Must agree, byte for byte,
/// with `endpointKey()` in `tools/parity/dump-ts-resolution.mjs`.
fn endpoint_key(n: &Node) -> String {
    match n.kind {
        NodeKind::Route => format!("route:{}@{}:{}", n.name, n.file_path, n.start_line),
        NodeKind::File => format!("file:@{}", n.file_path),
        k => format!("{}:{}@{}", k.as_str(), n.name, n.file_path),
    }
}

/// Index, then run **THE PRODUCTION DRIVER**, then read the whole graph back.
///
/// This used to be the driver, hand-composed in the test — which is precisely how a gate
/// can be green over a product that has no pipeline at all. It now calls
/// `resolve_and_persist_batched`: the same entry point an indexer calls, with the pass
/// order, the batch loop, the keyed delete and the synthesis tail that live in `batch.rs`.
async fn resolve_project(dir: &Path) -> (Vec<EdgeRow>, Vec<String>, usize) {
    let store = SurrealStore::in_memory().await.expect("store");
    store.apply_schema().await.expect("schema");

    let indexer = Indexer::new(dir.to_path_buf(), store);
    let __ix = indexer.index_all(None).await;
    let result = &__ix;
    assert!(
        result.files_indexed > 0,
        "{dir:?} indexed ZERO files — the gate would be comparing nothing"
    );
    let store = indexer.into_store();

    let stats =
        selene_resolve::resolve_and_persist_in_memory(&store, dir, __ix.unresolved.clone(), None)
            .await
            .expect("the driver must never fail an index");
    assert_eq!(
        stats.store_read_errors, 0,
        "{dir:?}: the driver swallowed {} store read error(s). A store outage is otherwise \
         byte-identical to a repo with nothing to resolve — which would make this gate \
         green by comparing two empty sets.",
        stats.store_read_errors
    );

    // The frameworks the DRIVER detected (it does its own detection — asserting on a list
    // the test injected would be asserting on the test).
    let ctx = StoreContext::new(store.clone(), dir.to_path_buf())
        .await
        .expect("ctx");
    let framework_names: Vec<String> = detect_frameworks(&ctx)
        .iter()
        .map(|f| f.name().to_string())
        .collect();

    // --- read the whole graph back, and key it semantically -------------------
    let mut nodes: Vec<Node> = Vec::new();
    for kind in NodeKind::ALL {
        // STRUCTURAL EXCLUSION (doc-ingestion PRD 2026-08-14 §8.1): the gate's
        // object is TS↔Rust RESOLUTION parity, and the TS baseline predates
        // document ingestion — the corpus's requirements.txt files now index
        // as Document/Section, which the baseline cannot contain. Excluding
        // the documentary kinds here also drops their edges (the by_id
        // endpoint filter below). Documented in deviations.toml's header.
        if matches!(kind, NodeKind::Document | NodeKind::Section) {
            continue;
        }
        nodes.extend(store.get_nodes_by_kind(kind).await.expect("by kind"));
    }
    let by_id: BTreeMap<String, Node> = nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();
    let ids: Vec<String> = by_id.keys().cloned().collect();

    let out = store
        .outgoing_batch(&ids, &EdgeKind::ALL)
        .await
        .expect("outgoing");

    let mut rows = Vec::new();
    for (src_id, neighbors) in &out {
        let Some(src) = by_id.get(src_id) else {
            continue;
        };
        for n in neighbors {
            let Some(dst) = by_id.get(&n.node.id) else {
                continue;
            };
            let synthesized_by = n
                .edge
                .metadata
                .as_ref()
                .and_then(|m| m.get("synthesizedBy"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            rows.push(EdgeRow {
                source: endpoint_key(src),
                target: endpoint_key(dst),
                kind: n.edge.kind.as_str().to_string(),
                // The wire spelling, not the Rust identifier — the baseline carries TS's.
                provenance: n
                    .edge
                    .provenance
                    .map(|p| {
                        serde_json::to_value(p)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    })
                    .unwrap_or_else(|| "tree-sitter".to_string()),
                synthesized_by,
            });
        }
    }
    rows.sort();
    (rows, framework_names, stats.total)
}

// =============================================================================
// The structural assertions — Phase 2 learned every one of these the hard way
// =============================================================================

/// A fixture added without regenerating the baseline must **FAIL**, not be
/// silently ungated. (Phase 2 shipped with exactly this hole.)
#[test]
fn every_project_on_disk_is_gated() {
    let baseline = load_baseline();
    let on_disk = projects_on_disk();
    let in_baseline: BTreeSet<String> = baseline.projects.keys().cloned().collect();

    assert_eq!(
        on_disk,
        in_baseline,
        "the corpus and the baseline disagree.\n  on disk, ungated: {:?}\n  in baseline, missing from disk: {:?}\n\
         A fixture nobody gates is a fixture that proves nothing. Regenerate the baseline.",
        on_disk.difference(&in_baseline).collect::<Vec<_>>(),
        in_baseline.difference(&on_disk).collect::<Vec<_>>(),
    );
}

/// A baseline of zeros is reproduced perfectly by a Rust side that is equally
/// broken — the gate is then green forever, having compared nothing.
#[test]
fn baseline_is_not_vacuous() {
    let baseline = load_baseline();

    assert_ne!(
        baseline.codegraph_commit, "unknown",
        "the baseline does not record which codegraph it came from — a stale \
         baseline would be undetectable"
    );
    assert!(
        !baseline.projects.is_empty(),
        "an empty corpus gates nothing"
    );

    for (name, p) in &baseline.projects {
        assert!(p.nodes > 0, "{name}: zero nodes");
        assert!(!p.edges.is_empty(), "{name}: zero edges");

        // The `*-control` projects are the PRECISION corpus: ordinary code containing
        // NONE of the dispatch shapes, whose whole purpose is to prove synthesis emits
        // zero edges on them. They legitimately have no cross-file edges — demanding
        // some would force dispatch shapes into the very fixture that exists to have
        // none. They are not exempt from being gated; they are gated on the opposite
        // property.
        if name.ends_with("-control") {
            let synthesized: Vec<&EdgeRow> = p
                .edges
                .iter()
                .filter(|e| e.provenance == "heuristic")
                .collect();
            assert!(
                synthesized.is_empty(),
                "{name} is a CONTROL fixture and the baseline gives it {} synthesized \
                 edge(s): {synthesized:?}\n\
                 Every positive assertion in this phase is satisfied by a synthesizer \
                 that bridges EVERYTHING. Only a control catches one — and this control \
                 says a channel is guessing.",
                synthesized.len()
            );
            continue;
        }

        assert!(
            p.cross_file_edges > 0,
            "{name}: ZERO cross-file edges. Resolution's entire output is edges \
             BETWEEN files; a project with none of them gates nothing at all."
        );
    }

    let total: usize = baseline.projects.values().map(|p| p.edges.len()).sum();
    let routes: usize = baseline.projects.values().map(|p| p.routes).sum();
    assert!(total >= 100, "only {total} edges in the whole corpus");
    assert!(
        routes >= 10,
        "only {routes} route nodes — the frameworks are not firing"
    );
}

/// The differ must actually see a difference. A comparison that cannot fail is
/// the most expensive kind of green.
#[test]
fn the_differ_catches_a_planted_mismatch() {
    let real = EdgeRow {
        source: "function:login@src/service.ts".into(),
        target: "function:hashPassword@src/crypto.ts".into(),
        kind: "calls".into(),
        provenance: "tree-sitter".into(),
        synthesized_by: None,
    };

    // A WRONG TARGET — the failure counts cannot see, and the whole reason this
    // gate compares identity.
    let wrong_target = EdgeRow {
        target: "function:hashPassword@src/other.ts".into(),
        ..real.clone()
    };
    assert_ne!(real.key(), wrong_target.key(), "a wrong target must diff");

    // Same endpoints, different KIND (a `calls` promoted to `instantiates`).
    let wrong_kind = EdgeRow {
        kind: "instantiates".into(),
        ..real.clone()
    };
    assert_ne!(real.key(), wrong_kind.key(), "a wrong kind must diff");

    // Same edge, but claimed as a synthesized one.
    let wrong_prov = EdgeRow {
        provenance: "heuristic".into(),
        synthesized_by: Some("callback".into()),
        ..real.clone()
    };
    assert_ne!(real.key(), wrong_prov.key(), "a wrong provenance must diff");

    // And two routes on ONE line differ only by name — the collision that silently
    // deletes a verb.
    let get = "route:GET /x@src/main.rs:9";
    let post = "route:POST /x@src/main.rs:9";
    assert_ne!(
        get, post,
        "same file, same line — only the name separates them"
    );
}

// =============================================================================
// THE GATE
// =============================================================================

/// Framework detection must agree. A framework whose `detect()` silently returns
/// `false` emits no routes, so no route edges, so **both** engines dump an empty
/// set and a pure diff is green. This is the sharpest anti-vacuity check there is.
#[tokio::test(flavor = "multi_thread")]
async fn frameworks_detected_agree() {
    let baseline = load_baseline();
    let mut missing = Vec::new();

    for (name, p) in &baseline.projects {
        let (_, detected, _) = resolve_project(&corpus_dir().join(name)).await;
        if p.routes > 0 && detected.is_empty() {
            missing.push(format!(
                "{name}: TS emitted {} routes; Rust detected NO framework",
                p.routes
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "framework detection disagrees:\n  {}",
        missing.join("\n  ")
    );
}

/// **The gate.** Tolerance zero, on edge identity.
#[tokio::test(flavor = "multi_thread")]
async fn ts_rust_resolution_edge_parity() {
    let baseline = load_baseline();
    let deviations = load_deviations();
    let mut used = vec![false; deviations.len()];

    let mut report = String::new();
    let mut only_ts_total = 0usize;
    let mut only_rust_total = 0usize;
    let mut matched_total = 0usize;

    for (name, p) in &baseline.projects {
        let (rust_edges, _, _) = resolve_project(&corpus_dir().join(name)).await;

        let ts: BTreeSet<String> = p.edges.iter().map(EdgeRow::key).collect();
        let rust: BTreeSet<String> = rust_edges.iter().map(EdgeRow::key).collect();

        // A deviation excuses exactly the edge it names, in the project it names, on
        // the side it names — and is marked USED, so an entry that excuses nothing
        // fails the gate below as stale.
        let mut excuse = |side: &str, edge: &String| -> bool {
            deviations
                .iter()
                .position(|d| d.project == *name && d.side == side && d.edge == *edge)
                .map(|i| {
                    used[i] = true;
                    true
                })
                .unwrap_or(false)
        };

        let only_ts: Vec<&String> = ts.difference(&rust).filter(|e| !excuse("ts", e)).collect();
        let only_rust: Vec<&String> = rust
            .difference(&ts)
            .filter(|e| !excuse("rust", e))
            .collect();
        matched_total += ts.intersection(&rust).count();
        only_ts_total += only_ts.len();
        only_rust_total += only_rust.len();

        if !only_ts.is_empty() || !only_rust.is_empty() {
            report.push_str(&format!(
                "\n{name}: TS {} edges, Rust {} — {} missing, {} extra\n",
                ts.len(),
                rust.len(),
                only_ts.len(),
                only_rust.len()
            ));
            for e in only_ts.iter().take(12) {
                report.push_str(&format!("  - TS ONLY : {e}\n"));
            }
            for e in only_rust.iter().take(12) {
                report.push_str(&format!("  + RUST ONLY: {e}\n"));
            }
        }
    }

    // A stale deviation is a whitelist nobody pruned — the way a gate quietly stops
    // gating. It fails the gate exactly as loudly as a real diff.
    let stale: Vec<&Deviation> = deviations
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|(d, _)| d)
        .collect();
    assert!(
        stale.is_empty(),
        "STALE DEVIATIONS — each of these excuses a difference that no longer exists. \
         Delete them; a whitelist nobody prunes is how a gate stops gating.\n{}",
        stale
            .iter()
            .map(|d| format!("  {} [{}] {}", d.project, d.side, d.edge))
            .collect::<Vec<_>>()
            .join("\n")
    );

    assert!(
        report.is_empty(),
        "RESOLUTION PARITY FAILED — {matched_total} matched, {only_ts_total} missing \
         (TS has, we do not), {only_rust_total} extra (we invented).\n\
         \n\
         A missing edge is a flow the agent cannot follow. An EXTRA edge is worse: it \
         is a wrong answer the agent will trust.\n{report}"
    );
}
