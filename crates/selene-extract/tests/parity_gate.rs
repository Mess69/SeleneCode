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
//! ## Two halves: counts AND names
//!
//! `ts_rust_extraction_count_parity` compares arity; `ts_rust_extraction_name_parity`
//! compares IDENTITY (`kind:name`, as a sorted multiset). The second is not
//! redundant — a count gate is structurally blind to a divergence that keeps the
//! count and changes the thing, and a port under count-pressure is precisely the
//! process that manufactures those. Both halves have their own deviation kind in
//! `deviations.toml` (`[[deviation]]` / `[[name-deviation]]`).
//!
//! ## Failure modes this gate is built to prevent
//!
//! 1. **A vacuously-passing gate.** If the TS dumper had run without grammar
//!    init, every expected count would be 0 (`extractFromSource` returns an
//!    empty result + `parser_error` for an unloaded grammar — tree-sitter.ts:427-450).
//!    The dumper refuses to write such a baseline, and `baseline_is_not_vacuous`
//!    below re-asserts it from the Rust side: the committed baseline must be
//!    non-trivial, every fixture must have parsed, and the name sets must be
//!    populated (else the name half would compare empty vectors and pass).
//! 2. **Stale deviations.** A deviation entry that matches no observed
//!    difference FAILS the gate — otherwise a fixed bug would leave a permanent
//!    "known deviation" that silently permits a future regression.
//! 3. **An UNGATED fixture.** The diff iterates the BASELINE, so a fixture added
//!    to the corpus but never dumped is compared by nobody while the gate still
//!    says green. `every_fixture_on_disk_is_gated` asserts set equality between
//!    the corpus on disk and the baseline's keys. (This was live: ten heritage
//!    fixtures sat ungated behind a green gate.)
//! 4. **Comparing two different extractors.** TS detects language from the PATH,
//!    Rust from path AND CONTENT — they can disagree (a C fixture re-detected as
//!    C++). `language_detection_agrees` asserts they don't.
//! 5. **A differ that doesn't diff.** `harness_catches_a_synthetic_mismatch` and
//!    `name_harness_catches_a_synthetic_mismatch` perturb known-good inputs and
//!    require the harness to report exactly the injected fault.
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
    /// The language TS detected FROM THE PATH. Compared against Rust's own
    /// detection — see `language_detection_agrees`.
    language: String,
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
    /// Every node as `kind:name`, sorted. Identity, not arity.
    #[serde(rename = "nodeNames", default)]
    node_names: Vec<String>,
    /// Every ref as `kind:name`, sorted. Identity, not arity.
    #[serde(rename = "refNames", default)]
    ref_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Deviations {
    #[serde(default)]
    deviation: Vec<Deviation>,
    #[serde(default, rename = "name-deviation")]
    name_deviation: Vec<NameDeviation>,
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

/// One justified TS↔Rust divergence in a NAME (identity), where the counts agree.
///
/// A count gate is structurally blind to this: `extends:SimplePositional(A)` and
/// `extends:SimplePositional` both count 1. `ts` and `rust` are the `kind:name`
/// strings each engine emits for the same construct; either may be absent
/// (`""`) if only one side emits it at all.
#[derive(Debug, Deserialize)]
struct NameDeviation {
    fixture: String,
    /// `"refs"` or `"nodes"`.
    set: String,
    ts: String,
    rust: String,
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
// Name model — identity, not arity.
// -----------------------------------------------------------------------------

/// The `kind:name` multiset of a fixture's nodes and refs, each sorted.
///
/// The counter model above cannot see a divergence that PRESERVES the count and
/// changes the identity. That is not a hypothetical: TS emits
/// `extends:SimplePositional(A)` — the raw `primary_constructor_base_type` text,
/// primary-ctor args included — where we emit `extends:SimplePositional`. Both
/// count 1 in `refs.extends`. Ours resolves; TS's never can.
///
/// A port being pushed to make counts match is precisely the process that
/// produces "right number, wrong thing", so the gate diffs names too. Justified
/// name divergences live in `deviations.toml` as `[[name-deviation]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NameSets {
    nodes: Vec<String>,
    refs: Vec<String>,
}

fn names_from_baseline(f: &FileCounts) -> NameSets {
    let mut n = NameSets {
        nodes: f.node_names.clone(),
        refs: f.ref_names.clone(),
    };
    n.nodes.sort();
    n.refs.sort();
    n
}

fn names_from_rust(r: &ExtractionResult) -> NameSets {
    let mut nodes: Vec<String> = r
        .nodes
        .iter()
        .map(|n| format!("{}:{}", n.kind.as_str(), n.name))
        .collect();
    let mut refs: Vec<String> = r
        .unresolved
        .iter()
        .map(|u| format!("{}:{}", u.reference_kind, u.reference_name))
        .collect();
    nodes.sort();
    refs.sort();
    NameSets { nodes, refs }
}

/// One `kind:name` present on exactly one side.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NameDiff {
    fixture: String,
    set: String,
    /// The name TS emitted and Rust did not (or `""`).
    ts: String,
    /// The name Rust emitted and TS did not (or `""`).
    rust: String,
}

