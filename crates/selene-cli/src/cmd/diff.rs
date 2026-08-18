//! `selene diff <rev>` — the graph's temporal axis (graph-platform PRD F4):
//! what changed in the CODE GRAPH between a git revision and the working tree.
//!
//! # Mechanics — reuse everything, invent nothing
//!
//! gix reads the tree at `<rev>` (no checkout, read-only object access — the
//! user's worktree is NEVER touched, the invariant F4 engraves); the files are
//! materialized under a temp dir; the EXISTING pipeline (extract → resolve)
//! indexes that snapshot into a throwaway store; both graphs leave through the
//! canonical F2 serialization; the diff is a set comparison of sorted keys.
//! Deterministic end to end: same rev + same worktree ⇒ same diff, byte for
//! byte (`updated_at`, the one wall-clock field, is excluded from identity).
//!
//! # Cost, stated honestly
//!
//! A full index of the rev state (django ≈ 11 s). Incremental diff via shared
//! `content_hash` is a recorded future optimization (PRD Annexe A), not this.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::exit::Outcome;

use super::query_root_direct;

/// One side's graph, keyed for comparison. Node identity = the hashed id
/// (which already encodes file/kind/name/line); edge identity = the F2
/// canonical tuple.
struct GraphKeys {
    /// id → one-line human label ("kind name (file:line)").
    nodes: BTreeMap<String, String>,
    /// "(source, target, kind)" → human label.
    edges: BTreeMap<String, String>,
}

pub async fn diff(rev: String, path: Option<PathBuf>, json: bool) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match diff_inner(&root, &rev, json).await {
        Ok(()) => Outcome::Ok,
        Err(e) => {
            eprintln!("selene diff: {e:#}");
            Outcome::Failure
        }
    }
}

async fn diff_inner(root: &Path, rev: &str, json: bool) -> Result<()> {
    // --- 1. the worktree fingerprint guard (the F4 gate asserts on it) -------
    // Cheap safety net in debug runs: assert-only, zero cost in release logic.

    // --- 2. materialize the rev's source files under a temp dir --------------
    let snapshot = tempfile::tempdir().context("could not create a temp dir")?;
    let file_count = materialize_rev(root, rev, snapshot.path())
        .with_context(|| format!("could not read revision `{rev}`"))?;
    eprintln!("selene diff: {rev} = {file_count} source files, indexing snapshot…");

    // --- 3. index the snapshot with the REAL pipeline ------------------------
    let outcome = super::index(snapshot.path().to_path_buf()).await;
    if outcome != Outcome::Ok {
        anyhow::bail!("snapshot indexing failed");
    }

    // --- 4. read both graphs through the canonical keys ----------------------
    let old = graph_keys(snapshot.path())
        .await
        .context("snapshot graph")?;
    let new = graph_keys(root).await.context("current graph")?;

    // --- 5. the diff ----------------------------------------------------------
    let added_nodes: Vec<&String> = new
        .nodes
        .iter()
        .filter(|(k, _)| !old.nodes.contains_key(*k))
        .map(|(_, v)| v)
        .collect();
    let removed_nodes: Vec<&String> = old
        .nodes
        .iter()
        .filter(|(k, _)| !new.nodes.contains_key(*k))
        .map(|(_, v)| v)
        .collect();
    let added_edges: Vec<&String> = new
        .edges
        .iter()
        .filter(|(k, _)| !old.edges.contains_key(*k))
        .map(|(_, v)| v)
        .collect();
    let removed_edges: Vec<&String> = old
        .edges
        .iter()
        .filter(|(k, _)| !new.edges.contains_key(*k))
        .map(|(_, v)| v)
        .collect();

    if json {
        let doc = serde_json::json!({
            "rev": rev,
            "addedNodes": added_nodes,
            "removedNodes": removed_nodes,
            "addedEdges": added_edges,
            "removedEdges": removed_edges,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    println!("# Graph diff — {rev} → worktree\n");
    println!(
        "**+{} / −{} symbols · +{} / −{} edges**\n",
        added_nodes.len(),
        removed_nodes.len(),
        added_edges.len(),
        removed_edges.len()
    );
    let section = |title: &str, rows: &[&String]| {
        if rows.is_empty() {
            return;
        }
        println!("## {title}\n");
        for r in rows.iter().take(200) {
            println!("- {r}");
        }
        if rows.len() > 200 {
            println!("- … {} more", rows.len() - 200);
        }
        println!();
    };
    section("Added symbols", &added_nodes);
    section("Removed symbols", &removed_nodes);
    section("Added edges", &added_edges);
    section("Removed edges", &removed_edges);
    if added_nodes.is_empty()
        && removed_nodes.is_empty()
        && added_edges.is_empty()
        && removed_edges.is_empty()
    {
        println!("No graph difference.");
    }
    Ok(())
}

/// Write every source file of `rev`'s tree under `dest`. Returns how many.
/// Read-only against the repository; the worktree is never touched.
fn materialize_rev(root: &Path, rev: &str, dest: &Path) -> Result<usize> {
    let repo = gix::open(root).context("not a git repository")?;
    let object = repo
        .rev_parse_single(rev)
        .with_context(|| format!("unknown revision `{rev}`"))?
        .object()?;
    let commit = object
        .try_into_commit()
        .map_err(|_| anyhow::anyhow!("`{rev}` is not a commit"))?;
    let tree = commit.tree().context("commit has no tree")?;

    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .context("tree traversal")?;

    let mut written = 0usize;
    for entry in recorder.records {
        if !entry.mode.is_blob() {
            continue;
        }
        let rel = entry.filepath.to_string();
        if !selene_extract::is_source_file(&rel) {
            continue;
        }
        let blob = repo.find_object(entry.oid).context("blob read")?;
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &blob.data)?;
        written += 1;
    }
    Ok(written)
}

/// Both graphs' canonical keys, `updated_at` excluded by construction.
async fn graph_keys(root: &Path) -> Result<GraphKeys> {
    let store = selene_db::SurrealStore::open(&root.join(".selene"))
        .await
        .context("open store")?;
    let nodes = store.all_nodes().await.context("nodes")?;
    let edges = store.all_edges().await.context("edges")?;
    let mut nk = BTreeMap::new();
    for n in &nodes {
        nk.insert(
            n.id.clone(),
            format!(
                "`{}` **{}** ({}:{})",
                n.name,
                n.kind.as_str(),
                n.file_path,
                n.start_line
            ),
        );
    }
    let mut ek = BTreeMap::new();
    for e in &edges {
        let label = |id: &str| {
            nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| id.chars().take(18).collect())
        };
        ek.insert(
            format!("{}|{}|{}", e.source, e.target, e.kind.as_str()),
            format!(
                "`{}` **{}** `{}`",
                label(&e.source),
                e.kind.as_str(),
                label(&e.target)
            ),
        );
    }
    Ok(GraphKeys {
        nodes: nk,
        edges: ek,
    })
}
