#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 21 — the synthesizer harness.
//!
//! All four whole-graph passes share this skeleton, and getting it wrong is how
//! the TS build earned three separate OOM/perf incidents. These tests pin the
//! parts that are easy to break silently: the dedupe key, the pass order, the
//! language gate, panic containment, and determinism.

use std::path::PathBuf;

use selene_core::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind, Provenance};
use selene_db::{GraphStore, SurrealStore};
use selene_resolve::synth::{
    SYNTH_PASS_ORDER, SynthPassDef, registered_synthesizers, run_synthesis_with, synth_passes,
};
use selene_resolve::{ResolutionContext, StoreContext};

// =============================================================================
// Fixtures
// =============================================================================

fn node(id: &str, name: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: "src/a.ts".to_string(),
        language: Language::Typescript.as_str().to_string(),
        start_line: 1,
        end_line: 2,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: vec![],
        type_parameters: vec![],
        return_type: None,
        route_method: None,
        route_path: None,
        framework: None,
        updated_at: 0,
    }
}

fn file_record(path: &str, lang: Language) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: "h".to_string(),
        language: lang.as_str().to_string(),
        size: 1,
        modified_at: 0,
        indexed_at: 0,
        node_count: 0,
        errors: vec![],
    }
}

/// A store + context whose only file is of `lang` — so the language gate has
/// something to gate on.
async fn ctx_of(lang: Language, nodes: &[Node]) -> StoreContext<SurrealStore> {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store
        .upsert_file(&file_record("src/a.ts", lang))
        .await
        .unwrap();
    if !nodes.is_empty() {
        store.insert_nodes(nodes).await.unwrap();
    }
    StoreContext::new(store, PathBuf::from("/tmp/synth-harness"))
        .await
        .unwrap()
}

fn edge(source: &str, target: &str, kind: EdgeKind, by: &str) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: Some(serde_json::json!({ "synthesizedBy": by })),
        line: None,
        column: None,
        provenance: None, // the orchestrator stamps Heuristic
    }
}

/// A pass that emits a fixed edge list.
fn stub<S: GraphStore>(
    name: &'static str,
    languages: &'static [Language],
    run: selene_resolve::synth::SynthRunFn<S>,
) -> SynthPassDef<S> {
    SynthPassDef {
        name,
        languages,
        run,
    }
}

const TS: &[Language] = &[Language::Typescript];
const PY: &[Language] = &[Language::Python];

// =============================================================================
// (a) the dedupe key, and FIRST PASS WINS
// =============================================================================

/// Two passes emit the same `(source, target)` with **different kinds**. Exactly
/// one edge survives — the FIRST pass's.
///
/// The dedupe key is `(source, target)`, deliberately NOT `(source, target,
/// kind)`: a second pass must not double-link a pair an earlier pass already
/// bridged, whatever kind it would have used. That is what makes
/// `SYNTH_PASS_ORDER` behavior rather than decoration.
#[tokio::test(flavor = "multi_thread")]
async fn dedupe_is_keyed_on_source_target_and_the_first_pass_wins() {
    let ctx = ctx_of(
        Language::Typescript,
        &[node("function:a", "a"), node("function:b", "b")],
    )
    .await;

    fn first<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        Box::pin(async {
            Ok(vec![edge(
                "function:a",
                "function:b",
                EdgeKind::Calls,
                "first",
            )])
        })
    }
    fn second<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        Box::pin(async {
            Ok(vec![edge(
                "function:a",
                "function:b",
                EdgeKind::References, // different kind, SAME pair
                "second",
            )])
        })
    }

    let passes = vec![stub("first", TS, first), stub("second", TS, second)];
    let inserted = run_synthesis_with(ctx.store(), &ctx, &passes)
        .await
        .unwrap();
    assert_eq!(inserted, 1, "the pair is bridged ONCE, not twice");

    let out = ctx
        .store()
        .outgoing("function:a", &[EdgeKind::Calls, EdgeKind::References], None)
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let e = &out[0].edge;
    assert_eq!(
        e.metadata.as_ref().unwrap()["synthesizedBy"],
        "first",
        "FIRST pass wins the pair — order is behavior"
    );
    assert_eq!(
        e.provenance,
        Some(Provenance::Heuristic),
        "every synthesized edge is heuristic, stamped by the orchestrator"
    );
}

// =============================================================================
// (b) the language gate — checked BEFORE the pass runs
// =============================================================================

/// A pass whose languages do not intersect the repo's is **never invoked**. A
/// Python-only repo must not spend a second scanning for JSX (#1212).
#[tokio::test(flavor = "multi_thread")]
async fn a_pass_gated_out_by_language_is_never_invoked() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    fn counting<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(vec![]) })
    }

    // The repo is Python; the pass is TypeScript-only.
    let ctx = ctx_of(Language::Python, &[]).await;
    let passes = vec![stub("ts-only", TS, counting)];
    run_synthesis_with(ctx.store(), &ctx, &passes)
        .await
        .unwrap();
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        0,
        "the gate must short-circuit BEFORE the pass runs, not filter its output"
    );

    // …and a matching language does invoke it.
    let ctx = ctx_of(Language::Python, &[]).await;
    let passes = vec![stub("py-only", PY, counting)];
    run_synthesis_with(ctx.store(), &ctx, &passes)
        .await
        .unwrap();
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}