/// Multiset difference in both directions, so a DUPLICATE emitted once too often
/// is caught as well as a rename.
fn diff_names_one(fixture: &str, set: &str, ts: &[String], rust: &[String]) -> Vec<NameDiff> {
    let mut only_ts = ts.to_vec();
    let mut only_rust = rust.to_vec();
    // Cancel matched pairs (multiset-aware: removes ONE occurrence per match).
    for name in ts {
        if let Some(i) = only_rust.iter().position(|r| r == name) {
            only_rust.remove(i);
            let j = only_ts.iter().position(|t| t == name).unwrap();
            only_ts.remove(j);
        }
    }
    let mut diffs = Vec::new();
    // Pair them up positionally so a plain RENAME reads as one ts↔rust line.
    let n = only_ts.len().max(only_rust.len());
    for i in 0..n {
        diffs.push(NameDiff {
            fixture: fixture.to_string(),
            set: set.to_string(),
            ts: only_ts.get(i).cloned().unwrap_or_default(),
            rust: only_rust.get(i).cloned().unwrap_or_default(),
        });
    }
    diffs
}

fn diff_names_all(baseline: &Baseline, rust: &BTreeMap<String, NameSets>) -> Vec<NameDiff> {
    let mut diffs = Vec::new();
    for (fixture, expected) in &baseline.files {
        let Some(rs) = rust.get(fixture) else {
            continue;
        };
        let ts = names_from_baseline(expected);
        diffs.extend(diff_names_one(fixture, "nodes", &ts.nodes, &rs.nodes));
        diffs.extend(diff_names_one(fixture, "refs", &ts.refs, &rs.refs));
    }
    diffs
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

fn load_deviations_file() -> Deviations {
    let p = fixtures_dir().join("deviations.toml");
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    toml::from_str(&raw).expect("parse deviations.toml")
}

fn load_deviations() -> Vec<Deviation> {
    load_deviations_file().deviation
}

/// Every fixture ON DISK, as paths relative to the corpus root — the same keys
/// the dumper writes. `.json`/`.toml` (the baseline and this ledger) are not
/// fixtures.
fn fixtures_on_disk() -> BTreeSet<String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read fixtures dir") {
            let p = entry.expect("dir entry").path();
            if p.is_dir() {
                walk(&p, root, out);
            } else if !matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("json") | Some("toml")
            ) {
                let rel = p.strip_prefix(root).expect("under root");
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let root = fixtures_dir();
    let mut out = BTreeSet::new();
    walk(&root, &root, &mut out);
    out
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

/// **Every fixture on disk MUST be in the baseline.**
///
/// `diff_all` iterates `baseline.files` — so a fixture added to the corpus but
/// never dumped into `expected.json` is compared by NOBODY, and the gate still
/// reports green. That is not hypothetical: this corpus carried 10 undumped
/// heritage fixtures in exactly that state, silently ungated, while the gate
/// said GREEN.
///
/// The set must match EXACTLY in both directions: an extra baseline key means a
/// fixture was deleted without regenerating, which would panic in
/// `extract_fixture` anyway — but fail here, with a message that says what to do.
#[test]
fn every_fixture_on_disk_is_gated() {
    let baseline = load_baseline();
    let on_disk = fixtures_on_disk();
    let in_baseline: BTreeSet<String> = baseline.files.keys().cloned().collect();

    let ungated: Vec<&String> = on_disk.difference(&in_baseline).collect();
    let orphaned: Vec<&String> = in_baseline.difference(&on_disk).collect();

    assert!(
        ungated.is_empty(),
        "\n{} fixture(s) on disk are NOT in the baseline — they are gated by NOTHING:\n{}\n\
         Regenerate: cd ../codegraph && npx vite-node <selene>/tools/parity/dump-ts-extraction.mjs \\\n\
         \x20   <selene>/crates/selene-extract/tests/fixtures/parity \\\n\
         \x20   <selene>/crates/selene-extract/tests/fixtures/parity/expected.json\n",
        ungated.len(),
        ungated
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        orphaned.is_empty(),
        "\n{} baseline entr(ies) have no fixture on disk (deleted without regenerating):\n{}",
        orphaned.len(),
        orphaned
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// **Both engines must be looking at the same language.**
///
/// TS detects from the PATH (`detectLanguage(rel)`); Rust detects from path AND
/// CONTENT (`detect_language(rel, Some(&source))`). They can disagree — the
/// dumper itself warns that a C fixture may be re-detected as C++. If they ever
/// did, the gate would be comparing the output of two DIFFERENT extractors and
/// calling the result parity. Assert they agree, per fixture.
#[test]
fn language_detection_agrees() {
    let baseline = load_baseline();
    let mut wrong = Vec::new();
    for (rel, f) in &baseline.files {
        let abs = fixtures_dir().join(rel);
        let source = std::fs::read_to_string(&abs).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let rust = detect_language(rel, Some(&source));
        let rust = format!("{rust:?}").to_lowercase();
        // TS spells them the same modulo case (`csharp`, `tsx`, `cpp`, …).
        if rust != f.language.to_lowercase() {
            wrong.push(format!("  {rel:<32} ts={:<12} rust={rust}", f.language));
        }
    }
    assert!(
        wrong.is_empty(),
        "\n{} fixture(s) where TS and Rust detect DIFFERENT languages — the gate would be\n\
         comparing two different extractors and calling it parity:\n{}\n",
        wrong.len(),
        wrong.join("\n")
    );
}

/// **Names, not just counts.** See `NameSets`.
///
/// The count gate is structurally blind to a divergence that keeps the count and
/// changes the identity. We already know of one (`csharp/Records.cs`:
/// `extends:SimplePositional(A)` vs `extends:SimplePositional`), and a port under
/// count-pressure is exactly the thing that manufactures more. Justified name
/// divergences are `[[name-deviation]]` entries in `deviations.toml`.
#[test]
fn ts_rust_extraction_name_parity() {
    let baseline = load_baseline();
    let devs = load_deviations_file().name_deviation;

    let rust: BTreeMap<String, NameSets> = baseline
        .files
        .keys()
        .map(|rel| (rel.clone(), names_from_rust(&extract_fixture(rel))))
        .collect();

    let diffs = diff_names_all(&baseline, &rust);

    let mut unexplained: Vec<&NameDiff> = Vec::new();
    let mut used: Vec<usize> = Vec::new();
    for d in &diffs {
        match devs.iter().position(|x| {
            x.fixture == d.fixture && x.set == d.set && x.ts == d.ts && x.rust == d.rust
        }) {
            Some(i) => used.push(i),
            None => unexplained.push(d),
        }
    }
    let stale: Vec<&NameDeviation> = devs
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(_, d)| d)
        .collect();

    let mut failure = String::new();
    if !unexplained.is_empty() {
        failure.push_str(&format!(
            "\n{} UNEXPLAINED name difference(s) vs the TS build (codegraph {}).\n\
             The COUNTS may well match — this is the identity gate:\n",
            unexplained.len(),
            baseline.codegraph_commit
        ));
        for d in &unexplained {
            failure.push_str(&format!(
                "  {:<28} {:<6} ts={:<34} rust={}\n",
                d.fixture,
                d.set,
                if d.ts.is_empty() { "—" } else { &d.ts },
                if d.rust.is_empty() { "—" } else { &d.rust },
            ));
        }
        failure.push_str(
            "\nEach is a real bug OR a justified divergence. Fix it, or add a\n\
             [[name-deviation]] to tests/fixtures/parity/deviations.toml (cite the TS source).\n",
        );
    }
    if !stale.is_empty() {
        failure.push_str(&format!(
            "\n{} STALE name-deviation entr(ies) — they match no observed difference:\n",
            stale.len()
        ));
        for d in &stale {
            failure.push_str(&format!(
                "  {:<28} {:<6} ts={:<34} rust={}\n",
                d.fixture, d.set, d.ts, d.rust
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
        // The NAME sets must be populated and consistent with the counts —
        // otherwise `ts_rust_extraction_name_parity` would compare empty vectors
        // and pass vacuously, exactly the trap this whole section exists to close.
        assert_eq!(
            f.node_names.len(),
            f.node_count,
            "{rel}: baseline nodeNames missing/short — regenerate with the current dumper"
        );
        assert_eq!(
            f.ref_names.len(),
            f.ref_count,
            "{rel}: baseline refNames missing/short — regenerate with the current dumper"
        );
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
    for d in load_deviations_file().name_deviation {
        assert!(
            d.reason.trim().len() >= 20,
            "name-deviation {}/{} has no real justification: {:?}",
            d.fixture,
            d.set,
            d.reason
        );
        assert_ne!(
            d.ts, d.rust,
            "name-deviation {}/{} records ts == rust — that is not a deviation",
            d.fixture, d.set
        );
        assert!(
            matches!(d.set.as_str(), "nodes" | "refs"),
            "name-deviation {}: `set` must be \"nodes\" or \"refs\", got {:?}",
            d.fixture,
            d.set
        );
    }
}

/// The NAME differ must actually differ — the same self-test the counter differ
/// gets. A `diff_names_one` that silently returned `vec![]` would make the
/// identity gate green forever, which is the failure mode it exists to prevent.
#[test]
fn name_harness_catches_a_synthetic_mismatch() {
    let ts = vec!["extends:Base".to_string(), "calls:f".to_string()];

    // Identical ⇒ no diffs.
    assert!(diff_names_one("x", "refs", &ts, &ts.clone()).is_empty());

    // A RENAME that preserves the count — the exact blind spot of the counter
    // gate — must be caught, and reported as one ts↔rust line.
    let renamed = vec!["extends:Base(A)".to_string(), "calls:f".to_string()];
    let d = diff_names_one("x", "refs", &ts, &renamed);
    assert_eq!(d.len(), 1, "count-preserving rename must be caught: {d:?}");
    assert_eq!(d[0].ts, "extends:Base");
    assert_eq!(d[0].rust, "extends:Base(A)");

    // An OVER-emission (Rust emits something TS never does) is caught with an
    // empty `ts` side — this is how the Python `decorates:route` bug surfaces.
    let extra = vec![
        "extends:Base".to_string(),
        "calls:f".to_string(),
        "decorates:route".to_string(),
    ];
    let d = diff_names_one("x", "refs", &ts, &extra);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].ts, "");
    assert_eq!(d[0].rust, "decorates:route");

    // MULTIPLICITY matters: the same name emitted twice where TS emits it once
    // is a duplicate, not a match. (A supertrait double-emit would look like this.)
    let dupe = vec![
        "extends:Base".to_string(),
        "extends:Base".to_string(),
        "calls:f".to_string(),
    ];
    let d = diff_names_one("x", "refs", &ts, &dupe);
    assert_eq!(d.len(), 1, "duplicate must be caught: {d:?}");
    assert_eq!(d[0].rust, "extends:Base");
    assert_eq!(d[0].ts, "");
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
