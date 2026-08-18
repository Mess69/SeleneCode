//! `selene export` — open the store, render via [`crate::export`], write bytes.
//!
//! stdout by default: an export exists to be piped (`selene export | jq`),
//! imported (Gephi/yEd via `--format graphml`), or diffed — unlike `viz`/
//! `report`, whose outputs are documents with a natural home on disk. `--out`
//! writes a file and prints its path (the `viz` contract).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use selene_db::SurrealStore;

use crate::exit::Outcome;
use crate::export::ExportFormat;

use super::query_root_direct;

pub async fn export(path: Option<PathBuf>, format: String, out: Option<PathBuf>) -> Outcome {
    let Some(format) = ExportFormat::parse(&format) else {
        eprintln!(
            "selene export: unknown format `{format}` (expected json | jsonl | graphml | dot)"
        );
        return Outcome::Failure;
    };
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match export_inner(&root, format, out).await {
        Ok(Some(dest)) => {
            eprintln!("selene export: {} written", format.as_str());
            println!("{}", dest.display());
            Outcome::Ok
        }
        Ok(None) => Outcome::Ok, // already streamed to stdout
        Err(e) => {
            eprintln!("selene export: {e:#}");
            Outcome::Failure
        }
    }
}

async fn export_inner(
    root: &Path,
    format: ExportFormat,
    out: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("could not open the index")?;
    let nodes = store.all_nodes().await.context("read nodes")?;
    let edges = store.all_edges().await.context("read edges")?;
    let rendered = crate::export::render(format, &nodes, &edges, &root.display().to_string());

    match out {
        Some(dest) => {
            std::fs::write(&dest, rendered)
                .with_context(|| format!("could not write {}", dest.display()))?;
            Ok(Some(dest))
        }
        None => {
            print!("{rendered}");
            Ok(None)
        }
    }
}