// =============================================================================
// (c) panic containment
// =============================================================================

/// A pass that panics contributes 0 edges, and `run_synthesis` still returns
/// `Ok` — the blast radius of one bad regex is one channel, never the index.
/// The *other* passes still run.
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_pass_yields_zero_edges_and_never_fails_the_index() {
    let ctx = ctx_of(
        Language::Typescript,
        &[node("function:a", "a"), node("function:b", "b")],
    )
    .await;

    fn boom<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        Box::pin(async { panic!("bad regex") })
    }
    fn good<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        Box::pin(async {
            Ok(vec![edge(
                "function:a",
                "function:b",
                EdgeKind::Calls,
                "good",
            )])
        })
    }

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let passes = vec![stub("boom", TS, boom), stub("good", TS, good)];
    let inserted = run_synthesis_with(ctx.store(), &ctx, &passes)
        .await
        .expect("a panicking pass must NOT fail the index");
    std::panic::set_hook(prev);

    assert_eq!(
        inserted, 1,
        "the panicking pass contributed 0; the healthy one still ran"
    );
}

// =============================================================================
// (d) chunked insert
// =============================================================================

/// 4500 edges insert across 3 chunks of 2000. (What is asserted is that they ALL
/// land — a chunking bug that drops the tail is silent otherwise.)
#[tokio::test(flavor = "multi_thread")]
async fn edges_insert_in_chunks_and_none_are_dropped() {
    const N: usize = 4500;

    let mut nodes = vec![node("function:src", "src")];
    for i in 0..N {
        nodes.push(node(&format!("function:t{i:04}"), &format!("t{i:04}")));
    }
    let ctx = ctx_of(Language::Typescript, &nodes).await;

    fn many<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        Box::pin(async {
            Ok((0..4500)
                .map(|i| {
                    edge(
                        "function:src",
                        &format!("function:t{i:04}"),
                        EdgeKind::Calls,
                        "many",
                    )
                })
                .collect())
        })
    }

    let passes = vec![stub("many", TS, many)];
    let inserted = run_synthesis_with(ctx.store(), &ctx, &passes)
        .await
        .unwrap();
    assert_eq!(
        inserted as usize, N,
        "every edge lands — a chunk boundary must not drop the tail"
    );
}

// =============================================================================
// (e) determinism
// =============================================================================

/// The same input twice ⇒ byte-identical edge sets, in the same order. This is
/// the test that catches a `HashMap` sneaking into the merge.
#[tokio::test(flavor = "multi_thread")]
async fn synthesis_is_deterministic_across_runs() {
    fn spray<S: GraphStore>(
        _s: &S,
        _c: &dyn ResolutionContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = selene_resolve::Result<Vec<Edge>>> + Send>> {
        Box::pin(async {
            Ok((0..50)
                .map(|i| {
                    edge(
                        &format!("function:s{i:02}"),
                        &format!("function:t{i:02}"),
                        EdgeKind::Calls,
                        "spray",
                    )
                })
                .collect())
        })
    }

    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut nodes = Vec::new();
        for i in 0..50 {
            nodes.push(node(&format!("function:s{i:02}"), &format!("s{i:02}")));
            nodes.push(node(&format!("function:t{i:02}"), &format!("t{i:02}")));
        }
        let ctx = ctx_of(Language::Typescript, &nodes).await;
        let passes = vec![stub("spray", TS, spray)];
        run_synthesis_with(ctx.store(), &ctx, &passes)
            .await
            .unwrap();

        let mut all: Vec<String> = Vec::new();
        for i in 0..50 {
            for n in ctx
                .store()
                .outgoing(&format!("function:s{i:02}"), &[EdgeKind::Calls], None)
                .await
                .unwrap()
            {
                all.push(format!("{}->{}", i, n.node.id));
            }
        }
        runs.push(all);
    }
    assert_eq!(runs[0], runs[1], "same input ⇒ same edges, same order");
}

// =============================================================================
// The single-source-of-truth pair
// =============================================================================

/// `synth_passes()` and `SYNTH_PASS_ORDER` are the SAME list, in the SAME order.
///
/// This one assertion is what makes the order a single source of truth. It fails
/// the moment someone adds a pass to the table without declaring it (or declares
/// one that is not wired). **Part C's completeness gate — "no synthesizer ships
/// ungated" — is keyed to `registered_synthesizers()`, so without this pair a
/// fifth dispatch channel could ship gated by nobody.**
#[test]
fn registry_agrees_with_the_declared_order() {
    let table: Vec<&str> = synth_passes::<SurrealStore>()
        .iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(
        table, SYNTH_PASS_ORDER,
        "the pass TABLE and the declared ORDER have drifted — one of them is lying"
    );
    assert_eq!(
        registered_synthesizers(),
        SYNTH_PASS_ORDER,
        "registered_synthesizers() must DERIVE from the order, never re-list it"
    );

    // Names are unique — two passes sharing a name would make `synthesizedBy`
    // ambiguous and the coverage gate unaddressable.
    let unique: std::collections::BTreeSet<&&str> = SYNTH_PASS_ORDER.iter().collect();
    assert_eq!(unique.len(), SYNTH_PASS_ORDER.len(), "duplicate pass name");
}
