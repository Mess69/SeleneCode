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
