//! The query-class subcommands — thin wrappers over the shared MCP handlers.

#[cfg(feature = "semantic-search")]
use std::path::Path;
use std::path::PathBuf;

use selene_db::SurrealStore;
#[cfg(feature = "semantic-search")]
use selene_mcp::ToolOutcome;
use selene_mcp::handlers;

use crate::exit::Outcome;

use super::{query_root_direct, render};

// ---- query-class (reuse the MCP handlers) --------------------------------------------------

pub async fn explore(query: Vec<String>, path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::explore(Some(root), &query.join(" ")).await)
}

/// `selene query --raw` — the read-only SurrealQL passthrough (F6). CLI-only:
/// the MCP surface keeps its curated tools. Refusals exit 1 and write nothing.
pub async fn raw_query(search: String, path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let store = match SurrealStore::open(&root.join(".selene")).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("selene query --raw: could not open the index: {e}");
            return Outcome::Failure;
        }
    };
    match store.raw_select(&search).await {
        Ok(rows) => {
            eprintln!("selene query --raw: {} row(s)", rows.len());
            match serde_json::to_string_pretty(&rows) {
                Ok(out) => println!("{out}"),
                Err(e) => {
                    eprintln!("selene query --raw: unserializable rows: {e}");
                    return Outcome::Failure;
                }
            }
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene query --raw: {e}");
            Outcome::Failure
        }
    }
}

/// `selene insights` — the structural summary (same recipe as the MCP tool).
pub async fn insights(path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::insights(Some(root)).await)
}

pub async fn node(name: Option<String>, path: Option<PathBuf>, file: Option<String>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    // `node` takes a symbol name; the file-read variant is deferred (map §Subcommands `node`).
    let Some(symbol) = name.or(file) else {
        eprintln!("selene node: give a symbol name (or --file <path>)");
        return Outcome::Failure;
    };
    render(handlers::node(Some(root), &symbol).await)
}

pub async fn query(search: String, path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    // If this index has embeddings (a `selene embed` was run) and this binary can generate a query
    // vector, use HYBRID search — it bridges the vocabulary gap that lexical BM25 cannot.
    #[cfg(feature = "semantic-search")]
    if let Some(outcome) = semantic_query(&root, &search).await {
        return outcome;
    }
    render(handlers::search(Some(root), &search).await)
}

/// Hybrid (lexical + vector) search. `None` when the index has no embeddings or the query cannot be
/// embedded — the caller then falls back to lexical. Loads the model per call (the daemon holds it
/// warm; a one-shot CLI query pays the load, which is the trade for `selene query` from a shell).
#[cfg(feature = "semantic-search")]
async fn semantic_query(root: &Path, query: &str) -> Option<Outcome> {
    let store = SurrealStore::open(&root.join(".selene")).await.ok()?;
    if !store.has_embeddings().await.unwrap_or(false) {
        return None; // lexical-only index — nothing to fuse
    }
    let mut embedder = selene_embed::Embedder::load().ok()?;
    let qvec = embedder.embed_query(query).ok()?;
    let cands = store.hybrid_search(query, &qvec, &[], &[], 20).await.ok()?;

    let mut out = format!("## Symbols matching `{query}` (hybrid: semantic + lexical)\n\n");
    if cands.is_empty() {
        out.push_str("_No matches._\n");
    }
    for c in &cands {
        out.push_str(&format!(
            "- `{}` — {} ({}:{})\n",
            c.node.name,
            c.node.kind.as_str(),
            c.node.file_path,
            c.node.start_line
        ));
    }
    Some(render(ToolOutcome::guidance(out)))
}

pub async fn callers(symbol: String, path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::adjacency(Some(root), &symbol, true).await)
}

pub async fn callees(symbol: String, path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::adjacency(Some(root), &symbol, false).await)
}

pub async fn impact(symbol: String, depth: u32, path: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::impact(Some(root), &symbol, depth).await)
}

pub async fn files(path: Option<PathBuf>, filter: Option<String>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::files(Some(root), filter.as_deref()).await)
}

/// `selene affected` — the files whose graph depends on the given files, BFS to `depth`.
/// No inputs is an expected no-op (exit 0). Reads dependents from the store.
pub async fn affected(
    files: Vec<String>,
    use_stdin: bool,
    depth: u32,
    path: Option<PathBuf>,
) -> Outcome {
    let mut seeds = files;
    if use_stdin {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let l = line.trim();
            if !l.is_empty() {
                seeds.push(l.to_string());
            }
        }
    }
    if seeds.is_empty() {
        return Outcome::ExpectedNoOp; // nothing to expand — a valid, empty request
    }
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let store = match SurrealStore::open(&root.join(".selene")).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("selene affected: {e:#}");
            return Outcome::Failure;
        }
    };
    // BFS over dependents. `seen` is the answer; `frontier` the current ring.
    let mut seen: std::collections::BTreeSet<String> = seeds.iter().cloned().collect();
    let mut frontier: Vec<String> = seeds;
    for _ in 0..depth {
        let mut next: Vec<String> = Vec::new();
        for f in &frontier {
            match store.dependent_file_paths(f).await {
                Ok(deps) => {
                    for d in deps {
                        if seen.insert(d.clone()) {
                            next.push(d);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("selene affected: {e:#}");
                    return Outcome::Failure;
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    for f in &seen {
        println!("{f}");
    }
    Outcome::Ok
}
