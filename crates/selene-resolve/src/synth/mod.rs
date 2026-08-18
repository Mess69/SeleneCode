//! The dynamic-dispatch synthesizers — the whole-graph passes that bridge the
//! calls tree-sitter structurally cannot see.
//!
//! # What a synthesizer is, and when it is the wrong tool
//!
//! Static extraction captures explicit calls (`foo()`, `this.bar()`). It misses
//! any call whose target is computed. Two mechanisms fix that, and picking the
//! wrong one is the classic mistake:
//!
//! | The reference is… | Mechanism | Provenance |
//! |---|---|---|
//! | **named** (`self._iterable_class(self)`) | a framework `resolve()` + `claims_reference` | ordinary `tree-sitter` edge |
//! | **anonymous** (`cb()`, `emit('e')`, `<Child/>`) | a whole-graph pass **here** | `heuristic` + `synthesizedBy` |
//!
//! A synthesizer exists precisely because there is **no ref to resolve** and the
//! correlation is cross-file: the registrar, the registration site and the
//! dispatcher are three different places.
//!
//! # The invariant that governs every pass
//!
//! **Dynamic-dispatch coverage is end-to-end or not at all** (PRD §8.2). A
//! half-bridged flow is *worse* than none: it advertises a hop the agent then has
//! to Read to finish. The `react-render` pass is the worked example — shipped
//! without `jsx-render` it measurably RAISED agent reads, which is why the two
//! are one mergeable unit.
//!
//! # Why the registry is a table of fn pointers (decision D3)
//!
//! The obvious shape — `trait SynthPass { async fn run<S: GraphStore>(…) }` and a
//! `&'static [&dyn SynthPass]` — **cannot compile**. A generic method is not
//! object-safe (it has no single vtable entry), and neither is RPITIT. Keeping
//! the resolver generic over `S: GraphStore` is a Global Constraint, so the
//! registry is instead a table of **monomorphized fn pointers**, built per store
//! type at the call site: [`SynthPassDef<S>`] + [`synth_passes`].
//!
//! # Perf discipline (each of these is a TS incident)
//!
//! - **Stream, never materialize.** A pass scans every method in the repo;
//!   `get_nodes_by_kind` on an unbounded kind OOM'd (#610). Use
//!   [`stream_nodes_by_kind`].
//! - **Language-gate every pass** before it runs (#1212) — a Python-only repo
//!   must never scan for JSX.
//! - **Pre-gate expensive regexes with cheap `contains()`** (#1235 — an ungated
//!   scan cost 20+ minutes on real corpora).
//! - **Index lines once per file** ([`LineIndex`]), never `slice().split()` per
//!   match.
//! - **Chunk the inserts** ([`INSERT_CHUNK`]).
//!
//! # The sync/async seam — every pass must respect it
//!
//! A pass is `async` (it reads the store), but [`ResolutionContext`] is a **sync**
//! seam that drives the async store through `block_on` internally. Calling a
//! context method straight from a pass's async body therefore **panics** —
//! *"cannot start a runtime from within a runtime"* — because the thread is
//! currently driving tasks.
//!
//! So every ctx-touching section of every pass is wrapped in
//! `tokio::task::block_in_place(|| …)`, which hands the worker's other tasks off
//! and makes the context's internal `block_on` legal. The shape is:
//!
//! ```text
//! async  : read the store (stream nodes, fetch edges)
//! sync   : block_in_place — classify / correlate / resolve names, over ctx
//! async  : read the store again if the sync phase asked a new question
//! sync   : block_in_place — build the edges
//! ```
//!
//! This is why the passes read as "gather, then decide" rather than interleaving:
//! the seam forces the phases apart, and that is a feature — it is also what keeps
//! the store round-trips batched instead of one-per-candidate.

pub mod callback;
pub mod event_emitter;
pub mod lineindex;
pub mod react;

use std::collections::BTreeSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

use futures::FutureExt;
use selene_core::{Edge, Language, Node, NodeKind};
use selene_db::GraphStore;

use crate::Result;
use crate::context::ResolutionContext;

pub use lineindex::LineIndex;

/// Rows per `insert_edges` round trip.
pub const INSERT_CHUNK: usize = 2000;
/// Rows per `nodes_by_kind_page` round trip while streaming.
pub const STREAM_PAGE: usize = 500;

/// A pass's `run`, monomorphized for the caller's store type.
///
/// The boxed future is what makes it storable in a table at all — an `async fn`
/// has an anonymous return type and cannot be a bare fn-pointer target.
pub type SynthRunFn<S> = for<'a> fn(
    &'a S,
    &'a dyn ResolutionContext,
)
    -> Pin<Box<dyn Future<Output = Result<Vec<Edge>>> + Send + 'a>>;

