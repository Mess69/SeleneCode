#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 7 — `ContextBuilder`: the bytes an agent actually reads.
//!
//! Every assertion here answers one question: **does this output stop the agent from opening
//! the file?** Not "is it correct" — correct-but-insufficient is a failed product.

mod common;

use common::{index_fixture, write_3_file_fixture};
use selene_context::{ContextBuilder, NOT_INDEXED};
use selene_db::SurrealStore;
use selene_graph::QueryManager;

async fn builder() -> (ContextBuilder<SurrealStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    (
        ContextBuilder::new(QueryManager::new(store, tmp.path().to_path_buf())),
        tmp,
    )
}

/// **The sufficiency assertion.** The output must carry the actual source, line-numbered, so
/// the agent can answer *and cite* without opening anything.
#[tokio::test(flavor = "multi_thread")]
async fn the_output_carries_verbatim_numbered_source() {
    let (b, _tmp) = builder().await;

    let out = b.build_context("hashPassword").await.unwrap();

    assert!(out.contains("hashPassword"), "the symbol is named");
    assert!(
        out.contains("input.length"),
        "THE BODY IS IN THE OUTPUT. Without it the agent knows the function exists and \
         nothing about what it does — so it opens the file, and we have lost the bet:\n{out}"
    );
    assert!(
        out.contains("\t"),
        "the source is LINE-NUMBERED (`<n>\\t<line>`), so the agent can cite file:line \
         exactly as if it had Read the file"
    );
    assert!(
        out.contains("**`src/crypto.ts`**"),
        "the file section header"
    );
}

/// Never tell the agent to Read. The only strings allowed to are the staleness/degraded
/// banners, and this crate emits neither.
#[tokio::test(flavor = "multi_thread")]
async fn no_output_string_ever_suggests_reading_the_file() {
    let (b, _tmp) = builder().await;

    for query in ["hashPassword", "zzzalpha zzzbeta", "how does this work"] {
        let out = b.build_context(query).await.unwrap().to_lowercase();
        assert!(
            !out.contains("read the file") && !out.contains("open the file"),
            "{query:?}: the output told the agent to Read — that is losing the product bet \
             on purpose:\n{out}"
        );
    }
}

/// **The low-confidence handoff.** Honest, actionable, and it does not send the agent to Read.
#[tokio::test(flavor = "multi_thread")]
async fn a_low_confidence_answer_says_so_and_says_what_to_do_next() {
    let (b, _tmp) = builder().await;

    let out = b.build_context("zzzalpha zzzbeta").await.unwrap();

    assert!(
        out.to_lowercase().contains("no relevant symbols found"),
        "when the graph cannot answer it must SAY so — thin context that LOOKS like an \
         answer is worse, because a confident wrong answer is the one failure mode an agent \
         cannot detect:\n{out}"
    );
    assert!(
        out.contains("What to do next"),
        "an honest miss must be ACTIONABLE, or the agent's only remaining move is Read"
    );
    assert!(
        out.contains("selene index") || out.contains("selene_explore"),
        "…and the next move is another TOOL CALL, not a file open"
    );
}

/// An un-indexed project is the most common first contact. Success-shaped, always.
#[tokio::test(flavor = "multi_thread")]
async fn an_unindexed_project_gets_guidance_not_an_error() {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let b = ContextBuilder::new(QueryManager::new(
        store,
        std::path::PathBuf::from("/nowhere"),
    ));

    let out = b
        .build_context("anything")
        .await
        .expect("NOT an Err — an isError here ends the session before it starts");

    assert_eq!(out, NOT_INDEXED);
    assert!(out.contains("selene index"));
}

/// Determinism: same graph + same query ⇒ byte-identical output.
#[tokio::test(flavor = "multi_thread")]
async fn the_output_is_byte_identical_across_runs() {
    let (b, _tmp) = builder().await;
    let a = b.build_context("hashPassword").await.unwrap();
    let c = b.build_context("hashPassword").await.unwrap();
    assert_eq!(a, c, "IndexMap everywhere the order can reach output");
}
