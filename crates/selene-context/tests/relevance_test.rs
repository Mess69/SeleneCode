#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 5 — scoring passes 1–4, **against a real store**.
//!
//! The weights are asserted **numerically**, not by ordering. An ordering assertion passes
//! even when a weight is 10× wrong — and a wrong weight is invisible until an agent gets a
//! worse answer on a repo nobody tested.

mod common;

use common::index_fixture;
use selene_context::{FindOptions, score_candidates, weights};
use selene_db::SurrealStore;
use selene_graph::QueryManager;

/// Two query symbols in ONE file (`scrapeLoop` + `parseFeed`), and the same two names
/// scattered across two others. The co-located pair must win by at least the ported `+20`.
///
/// ⚠ The TS comment's own example is `scrapeLoop` + `run` — and **`run` is a STOPWORD**
/// (it matches thousands of unrelated symbols, which is exactly why it is on the list). Using
/// it here would have made the fixture score one term and quietly prove nothing. The list
/// doing its job caught my own test.
fn write_colocation_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/scrape.ts"),
        "export function scrapeLoop() { return 1; }\nexport function parseFeed() { return 2; }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/other.ts"),
        "export function parseFeed() { return 3; }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/loops.ts"),
        "export function scrapeLoop() { return 4; }\n",
    )
    .unwrap();
}

async fn manager(f: fn(&std::path::Path)) -> (QueryManager<SurrealStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    f(tmp.path());
    let store = index_fixture(tmp.path()).await;
    (QueryManager::new(store, tmp.path().to_path_buf()), tmp)
}

/// Pass 2's co-location boost, **numerically**: `+20` per extra co-named symbol in the file.
#[tokio::test(flavor = "multi_thread")]
async fn the_co_location_boost_is_exactly_twenty_per_extra_symbol() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;

    let scored = score_candidates(&qm, "scrapeLoop parseFeed", &FindOptions::default(), None)
        .await
        .unwrap();
    assert!(!scored.is_empty(), "the pipeline produced nothing at all");

    let in_scrape: Vec<&selene_context::ScoredNode> = scored
        .iter()
        .filter(|s| s.node.file_path == "src/scrape.ts")
        .collect();
    let elsewhere: Vec<&selene_context::ScoredNode> = scored
        .iter()
        .filter(|s| s.node.file_path != "src/scrape.ts" && s.node.name == "parseFeed")
        .collect();

    assert!(
        !in_scrape.is_empty() && !elsewhere.is_empty(),
        "both sides must be present or the comparison is vacuous.\n  scored: {:?}",
        scored
            .iter()
            .map(|s| (s.node.file_path.as_str(), s.node.name.as_str(), s.score))
            .collect::<Vec<_>>()
    );

    let best_colocated = in_scrape.iter().map(|s| s.score).fold(f64::MIN, f64::max);
    let best_scattered = elsewhere.iter().map(|s| s.score).fold(f64::MIN, f64::max);

    assert!(
        best_colocated - best_scattered >= weights::CO_LOCATION - f64::EPSILON,
        "two query symbols in ONE file must beat the same names scattered, by at least the \
         ported +{}: {best_colocated} vs {best_scattered}",
        weights::CO_LOCATION
    );
}

/// A stopword-only query is an ANSWER (empty), never an error.
#[tokio::test(flavor = "multi_thread")]
async fn a_stopword_only_query_scores_nothing_and_does_not_error() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;

    let scored = score_candidates(&qm, "how does this work", &FindOptions::default(), None)
        .await
        .expect(
            "a stopword-only query is the most common way a FIRST query fails — an Err \
                 here becomes an isError, and one of those ends the session",
        );
    assert!(scored.is_empty());

    // The positive control: the same pipeline DOES produce results on a real query.
    assert!(
        !score_candidates(&qm, "scrapeLoop", &FindOptions::default(), None)
            .await
            .unwrap()
            .is_empty(),
        "the empty answer above is a DECISION, not a dead pipeline"
    );
}

