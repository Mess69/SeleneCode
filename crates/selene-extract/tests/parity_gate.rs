//! **THE Phase 2 gate** (Task 19): TS↔Rust extraction count parity on a shared
//! fixture corpus.
//!
//! Both engines consume the *same bytes*: `tests/fixtures/parity/<lang>/…` are
//! real files on disk, materialized from the inline snippets in CodeGraph's
//! `__tests__/extraction.test.ts`. The TS side's counts are dumped once by
//! `tools/parity/dump-ts-extraction.mjs` into `expected.json` (which records the
//! codegraph commit it came from); this test re-extracts every fixture with the
//! Rust port and asserts the counters match.
//!
//! ## Tolerance is 0, deliberately
//!
//! Extraction is deterministic over byte-identical inputs, so ANY drift is
//! either a config bug or a grammar-version divergence — precisely what this
//! gate exists to catch. A blanket nonzero tolerance would mask config drift
//! forever. Justified divergences are enumerated one-by-one in
//! `deviations.toml`, each naming its cause.
//!
//! ## Failure modes this gate is built to prevent
//!
//! 1. **A vacuously-passing gate.** If the TS dumper had run without grammar
//!    init, every expected count would be 0 (`extractFromSource` returns an
//!    empty result + `parser_error` for an unloaded grammar — tree-sitter.ts:427-450).
//!    The dumper refuses to write such a baseline, and `baseline_is_not_vacuous`
//!    below re-asserts it from the Rust side: the committed baseline must be
//!    non-trivial and every fixture must have parsed.
//! 2. **Stale deviations.** A deviation entry that matches no observed
//!    difference FAILS the gate — otherwise a fixed bug would leave a permanent
//!    "known deviation" that silently permits a future regression.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use selene_extract::{ExtractionResult, detect_language, extract_from_source};
use serde::Deserialize;

// -----------------------------------------------------------------------------
// Baseline + deviation schemas
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Baseline {
    #[serde(rename = "codegraphCommit")]
    codegraph_commit: String,
    #[serde(rename = "fileCount")]
    file_count: usize,
    totals: Totals,
    files: BTreeMap<String, FileCounts>,
}

#[derive(Debug, Deserialize)]
struct Totals {
    nodes: usize,
    edges: usize,
    refs: usize,
}

#[derive(Debug, Deserialize)]
struct FileCounts {
    #[serde(rename = "nodesByKind")]
    nodes_by_kind: BTreeMap<String, usize>,
    #[serde(rename = "edgesByKind")]
    edges_by_kind: BTreeMap<String, usize>,
    #[serde(rename = "refsByKind")]
    refs_by_kind: BTreeMap<String, usize>,
    #[serde(rename = "nodeCount")]
    node_count: usize,
    #[serde(rename = "edgeCount")]
    edge_count: usize,
    #[serde(rename = "refCount")]
    ref_count: usize,
}

#[derive(Debug, Deserialize)]
struct Deviations {
    #[serde(default)]
    deviation: Vec<Deviation>,
}

/// One justified TS↔Rust divergence. `reason` is mandatory — a deviation
/// without a named cause is an unexamined bug.
#[derive(Debug, Deserialize)]
struct Deviation {
    fixture: String,
    counter: String,
    ts: usize,
    rust: usize,
    reason: String,
}

// -----------------------------------------------------------------------------
// Counter model — a flat `name -> count` map per fixture, so the gate can diff
// totals and per-kind buckets uniformly (and name a counter in one string).
// -----------------------------------------------------------------------------

/// `nodes`, `edges`, `refs`, `nodes.<kind>`, `edges.<kind>`, `refs.<kind>`.
type Counters = BTreeMap<String, usize>;

fn counters_from_baseline(f: &FileCounts) -> Counters {
    let mut c = Counters::new();
    c.insert("nodes".into(), f.node_count);
    c.insert("edges".into(), f.edge_count);
    c.insert("refs".into(), f.ref_count);
    for (k, v) in &f.nodes_by_kind {
        c.insert(format!("nodes.{k}"), *v);
    }
    for (k, v) in &f.edges_by_kind {
        c.insert(format!("edges.{k}"), *v);
    }
    for (k, v) in &f.refs_by_kind {
        c.insert(format!("refs.{k}"), *v);
    }
    c
}