/// One synthesizer pass.
pub struct SynthPassDef<S: GraphStore> {
    /// The pass name — and **the `metadata.synthesizedBy` value** its edges
    /// carry. One string, two jobs, deliberately: the edge says which pass made
    /// it, and the name is how the coverage gate addresses that channel.
    pub name: &'static str,
    /// Languages this pass applies to. Empty = all. Checked against
    /// `ctx.languages()` **before** the pass runs.
    pub languages: &'static [Language],
    /// Collects edges. **Never inserts** — the orchestrator dedupes across passes
    /// first, so a pass that wrote its own edges would defeat the dedupe.
    pub run: SynthRunFn<S>,
}

/// **THE ONE declared pass order.** Everything else derives from this list.
///
/// Order is behavior: the cross-pass dedupe is keyed on `(source, target)` and
/// **first pass wins**, so re-ordering silently changes which pass's metadata a
/// bridged pair ends up carrying. Each of Tasks 22–25 adds exactly one name here
/// and one row to [`synth_passes`]; `registry_agrees_with_the_declared_order`
/// (tests/synth_harness_test.rs) fails the moment the two drift.
///
/// The v0 target order is `callback`, `event-emitter`, `react-render`,
/// `jsx-render`. Each of Tasks 22–25 appends **its own** name as it lands, so the
/// list is empty until then — it is never pre-populated, because a name here with
/// no row in [`synth_passes`] would make the coverage gate demand a flow for a
/// channel that does not exist.
///
/// **Phase 8 slot:** Go's `contains` + `implements` pre-passes must be inserted
/// **first**, ahead of everything here — the interface-dispatch passes read the
/// edges they create.
pub const SYNTH_PASS_ORDER: &[&str] = &["callback", "event-emitter", "react-render", "jsx-render"];

/// The pass table, monomorphized for `S`. Must match [`SYNTH_PASS_ORDER`] exactly
/// — `registry_agrees_with_the_declared_order` fails the moment they drift.
pub fn synth_passes<S: GraphStore>() -> Vec<SynthPassDef<S>> {
    vec![
        SynthPassDef {
            name: "callback",
            languages: JS_FAMILY,
            run: |s, c| Box::pin(callback::run(s, c)),
        },
        SynthPassDef {
            name: "event-emitter",
            languages: JS_FAMILY,
            run: |s, c| Box::pin(event_emitter::run(s, c)),
        },
        // The React pair — registered TOGETHER, in this order. `react-render`
        // alone is the half-bridged flow that measurably RAISED agent reads
        // (PRD §8.2): it ends at `render`, which advertises a next hop and gives
        // the agent nowhere to go. Never register one without the other.
        SynthPassDef {
            name: "react-render",
            languages: JS_FAMILY,
            run: |s, c| Box::pin(react::run_react_render(s, c)),
        },
        SynthPassDef {
            name: "jsx-render",
            languages: JS_FAMILY,
            run: |s, c| Box::pin(react::run_jsx_render(s, c)),
        },
    ]
}

/// The language gate every v0 pass shares. The observer/emitter/JSX shapes are
/// all `this.`- and JSX-based; a broader gate (Java/C# `this.` works too) was
/// never validated and is a Phase 8 question.
pub(crate) const JS_FAMILY: &[Language] = &[
    Language::Typescript,
    Language::Tsx,
    Language::Javascript,
    Language::Jsx,
];

/// Every `synthesizedBy` value the registry can emit.
///
/// **Derived from [`SYNTH_PASS_ORDER`] — never a second hard-coded list.** Part
/// C's completeness gate ("no synthesizer ships ungated") is keyed to this, and a
/// hard-coded copy would drift on the first Phase 8 channel, at which point the
/// gate would silently stop defending the new one.
pub fn registered_synthesizers() -> &'static [&'static str] {
    SYNTH_PASS_ORDER
}

/// Page through every node of `kind`, in id order. O(1) memory (#610).
pub async fn stream_nodes_by_kind<S: GraphStore>(store: &S, kind: NodeKind) -> Result<Vec<Node>> {
    // Returns a Vec, but *pages* to build it: the caller's working set is one
    // page at a time inside the store, and callers that need to bound memory
    // further can page directly. (The passes filter as they go, so what survives
    // is small — it is the unbounded FETCH that OOM'd, not the survivors.)
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .nodes_by_kind_page(kind, after.as_deref(), STREAM_PAGE)
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().map(|n| n.id.clone());
        out.extend(page);
    }
    Ok(out)
}

/// Run every registered pass, merge, dedupe, insert. Returns the count inserted.
///
/// Runs on the **full-index path only** — see the deferral in `lib.rs`.
pub async fn run_synthesis<S: GraphStore>(store: &S, ctx: &dyn ResolutionContext) -> Result<u64> {
    run_synthesis_with(store, ctx, &synth_passes::<S>()).await
}