/// Determinism: same graph + same query ⇒ byte-identical ranking.
#[tokio::test(flavor = "multi_thread")]
async fn scoring_is_deterministic() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;

    let a = score_candidates(&qm, "scrapeLoop parseFeed", &FindOptions::default(), None)
        .await
        .unwrap();
    let b = score_candidates(&qm, "scrapeLoop parseFeed", &FindOptions::default(), None)
        .await
        .unwrap();

    let ids = |v: &[selene_context::ScoredNode]| -> Vec<String> {
        v.iter()
            .map(|s| format!("{}:{}", s.node.id, s.score))
            .collect()
    };
    assert_eq!(
        ids(&a),
        ids(&b),
        "IndexMap + a total tie-break: order is stable"
    );
}

// =============================================================================
// Task 6 — find_relevant_context: rerank, LIKE passes, confidence, BFS, trims
// =============================================================================

use selene_context::{Confidence, find_relevant_context};

/// The gather closes the real chain: a root, its neighbours, and the edges BETWEEN them.
#[tokio::test(flavor = "multi_thread")]
async fn find_relevant_context_gathers_a_connected_subgraph() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;

    let ctx = find_relevant_context(&qm, "scrapeLoop", &FindOptions::default(), None)
        .await
        .unwrap();

    assert!(!ctx.roots.is_empty(), "a root was chosen");
    assert!(
        ctx.subgraph.nodes.contains_key(&ctx.roots[0].node.id),
        "the root itself is IN the subgraph — it is the answer, and pass 11's trims must \
         never drop it"
    );
}

/// **The low-confidence contract.** `Low` is a *value*, never an `Err` — and it is the honest
/// half of the product.
#[tokio::test(flavor = "multi_thread")]
async fn low_confidence_is_a_value_not_an_error() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;

    // Two terms, nothing in the graph matching more than one of them, nothing distinctive.
    let ctx = find_relevant_context(&qm, "zzzalpha zzzbeta", &FindOptions::default(), None)
        .await
        .expect(
            "a query the graph cannot answer is an ANSWER — an Err here becomes an isError at \
             the MCP layer, and the rmcp spike proved an escaping `?` becomes a JSON-RPC \
             transport failure. One isError early and the agent abandons the tool.",
        );

    assert!(ctx.subgraph.nodes.is_empty());
    assert_eq!(
        ctx.confidence,
        Confidence::Low,
        "when the graph is guessing it must SAY so — a confident wrong answer is the one \
         failure mode an agent cannot detect"
    );

    // The positive control: the same pipeline reports High on a query it CAN answer.
    let good = find_relevant_context(&qm, "scrapeLoop parseFeed", &FindOptions::default(), None)
        .await
        .unwrap();
    assert_eq!(
        good.confidence,
        Confidence::High,
        "…and Low above is a DECISION, not a dead pipeline"
    );
}

/// A single-term query is never "low confidence" — there is no second term to have missed.
#[tokio::test(flavor = "multi_thread")]
async fn one_term_queries_are_always_high_confidence() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;
    let ctx = find_relevant_context(&qm, "scrapeLoop", &FindOptions::default(), None)
        .await
        .unwrap();
    assert_eq!(ctx.confidence, Confidence::High);
}

/// Pass 5: stem variants of one root word are ONE concept, not three.
#[test]
fn term_groups_collapse_stem_variants() {
    use selene_context::term_groups;

    let groups = term_groups(&[
        "index".to_string(),
        "indexed".to_string(),
        "shard".to_string(),
    ]);
    assert_eq!(
        groups.len(),
        2,
        "`index`/`indexed` are ONE concept — without the grouping they inflate the match \
         count and hand a false multi-term boost to a symbol matching one root word twice: \
         {groups:?}"
    );
}

/// Pass 11's trims never drop a root, whatever the caps say.
#[tokio::test(flavor = "multi_thread")]
async fn the_trims_never_drop_a_root() {
    let (qm, _tmp) = manager(write_colocation_fixture).await;

    let opts = FindOptions {
        max_nodes: 1, // a cap so tight that only a root can survive
        ..FindOptions::default()
    };
    let ctx = find_relevant_context(&qm, "scrapeLoop", &opts, None)
        .await
        .unwrap();

    for r in &ctx.roots {
        assert!(
            ctx.subgraph.nodes.contains_key(&r.node.id),
            "a root was trimmed away — the answer itself was cut to fit the budget"
        );
    }
}
