//! The subcommand bodies. Query-class commands reuse `selene_mcp::handlers` — the same code the MCP
//! surface runs, so the CLI and the agent see the same graph the same way; only the exit-code
//! mapping (the CLI's `$?` contract) differs from MCP's `isError`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rmcp::ServiceExt;
use selene_db::SurrealStore;
use selene_extract::Indexer;
use selene_mcp::ToolOutcome;
use selene_mcp::handlers;

use crate::exit::Outcome;

/// Canonicalize a path, or a clear error.
fn resolve(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("no such path: {}", path.display()))
}

/// Print a `ToolOutcome`'s text and map it to a CLI exit: `Ok` → 0, `Failed` → 1.
fn render(outcome: ToolOutcome) -> Outcome {
    match outcome {
        ToolOutcome::Ok(text) => {
            println!("{text}");
            Outcome::Ok
        }
        ToolOutcome::Failed(text) => {
            eprintln!("{text}");
            Outcome::Failure
        }
    }
}

/// **The CLI's not-indexed asymmetry.** Query-class commands exit 1 on an un-indexed project (a
/// shell caller checks `$?`), whereas the MCP handlers return success-shaped guidance. So check
/// up front and refuse with a non-zero exit — the guidance text is still printed, so a human sees
/// what to do.
fn require_indexed(root: &Path) -> Result<(), Outcome> {
    if root.join(".selene").exists() {
        return Ok(());
    }
    eprintln!(
        "not indexed: {} has no .selene/. Run `selene index {}` first.",
        root.display(),
        root.display()
    );
    Err(Outcome::Failure)
}

/// Resolve the project root for a query command (an explicit `--path`, else the current dir), and
/// confirm it is indexed. Returns the not-indexed `Outcome` on failure.
fn query_root(path: Option<PathBuf>) -> Result<PathBuf, Outcome> {
    let root = path
        .or_else(|| std::env::current_dir().ok())
        .and_then(|p| p.canonicalize().ok())
        .ok_or(Outcome::Failure)?;
    require_indexed(&root)?;
    Ok(root)
}

// ---- lifecycle ------------------------------------------------------------------------------

/// `selene index` / the indexing half of `selene init` — extract, then resolve. **Both**: an index
/// without resolution is symbols with no flow, the dead-product shape.
pub async fn index(path: PathBuf) -> Outcome {
    match index_inner(path).await {
        Ok(()) => Outcome::Ok,
        Err(e) => {
            eprintln!("selene index: {e:#}");
            Outcome::Failure
        }
    }
}

async fn index_inner(path: PathBuf) -> Result<()> {
    let root = resolve(&path)?;
    let dir = root.join(".selene");
    std::fs::create_dir_all(&dir).context("could not create .selene/")?;

    let store = SurrealStore::open(&dir)
        .await
        .context("could not open the index")?;
    store
        .apply_schema()
        .await
        .context("could not apply the schema")?;

    eprintln!("indexing {} …", root.display());
    let indexer = Indexer::new(root.clone(), store);
    let result = indexer.index_all_deferring_fts(None).await;
    let store = indexer.into_store();
    eprintln!(
        "  {} files, {} nodes",
        result.files_indexed, result.nodes_created
    );

    eprintln!("resolving …");
    let (stats, fts) = tokio::join!(
        selene_resolve::resolve_and_persist_in_memory(&store, &root, result.unresolved, None),
        store.bulk_load_finish(),
    );
    fts.context("the FULLTEXT index build failed")?;
    let stats = stats.context("resolution failed")?;

    let (nodes, edges) = store.node_edge_count().await.unwrap_or((0, 0));
    eprintln!(
        "done: {nodes} nodes, {edges} edges ({} references bound)",
        stats.resolved
    );
    Ok(())
}

/// `selene init` — index by default. Already-initialized is an expected no-op (warn, exit 0).
pub async fn init(path: PathBuf, force: bool) -> Outcome {
    let Ok(root) = resolve(&path) else {
        eprintln!("selene init: no such path: {}", path.display());
        return Outcome::Failure;
    };
    if root.join(".selene").exists() && !force {
        eprintln!(
            "already initialized: {} (use --force to re-index)",
            root.display()
        );
        return Outcome::ExpectedNoOp;
    }
    index(root).await
}

/// `selene uninit` — remove `.selene/`. Not-there is an expected no-op (exit 0).
pub fn uninit(path: PathBuf, _force: bool) -> Outcome {
    let root = match path.canonicalize() {
        Ok(r) => r,
        Err(_) => return Outcome::ExpectedNoOp, // nothing to remove
    };
    let dir = root.join(".selene");
    if !dir.exists() {
        eprintln!("not initialized: {} has no .selene/", root.display());
        return Outcome::ExpectedNoOp;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            eprintln!("removed {}", dir.display());
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene uninit: could not remove {}: {e}", dir.display());
            Outcome::Failure
        }
    }
}

/// `selene status` — what the graph holds. Not indexed ⇒ guidance + exit 1.
pub async fn status(path: PathBuf, json: bool) -> Outcome {
    let root = match query_root(Some(path)) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match status_inner(&root, json).await {
        Ok(()) => Outcome::Ok,
        Err(e) => {
            eprintln!("selene status: {e:#}");
            Outcome::Failure
        }
    }
}