/// [`run_synthesis`] over an explicit pass table — the seam the harness tests
/// inject stub passes through.
pub async fn run_synthesis_with<S: GraphStore>(
    store: &S,
    ctx: &dyn ResolutionContext,
    passes: &[SynthPassDef<S>],
) -> Result<u64> {
    let langs = ctx.languages();
    let mut merged: Vec<Edge> = Vec::new();
    // Dedupe key is `(source, target)` — NOT `(source, target, kind)`. A second
    // pass must not double-link a pair an earlier pass already bridged, whatever
    // kind it would have used. First pass wins, which is why the order is fixed.
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();

    // Run the language-gated passes CONCURRENTLY — each is a read-only correlation over the store
    // (the ctx is `Send + Sync`, its caches thread-safe), so their store reads overlap instead of
    // running one after another. `join_all` preserves order, so the merge below is still in pass
    // order and "first pass wins" is unchanged. A panicking pass still degrades to zero edges.
    let gated: Vec<&SynthPassDef<S>> = passes
        .iter()
        .filter(|p| p.languages.is_empty() || p.languages.iter().any(|l| langs.contains(l)))
        .collect();
    let results = futures::future::join_all(
        gated
            .iter()
            .map(|pass| AssertUnwindSafe((pass.run)(store, ctx)).catch_unwind()),
    )
    .await;

    for res in results {
        let edges = match res {
            Ok(Ok(edges)) => edges,
            Ok(Err(_)) | Err(_) => continue,
        };
        for mut e in edges {
            if !seen.insert((e.source.clone(), e.target.clone())) {
                continue;
            }
            e.provenance = Some(selene_core::Provenance::Heuristic);
            merged.push(e);
        }
    }

    let mut inserted = 0u64;
    for chunk in merged.chunks(INSERT_CHUNK) {
        inserted += store.insert_edges(chunk).await?;
    }
    Ok(inserted)
}

// =============================================================================
// Shared helpers for the passes
// =============================================================================

/// The body text of a node — the source lines it spans.
///
/// `None` when the file is unreadable or the range is nonsense; a pass then
/// simply skips that node (errors collected, never thrown).
pub(crate) fn node_body(ctx: &dyn ResolutionContext, node: &Node) -> Option<String> {
    let lines = ctx.file_lines(&node.file_path)?;
    let start = node.start_line.saturating_sub(1) as usize;
    let end = (node.end_line as usize).min(lines.len());
    if start >= end {
        return None;
    }
    Some(lines[start..end].join("\n"))
}

/// The tightest `method`/`function`/`component` node containing `line` in `file`.
pub(crate) fn enclosing_fn(nodes: &[Node], file: &str, line: u32) -> Option<Node> {
    nodes
        .iter()
        .filter(|n| {
            n.file_path == file
                && n.start_line <= line
                && n.end_line >= line
                && matches!(
                    n.kind,
                    NodeKind::Method | NodeKind::Function | NodeKind::Component
                )
        })
        // Tightest = the one whose span is smallest.
        .min_by_key(|n| n.end_line.saturating_sub(n.start_line))
        .cloned()
}

/// Declared strength of each synthesis pass (graph-platform PRD F7): how much
/// evidence stands behind an edge this pass invents. Initial values, declared
/// not measured — calibrate against real corpora before trusting them finely.
/// Exact-registration bridges (a JSX element naming its component) sit high;
/// name-pattern bridges (an event name matching a handler) sit lower.
pub(crate) fn confidence_for(pass: &str) -> f64 {
    match pass {
        "jsx-render" | "react-render" => 0.8,
        "callback" => 0.7,
        "event-emitter" => 0.6,
        _ => 0.7,
    }
}

/// Build a synthesized edge. Always `Heuristic`; `synthesizedBy` is the pass name.
pub(crate) fn synth_edge(
    source: &str,
    target: &str,
    pass: &str,
    line: Option<u32>,
    extra: &[(&str, String)],
) -> Edge {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "synthesizedBy".to_string(),
        serde_json::Value::String(pass.to_string()),
    );
    // F7: every invented edge carries its declared strength — the agent can
    // weigh a dynamic hop instead of taking it on faith.
    if let Some(n) = serde_json::Number::from_f64(confidence_for(pass)) {
        meta.insert("confidence".to_string(), serde_json::Value::Number(n));
    }
    for (k, v) in extra {
        meta.insert((*k).to_string(), serde_json::Value::String(v.clone()));
    }
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind: selene_core::EdgeKind::Calls,
        metadata: Some(serde_json::Value::Object(meta)),
        line,
        column: None,
        provenance: Some(selene_core::Provenance::Heuristic),
    }
}