fn counters_from_rust(r: &ExtractionResult) -> Counters {
    let mut c = Counters::new();
    c.insert("nodes".into(), r.nodes.len());
    c.insert("edges".into(), r.edges.len());
    c.insert("refs".into(), r.unresolved.len());
    for n in &r.nodes {
        *c.entry(format!("nodes.{}", n.kind.as_str())).or_insert(0) += 1;
    }
    for e in &r.edges {
        *c.entry(format!("edges.{}", e.kind.as_str())).or_insert(0) += 1;
    }
    for u in &r.unresolved {
        *c.entry(format!("refs.{}", u.reference_kind)).or_insert(0) += 1;
    }
    c
}

// -----------------------------------------------------------------------------
// Harness
// -----------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity")
}

fn load_baseline() -> Baseline {
    let p = fixtures_dir().join("expected.json");
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}\nRegenerate with tools/parity/dump-ts-extraction.mjs",
            p.display()
        )
    });
    serde_json::from_str(&raw).expect("parse expected.json")
}

fn load_deviations() -> Vec<Deviation> {
    let p = fixtures_dir().join("deviations.toml");
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let d: Deviations = toml::from_str(&raw).expect("parse deviations.toml");
    d.deviation
}

/// Re-extract one fixture with the Rust engine, keyed by the SAME relative path
/// the TS side used (the path feeds language detection and node ids).
fn extract_fixture(rel: &str) -> ExtractionResult {
    let abs = fixtures_dir().join(rel);
    let source = std::fs::read_to_string(&abs).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let lang = detect_language(rel, Some(&source));
    extract_from_source(rel, &source, lang)
}

/// One observed TS↔Rust difference.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Diff {
    fixture: String,
    counter: String,
    ts: usize,
    rust: usize,
}

/// Diff every fixture in the baseline against a freshly-extracted Rust result.
/// Pure over its inputs so the harness itself is testable (see
/// `harness_catches_a_synthetic_mismatch`).
fn diff_all(baseline: &Baseline, rust: &BTreeMap<String, Counters>) -> Vec<Diff> {
    let mut diffs = Vec::new();
    for (fixture, expected) in &baseline.files {
        let ts = counters_from_baseline(expected);
        let Some(rs) = rust.get(fixture) else {
            diffs.push(Diff {
                fixture: fixture.clone(),
                counter: "<missing-from-rust-run>".into(),
                ts: expected.node_count,
                rust: 0,
            });
            continue;
        };
        // Union of counter names — a kind present on one side only is a
        // difference against an implicit 0, not something to skip.
        let names: BTreeSet<&String> = ts.keys().chain(rs.keys()).collect();
        for name in names {
            let a = ts.get(name).copied().unwrap_or(0);
            let b = rs.get(name).copied().unwrap_or(0);
            if a != b {
                diffs.push(Diff {
                    fixture: fixture.clone(),
                    counter: name.clone(),
                    ts: a,
                    rust: b,
                });
            }
        }
    }
    diffs
}

// -----------------------------------------------------------------------------
// The gate
// -----------------------------------------------------------------------------

#[test]
fn ts_rust_extraction_count_parity() {
    let baseline = load_baseline();
    let deviations = load_deviations();

    let rust: BTreeMap<String, Counters> = baseline
        .files
        .keys()
        .map(|rel| (rel.clone(), counters_from_rust(&extract_fixture(rel))))
        .collect();

    let diffs = diff_all(&baseline, &rust);

    // Match each observed diff to a deviation entry (exact fixture+counter+ts+rust).
    let mut unexplained: Vec<&Diff> = Vec::new();
    let mut used: Vec<usize> = Vec::new();
    for d in &diffs {
        match deviations.iter().position(|x| {
            x.fixture == d.fixture && x.counter == d.counter && x.ts == d.ts && x.rust == d.rust
        }) {
            Some(i) => used.push(i),
            None => unexplained.push(d),
        }
    }

    // Stale deviations FAIL: a deviation matching no observed diff means the
    // underlying difference is gone (fixed, or the fixture changed). Leaving it
    // would permanently whitelist a counter that could regress unnoticed.
    let stale: Vec<&Deviation> = deviations
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(_, d)| d)
        .collect();

    let mut failure = String::new();
    if !unexplained.is_empty() {
        failure.push_str(&format!(
            "\n{} UNEXPLAINED count difference(s) vs the TS build (codegraph {}):\n",
            unexplained.len(),
            baseline.codegraph_commit
        ));
        for d in &unexplained {
            failure.push_str(&format!(
                "  {:<32} {:<24} ts={:<4} rust={:<4} (delta {:+})\n",
                d.fixture,
                d.counter,
                d.ts,
                d.rust,
                d.rust as i64 - d.ts as i64
            ));
        }
        failure.push_str(
            "\nEach is a real bug OR a justified deviation. Fix the bug, or add an entry to\n\
             tests/fixtures/parity/deviations.toml naming the cause (cite the TS source).\n",
        );
    }
    if !stale.is_empty() {
        failure.push_str(&format!("\n{} STALE deviation entr(ies) — they match no observed difference. The\ndivergence is gone; delete the entry so the counter is gated again:\n", stale.len()));
        for d in &stale {
            failure.push_str(&format!(
                "  {:<32} {:<24} ts={} rust={}  ({})\n",
                d.fixture, d.counter, d.ts, d.rust, d.reason
            ));
        }
    }
    assert!(failure.is_empty(), "{failure}");
}