async fn status_inner(root: &Path, json: bool) -> Result<()> {
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("open index")?;
    let stats = store.stats().await.context("read stats")?;
    let last = store.last_indexed_at().await.ok().flatten();

    if json {
        // A minimal, stable JSON shape — no serde dependency for a handful of fields.
        let langs: Vec<String> = stats
            .languages
            .iter()
            .map(|(l, n)| format!("\"{l}\":{n}"))
            .collect();
        println!(
            "{{\"files\":{},\"nodes\":{},\"edges\":{},\"lastIndexed\":{},\"languages\":{{{}}}}}",
            stats.files,
            stats.nodes,
            stats.edges,
            last.map(|m| m.to_string()).unwrap_or_else(|| "null".into()),
            langs.join(",")
        );
        return Ok(());
    }

    println!("{}", root.display());
    println!("  files:  {}", stats.files);
    println!("  nodes:  {}", stats.nodes);
    println!("  edges:  {}", stats.edges);
    if let Some(ms) = last {
        println!("  indexed: {ms} (unix ms)");
    }
    if !stats.languages.is_empty() {
        let mut langs: Vec<String> = stats
            .languages
            .iter()
            .map(|(l, n)| format!("{l} ({n})"))
            .collect();
        langs.sort();
        println!("  languages: {}", langs.join(", "));
    }
    let mut kinds: Vec<(&String, &u64)> = stats.nodes_by_kind.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1));
    let top: Vec<String> = kinds
        .iter()
        .take(6)
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    if !top.is_empty() {
        println!("  node kinds: {}", top.join(", "));
    }
    Ok(())
}

// ---- query-class (reuse the MCP handlers) --------------------------------------------------

pub async fn explore(query: Vec<String>, path: Option<PathBuf>) -> Outcome {
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::explore(Some(root), &query.join(" ")).await)
}

pub async fn node(name: Option<String>, path: Option<PathBuf>, file: Option<String>) -> Outcome {
    let root = match query_root(path) {
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
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::search(Some(root), &search).await)
}

pub async fn callers(symbol: String, path: Option<PathBuf>) -> Outcome {
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::adjacency(Some(root), &symbol, true).await)
}

pub async fn callees(symbol: String, path: Option<PathBuf>) -> Outcome {
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::adjacency(Some(root), &symbol, false).await)
}

pub async fn impact(symbol: String, depth: u32, path: Option<PathBuf>) -> Outcome {
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::impact(Some(root), &symbol, depth).await)
}

pub async fn files(path: Option<PathBuf>, filter: Option<String>) -> Outcome {
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::files(Some(root), filter.as_deref()).await)
}

// ---- serve (unchanged from the pre-CLI binary) ---------------------------------------------

/// `selene serve --mcp` — stdio, handshake answers before any heavy init (opens the graph lazily).
pub async fn serve(mcp: bool, path: Option<PathBuf>) -> Outcome {
    if !mcp {
        eprintln!(
            "usage: selene serve --mcp --path <dir>\n\n\
             Only the MCP stdio transport is supported. Wire this into an agent's MCP config with \
             the ABSOLUTE path of this binary; see the README."
        );
        // Not a failure — a `serve` without `--mcp` prints help and exits 0 (map OQ6).
        return Outcome::ExpectedNoOp;
    }
    let root = path
        .or_else(|| std::env::current_dir().ok())
        .and_then(|p| p.canonicalize().ok());
    match serve_inner(root).await {
        Ok(()) => Outcome::Ok,
        Err(e) => {
            eprintln!("selene serve: {e:#}");
            Outcome::Failure
        }
    }
}

async fn serve_inner(root: Option<PathBuf>) -> Result<()> {
    let service = selene_mcp::SeleneMcp::new(root)
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP handshake failed")?;
    service.waiting().await.context("MCP server stopped")?;
    Ok(())
}

/// `selene sync` — incremental re-index of changed files (delegates to `selene_sync`).
pub async fn sync(path: PathBuf, quiet: bool) -> Outcome {
    let root = match query_root(Some(path)) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match selene_sync::sync_project(&root).await {
        Ok(stats) => {
            if !quiet {
                if stats.is_noop() {
                    eprintln!("up to date ({} files unchanged)", stats.unchanged);
                } else {
                    eprintln!(
                        "synced: {} changed, {} removed, {} unchanged",
                        stats.changed, stats.removed, stats.unchanged
                    );
                }
            }
            Outcome::Ok
        }
        Err(e) => {
            if !quiet {
                eprintln!("selene sync: {e:#}");
            }
            Outcome::Failure
        }
    }
}

/// `selene version` — the crate version. Exit 0.
pub fn version() -> Outcome {
    println!("selene {}", env!("CARGO_PKG_VERSION"));
    Outcome::Ok
}

/// A body that is not built yet. Prints a clear line to stderr and returns `Failure` — the
/// anti-inert-seam shape: the arm is wired and reachable, its test fails until a task fills it.
pub fn not_yet(name: &str, task: &str) -> Outcome {
    eprintln!("selene {name}: not yet implemented ({task})");
    Outcome::Failure
}
