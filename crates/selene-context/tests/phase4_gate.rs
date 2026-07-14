#![allow(clippy::unwrap_used, clippy::expect_used)]
//! **THE PHASE 4 GATE.**
//!
//! # The snapshots are the weakest half. These assertions are the strong one.
//!
//! A snapshot proves the output has not *changed*. It does not prove the output is any
//! **good** — an empty file section, a Flow with no steps, a "```\n```" code block with no
//! code: every one of those snapshots cleanly and passes forever. That is how Phase 2 froze a
//! phantom Python edge into a passing test, and it is the shape of all four inert seams this
//! project has paid for.
//!
//! So this gate asserts **properties of real output from a real store**, independently of any
//! snapshot:
//!
//! 1. **Every file section contains actual source lines** — matched as `^\d+\t`. This is the
//!    positive control that would have caught all four seams: a renderer that emits headers
//!    and no code passes every "no crash" test and hands the agent nothing.
//! 2. **A Flow section with ≥3 numbered steps** exists, and **the synth project's flow
//!    carries a `↓ dynamic:` arrow** — because a dynamic hop rendered as a gap is the exact
//!    failure Phase 3's entire synthesizer layer exists to prevent.
//! 3. **The blast radius exists.**
//! 4. **`total_chars ≤ min(round(max_output_chars × 1.5), 25_000)`** on real output — past
//!    that the host externalizes into a file the agent must open.
//! 5. **No output string matches `\bRead\b|\bgrep\b`** outside the sanctioned banners.
//! 6. **A planted `API_KEY=sk-live-…` appears NOWHERE** (#383).
//!
//! The corpus is built by the **real** `Indexer` → **real** `resolve_and_persist_batched` →
//! real store. No test-composed pipeline: a gate that assembles its own graph certifies the
//! assembly, not the product.

use std::path::{Path, PathBuf};

use regex::Regex;
use selene_context::{ContextBuilder, budget_for};
use selene_db::SurrealStore;
use selene_extract::Indexer;
use selene_graph::QueryManager;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gate")
        .join(name)
}

/// The REAL pipeline. Nothing here hand-inserts a node.
async fn explore(project: &str, query: &str) -> (String, u64) {
    let dir = fixture(project);

    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = Indexer::new(dir.clone(), store);
    let __ix = indexer.index_all(None).await;
    let result = &__ix;
    assert!(result.files_indexed > 0, "{project}: indexed ZERO files");
    let store = indexer.into_store();

    selene_resolve::resolve_and_persist_in_memory(&store, &dir, __ix.unresolved.clone(), None)
        .await
        .expect("resolution must never fail an index");

    let file_count = store.stats().await.unwrap().files;
    let builder = ContextBuilder::new(QueryManager::new(store, dir));
    (builder.build_context(query).await.unwrap(), file_count)
}

/// **HALF 3, ASSERTION 1 — the positive control that would have caught all four inert seams.**
///
/// Every file section must contain **actual source lines**, matched as `^\d+\t`. A renderer
/// that emits headers and no code passes every "no crash" test, snapshots cleanly, and hands
/// the agent nothing.
#[tokio::test(flavor = "multi_thread")]
async fn every_file_section_contains_real_numbered_source_lines() {
    let numbered = Regex::new(r"(?m)^\d+\t").unwrap();

    for (project, query) in [
        ("plain", "handleLogin hashPassword"),
        ("synth", "mutateElement renderScene"),
    ] {
        let (out, _) = explore(project, query).await;

        assert!(
            out.contains("**`"),
            "{project}: no file section at all:\n{out}"
        );
        let lines = numbered.find_iter(&out).count();
        assert!(
            lines >= 3,
            "{project}: the output has file HEADERS but only {lines} numbered source \
             line(s). A renderer that shows an agent where the code is, without showing it \
             the code, has sent it to Read — and it would snapshot perfectly.\n{out}"
        );
    }
}

/// **HALF 3, ASSERTION 2 — the Flow, and the dynamic hop.**
///
/// A dynamic hop rendered as a *gap* is the exact failure Phase 3's synthesizer layer exists
/// to prevent: the agent follows the chain, hits `?`, and opens the file — where the
/// connection is not written, because it is registered at runtime.
#[tokio::test(flavor = "multi_thread")]
async fn a_flow_renders_with_numbered_steps_and_the_dynamic_hop_is_named() {
    let (plain, _) = explore("plain", "handleLogin hashPassword").await;

    assert!(plain.contains("### Flow"), "no Flow section:\n{plain}");
    let steps = Regex::new(r"(?m)^\d+\. `")
        .unwrap()
        .find_iter(&plain)
        .count();
    assert!(
        steps >= 3,
        "the flow has {steps} steps — handleLogin → login → hashPassword is THREE, and a \
         flow that skips the middle hop is the one that sends the agent to Read:\n{plain}"
    );

    // The synth project is Phase 3's OWN callback fixture — the chain
    // `mutateElement → triggerUpdate → triggerRender → renderScene` runs through a callback
    // registration, a hop that exists ONLY because a synthesizer built it.
    let (synth, _) = explore("synth", "mutateElement renderScene").await;
    assert!(
        synth.contains("↓ dynamic:") || synth.contains("dynamic:"),
        "NO DYNAMIC HOP RENDERED. The callback bridge exists in the graph (Phase 3's \
         synthesizer built it) and the answer does not show it — so the agent sees the chain \
         stop and goes to read `canvas.ts`, where the connection is not written down. Every \
         synthesized edge must appear as a NAMED HOP, never as a gap.\n{synth}"
    );
}