/// The anti-vacuity assertion. If `expected.json` were ever regenerated without
/// grammar init, every count would be 0 and `ts_rust_extraction_count_parity`
/// would still pass — against a Rust side that also emitted nothing. Pin the
/// baseline as non-trivial so that failure mode is impossible.
#[test]
fn baseline_is_not_vacuous() {
    let baseline = load_baseline();

    assert_eq!(baseline.file_count, baseline.files.len(), "fileCount drift");
    assert!(
        baseline.file_count >= 25,
        "expected the full fixture corpus, got {} files",
        baseline.file_count
    );
    assert!(
        baseline.totals.nodes >= 100 && baseline.totals.edges >= 50 && baseline.totals.refs >= 50,
        "baseline looks vacuous ({:?}) — was it generated without initGrammars()?",
        baseline.totals
    );
    assert_ne!(
        baseline.codegraph_commit, "unknown",
        "baseline must record the codegraph commit it was generated from"
    );

    // Every single fixture must have produced real nodes on the TS side —
    // one silently-empty fixture is a hole in the gate's coverage.
    for (rel, f) in &baseline.files {
        assert!(f.node_count > 0, "{rel}: TS baseline has 0 nodes");
    }

    // ...and on the Rust side, so a fixture the Rust port cannot parse at all
    // fails loudly here rather than looking like a mere count mismatch.
    for rel in baseline.files.keys() {
        let r = extract_fixture(rel);
        assert!(
            !r.nodes.is_empty(),
            "{rel}: Rust extraction produced 0 nodes (errors: {:?})",
            r.errors
        );
    }
}

/// Every deviation must carry a non-empty justification. An unexplained entry is
/// an unexamined bug wearing a deviation's clothes.
#[test]
fn every_deviation_is_justified() {
    for d in load_deviations() {
        assert!(
            d.reason.trim().len() >= 20,
            "deviation {}/{} has no real justification: {:?}",
            d.fixture,
            d.counter,
            d.reason
        );
        assert_ne!(
            d.ts, d.rust,
            "deviation {}/{} records ts == rust — that is not a deviation",
            d.fixture, d.counter
        );
    }
}

/// The harness must actually catch a mismatch. Without this, a bug in `diff_all`
/// (say, iterating an empty map) would make the gate green forever — the exact
/// "gate that passes because it compares nothing" the task warns about.
#[test]
fn harness_catches_a_synthetic_mismatch() {
    let baseline = load_baseline();
    let (fixture, expected) = baseline.files.iter().next().expect("non-empty baseline");

    // Truth: identical counters ⇒ no diffs.
    let truthful: BTreeMap<String, Counters> = baseline
        .files
        .iter()
        .map(|(k, v)| (k.clone(), counters_from_baseline(v)))
        .collect();
    assert!(
        diff_all(&baseline, &truthful).is_empty(),
        "identical inputs must produce no diffs"
    );

    // Perturb ONE counter and confirm the harness reports exactly that one.
    let mut lying = truthful.clone();
    let c = lying.get_mut(fixture).unwrap();
    let bumped = expected.node_count + 7;
    c.insert("nodes".into(), bumped);

    let diffs = diff_all(&baseline, &lying);
    assert_eq!(diffs.len(), 1, "expected exactly one diff, got {diffs:?}");
    assert_eq!(diffs[0].fixture, *fixture);
    assert_eq!(diffs[0].counter, "nodes");
    assert_eq!(diffs[0].ts, expected.node_count);
    assert_eq!(diffs[0].rust, bumped);

    // A kind present on ONE side only must diff against an implicit 0 (not be skipped).
    let mut extra = truthful.clone();
    extra
        .get_mut(fixture)
        .unwrap()
        .insert("nodes.tombstone_kind_that_ts_never_emits".into(), 3);
    let diffs = diff_all(&baseline, &extra);
    assert_eq!(diffs.len(), 1, "one-sided kind must be caught: {diffs:?}");
    assert_eq!(diffs[0].ts, 0);
    assert_eq!(diffs[0].rust, 3);
}
