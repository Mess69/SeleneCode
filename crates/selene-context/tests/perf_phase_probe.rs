//! A phase-resolution probe for the indexing pipeline. **Not a gate — a measuring stick.**
//!
//! The Phase-4 review measured ~1 s/file of resolution (280 s of a 307 s index over 289
//! files). Extrapolated to the milestone gate's dogfood repo (VS Code, 11,938 files) that is
//! over three hours, which breaks two things at once: the milestone gate cannot be run, and —
//! far worse — the PRD's promise of indexing *faster than the TS build* is false. A user's
//! first `selene index` on a real repo would look like a hang.
//!
//! One opaque number cannot tell you which phase to fix, and the six sub-phases of
//! `resolve_and_persist_batched` have completely different fixes. So this probe drives the
//! **real** `Indexer` → **real** `resolve_and_persist_batched` over a **real** repo and lets
//! the `tracing` spans in `batch.rs` report each phase separately.
//!
//! `#[ignore]` on purpose: it indexes hundreds of files and takes minutes. Run it by hand.
//!
//! ```text
//! cargo test -p selene-context --test perf_phase_probe -- --ignored --nocapture
//! SELENE_PROBE_REPO=../vscode cargo test -p selene-context --test perf_phase_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

use selene_db::SurrealStore;
use selene_extract::Indexer;

/// Default corpus: the TS parity source. A real repo, ~300 source files — big enough that a
/// per-file cost is visible, small enough to iterate on.
///
/// Sibling of the workspace root — NOT of this crate. `cargo test` runs with the CWD at the
/// *package* root, so a bare `../codegraph` resolves to `crates/codegraph` and finds nothing.
const DEFAULT_REPO: &str = "../../../codegraph";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "minutes-long; a measuring stick, not a gate"]
async fn where_does_indexing_time_actually_go() {
    // Print the `tracing::info!` phase spans that `batch.rs` emits. Without a subscriber the
    // library is silent, which is exactly why nobody had these numbers.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .try_init();

    let repo = std::env::var("SELENE_PROBE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let root = PathBuf::from(&repo);
    assert!(
        root.is_dir(),
        "probe corpus {repo} is not a directory — clone it or set SELENE_PROBE_REPO"
    );

    let store = SurrealStore::in_memory().await.expect("store");

    let t = Instant::now();
    let indexer = Indexer::new(root.clone(), store.clone());
    let __ix = indexer.index_all(None).await;
    let result = &__ix;
    let ms_extract = t.elapsed().as_millis();
    let files = result.files_indexed;
    assert!(files > 0, "indexed nothing from {repo} — wrong path?");

    let t = Instant::now();
    let stats =
        selene_resolve::resolve_and_persist_in_memory(&store, &root, __ix.unresolved.clone(), None)
            .await
            .expect("resolution");
    let ms_resolve = t.elapsed().as_millis();

    let total = ms_extract + ms_resolve;
    let per_file = total as f64 / files as f64;

    println!("\n================ {repo} ================");
    println!("files indexed     {files}");
    println!(
        "extraction        {:>8} ms  ({:.0}%)",
        ms_extract,
        100.0 * ms_extract as f64 / total.max(1) as f64
    );
    println!(
        "resolution        {:>8} ms  ({:.0}%)   <- the phase spans above break this down",
        ms_resolve,
        100.0 * ms_resolve as f64 / total.max(1) as f64
    );
    println!("TOTAL             {total:>8} ms");
    println!("per file          {per_file:>8.1} ms");
    println!("resolved refs     {:>8}", stats.resolved);
    println!("store read errors {:>8}", stats.store_read_errors);
    // The number that decides whether the milestone gate is runnable at all.
    println!(
        "\nEXTRAPOLATED to VS Code (11,938 files): {:.1} minutes",
        per_file * 11_938.0 / 60_000.0
    );
    println!("========================================\n");
}