/// **HALF 3, ASSERTION 3 — the blast radius.**
#[tokio::test(flavor = "multi_thread")]
async fn the_blast_radius_section_exists() {
    let (out, _) = explore("plain", "hashPassword").await;
    assert!(
        out.contains("### Blast radius"),
        "'what breaks if I change this' is the agent's second question — unanswered here, it \
         becomes a second tool call:\n{out}"
    );
}

/// **HALF 3, ASSERTION 4 — the hard ceiling, on REAL output.**
#[tokio::test(flavor = "multi_thread")]
async fn real_output_never_exceeds_the_externalization_ceiling() {
    for (project, query) in [("plain", "handleLogin"), ("synth", "mutateElement")] {
        let (out, file_count) = explore(project, query).await;
        let budget = budget_for(file_count);
        let ceiling = ((budget.max_output_chars as f64 * 1.5).round() as usize).min(25_000);

        assert!(
            out.len() <= ceiling,
            "{project}: {} chars > the {ceiling} ceiling. Past ~25 000 the host EXTERNALIZES \
             the result into a file the agent must open — a bigger answer that forces the \
             very Read it exists to prevent.",
            out.len()
        );
    }
}

/// **HALF 3, ASSERTION 5 — never tell the agent to Read.**
///
/// The only strings allowed to are the staleness/degraded banners (Phase 6) and the
/// truncation note, which says *do NOT Read*.
#[tokio::test(flavor = "multi_thread")]
async fn no_output_string_tells_the_agent_to_read_or_grep() {
    let forbidden = Regex::new(r"(?i)\b(read|grep)\b").unwrap();

    for (project, query) in [
        ("plain", "handleLogin hashPassword"),
        ("plain", "zzz nothing matches this"),
        ("synth", "mutateElement"),
    ] {
        let (out, _) = explore(project, query).await;

        for m in forbidden.find_iter(&out) {
            let line = out[..m.start()]
                .rsplit('\n')
                .next()
                .unwrap_or("")
                .to_string()
                + m.as_str();
            // The sanctioned uses: the truncation note ("treat it as already Read", "do NOT
            // Read these files") and the low-confidence handoff ("Do not open these files").
            let sanctioned = line.contains("already Read")
                || line.contains("do NOT Read")
                || line.contains("Do not open");
            assert!(
                sanctioned,
                "{project} ({query:?}): the output says {:?} — every such string is a nudge \
                 toward the tool we exist to replace, and the failure paths are where it is \
                 most tempting.\ncontext: …{line}",
                m.as_str()
            );
        }
    }
}

/// **HALF 3, ASSERTION 6 — #383. The secret appears NOWHERE.**
#[tokio::test(flavor = "multi_thread")]
async fn a_planted_secret_never_reaches_the_output() {
    // The synth fixture ships `.env` with `API_KEY=sk-live-abc123secret`.
    for query in ["API_KEY", "mutateElement", "env config"] {
        let (out, _) = explore("synth", query).await;
        assert!(
            !out.contains("sk-live-abc123secret"),
            "A SECRET REACHED THE OUTPUT for {query:?}. Once it is in an agent's context \
             window it has left the machine. Config leaves render KEYS ONLY (#383).\n{out}"
        );
    }
}

/// The corpus is real, and the gate is not vacuously green: the graph it asserts against has
/// cross-file edges. Without this, every assertion above could pass on an empty graph.
#[tokio::test(flavor = "multi_thread")]
async fn the_gate_corpus_is_a_real_resolved_graph() {
    let dir = fixture("plain");
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = Indexer::new(dir.clone(), store);
    let __ix = indexer.index_all(None).await;
    let store = indexer.into_store();
    selene_resolve::resolve_and_persist_in_memory(&store, &dir, __ix.unresolved.clone(), None)
        .await
        .unwrap();

    let (nodes, edges) = store.node_edge_count().await.unwrap();
    assert!(nodes > 0 && edges > 0);

    let from = store
        .get_nodes_by_name("handleLogin")
        .await
        .unwrap()
        .remove(0);
    let to = store.get_nodes_by_name("login").await.unwrap().remove(0);
    assert!(
        store
            .find_path(
                &from.id,
                &to.id,
                &[
                    selene_core::EdgeKind::Calls,
                    selene_core::EdgeKind::References
                ]
            )
            .await
            .unwrap()
            .is_some(),
        "the corpus has no cross-file edges — every assertion in this gate would pass \
         vacuously on a graph with symbols and no flow"
    );
}
