//! The subcommand bodies. Query-class commands reuse `selene_mcp::handlers` — the same code the MCP
//! surface runs, so the CLI and the agent see the same graph the same way; only the exit-code
//! mapping (the CLI's `$?` contract) differs from MCP's `isError`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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

    // Migration escape hatch: `SELENE_BACKEND=ladybug selene index <path>` indexes into a LadybugDB
    // store (`.selene-lbug/`) instead of SurrealDB — the head-to-head measurement path. Off unless
    // both the feature is compiled and the env var is set, so the default binary is untouched.
    #[cfg(feature = "kv-ladybug")]
    if std::env::var("SELENE_BACKEND").as_deref() == Ok("ladybug") {
        return index_inner_ladybug(root).await;
    }

    // A running daemon holds the exclusive lock. A full re-index can't open the store past it, so
    // rather than surface a cryptic RocksDB lock error, point the user at the two real options.
    if let Some(pid) = selene_mcp::daemon::running_pid(&root) {
        anyhow::bail!(
            "a SeleneCode daemon is serving this project (pid {pid}).\n  \
             • for incremental updates, use `selene sync` (it re-indexes through the daemon)\n  \
             • to fully re-index, stop the daemon first: `kill {pid}`"
        );
    }

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

    // **Scale gate for the write path.** Concurrent writes (edge inserts, and the FTS build
    // overlapped with resolve) are a large win on small/medium repos but collide on RocksDB's
    // optimistic-transaction layer at VS Code scale (~257k nodes / ~1.2M edges), aborting the resolve
    // or live-locking. Above the threshold we serialize: `set_serialize_writes` reaches every writer
    // (shared across store clones), and the FTS builds AFTER resolve instead of overlapping it.
    const LARGE_REPO_NODES: usize = 100_000;
    let large = result.nodes_created as usize > LARGE_REPO_NODES;
    store.set_serialize_writes(large);

    eprintln!("resolving …");
    let stats = if large {
        // Serial: resolve, then FTS. Correct at any size; the only path that survives VS Code.
        let stats =
            selene_resolve::resolve_and_persist_in_memory(&store, &root, result.unresolved, None)
                .await
                .context("resolution failed")?;
        store.bulk_load_finish().await.context("the FULLTEXT index build failed")?;
        stats
    } else {
        // Overlap the FTS build with resolve on a separate worker — hidden behind the resolve on
        // repos where the concurrent writes don't contend.
        let store_fts = store.clone();
        let fts_handle = tokio::spawn(async move { store_fts.bulk_load_finish().await });
        let stats =
            selene_resolve::resolve_and_persist_in_memory(&store, &root, result.unresolved, None)
                .await
                .context("resolution failed")?;
        fts_handle
            .await
            .context("the FULLTEXT index task panicked")?
            .context("the FULLTEXT index build failed")?;
        stats
    };

    let (nodes, edges) = store.node_edge_count().await.unwrap_or((0, 0));
    eprintln!(
        "done: {nodes} nodes, {edges} edges ({} references bound)",
        stats.resolved
    );
    Ok(())
}

/// The LadybugDB index path (feature `kv-ladybug`, `SELENE_BACKEND=ladybug`). Parallel to
/// `index_inner` but with no scale gate — LadybugDB's bulk `COPY` writes don't contend the way
/// SurrealDB's concurrent `INSERT RELATION` does, so there is nothing to serialize.
#[cfg(feature = "kv-ladybug")]
async fn index_inner_ladybug(root: PathBuf) -> Result<()> {
    use selene_db::{GraphStore, LadybugStore};
    // lbug creates the store directory itself and refuses an existing plain dir, so start fresh.
    let dbdir = root.join(".selene-lbug");
    let _ = std::fs::remove_dir_all(&dbdir);
    let store = LadybugStore::open(&dbdir)
        .await
        .context("could not open the LadybugDB index")?;

    eprintln!("indexing {} (LadybugDB backend) …", root.display());
    let indexer = Indexer::new(root.clone(), store);
    let result = indexer.index_all_deferring_fts(None).await;
    let store = indexer.into_store();
    eprintln!(
        "  {} files, {} nodes",
        result.files_indexed, result.nodes_created
    );

    // Flush the extraction into the store as one node COPY + one edge COPY (Kuzu's fast fresh-table
    // path) BEFORE resolve reads the eager node scan. The pipeline buffered all extraction writes;
    // this is the extraction→resolve boundary.
    store.flush_bulk().await.context("flushing the extraction to LadybugDB failed")?;

    eprintln!("resolving …");
    let stats =
        selene_resolve::resolve_and_persist_in_memory(&store, &root, result.unresolved, None)
            .await
            .context("resolution failed")?;
    store.bulk_load_finish().await.ok();

    let (nodes, edges) = store.node_edge_count().await.unwrap_or((0, 0));
    eprintln!(
        "done: {nodes} nodes, {edges} edges ({} references bound)",
        stats.resolved
    );
    Ok(())
}

/// `selene init` — index by default. Already-initialized is an expected no-op (warn, exit 0).
pub async fn init(path: PathBuf, force: bool, no_hooks: bool) -> Outcome {
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
    let outcome = index(root.clone()).await;
    // Install the git hooks that keep the index fresh after commit/merge/checkout. Best effort:
    // outside a git repo it is a silent no-op, and a hook failure never fails `init`.
    if !no_hooks && matches!(outcome, Outcome::Ok) {
        install_hooks(&root);
    }
    outcome
}

/// Install the git sync hooks with selene's absolute path. Never fails the caller.
fn install_hooks(root: &Path) {
    let Ok(binary) = std::env::current_exe() else {
        return;
    };
    match selene_sync::hooks::install(root, &binary) {
        Ok(results) if !results.is_empty() => {
            let n = results.iter().filter(|r| r.action != "unchanged").count();
            if n > 0 {
                eprintln!("installed {n} git sync hook(s) (post-commit/merge/checkout)");
            }
        }
        Ok(_) => {} // not a git repo — nothing to do
        Err(e) => eprintln!("selene init: could not install git hooks: {e}"),
    }
}

/// `selene uninit` — remove `.selene/` and the git sync hooks. Not-there is an expected no-op.
pub fn uninit(path: PathBuf, _force: bool) -> Outcome {
    let root = match path.canonicalize() {
        Ok(r) => r,
        Err(_) => return Outcome::ExpectedNoOp, // nothing to remove
    };
    // Strip our git hooks first (best effort, never fails uninit).
    if let Err(e) = selene_sync::hooks::remove(&root) {
        eprintln!("selene uninit: could not remove git hooks: {e}");
    }
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

    // Warn if the caller's git worktree differs from the one this index was built for.
    if let Some(m) = std::env::current_dir()
        .ok()
        .and_then(|cwd| selene_sync::worktree::detect(&cwd, root))
    {
        println!("\n{}", m.status_warning());
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

// ---- viz (self-contained interactive graph) ------------------------------------------------

/// `selene viz` — export the whole graph to ONE self-contained interactive HTML
/// page (a dark "galaxy"). Opens the store like every other query command, reads
/// all nodes + edges, caps/filters them (see [`crate::viz`]), and writes the file.
/// The written path goes to **stdout** (so a script can capture it); the summary
/// and any browser-open go to stderr.
pub async fn viz(
    path: Option<PathBuf>,
    out: Option<PathBuf>,
    max_nodes: usize,
    all_kinds: bool,
    open: bool,
) -> Outcome {
    let root = match query_root(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match viz_inner(&root, out, max_nodes, all_kinds).await {
        Ok((dest, doc)) => {
            eprintln!(
                "selene viz: {} of {} nodes, {} of {} edges",
                doc.shown_nodes, doc.total_nodes, doc.shown_edges, doc.total_edges
            );
            println!("{}", dest.display());
            if open {
                open_in_browser(&dest);
            }
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene viz: {e:#}");
            Outcome::Failure
        }
    }
}

async fn viz_inner(
    root: &Path,
    out: Option<PathBuf>,
    max_nodes: usize,
    all_kinds: bool,
) -> Result<(PathBuf, crate::viz::VizDoc)> {
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("could not open the index")?;
    let nodes = store.all_nodes().await.context("read nodes")?;
    let edges = store.all_edges().await.context("read edges")?;

    let opts = crate::viz::VizOptions {
        max_nodes,
        all_kinds,
        root_label: root.display().to_string(),
    };
    let doc = crate::viz::build_html(&nodes, &edges, &opts);

    let dest = out.unwrap_or_else(|| root.join("selene-graph.html"));
    std::fs::write(&dest, &doc.html)
        .with_context(|| format!("could not write {}", dest.display()))?;
    Ok((dest, doc))
}

/// Best-effort: open `path` in the OS default browser. Never fails the command —
/// the file is already written and its path was printed.
fn open_in_browser(path: &Path) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let program = "xdg-open";

    if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
        eprintln!(
            "selene viz: could not open a browser ({e}); open {} yourself",
            path.display()
        );
    }
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
    // The daemon launcher decides direct-vs-proxy-vs-daemon (see `selene_mcp::daemon`).
    selene_mcp::daemon::launch(root).await
}

/// `selene daemon` — list the running SeleneCode daemons across all projects. Exit 0 even when
/// none are running (an empty list is a fact, not a failure — map exit-code semantics).
pub fn daemon() -> Outcome {
    let daemons = selene_mcp::daemon::list_daemons();
    if daemons.is_empty() {
        println!("No SeleneCode daemons are running.");
        return Outcome::Ok;
    }
    println!("{} daemon(s) running:\n", daemons.len());
    for d in &daemons {
        println!("  pid {:<8} v{:<8} {}", d.pid, d.version, d.root);
        println!("           socket: {}", d.socket_path);
    }
    Outcome::Ok
}

/// `selene sync` — incremental re-index of changed files (delegates to `selene_sync`).
pub async fn sync(path: PathBuf, quiet: bool) -> Outcome {
    let root = match query_root(Some(path)) {
        Ok(r) => r,
        Err(o) => return o,
    };

    // A daemon holds the exclusive lock, so if one is up we route the sync THROUGH it (it re-indexes
    // against its warm store and the change is instantly visible to the next query). Only when there
    // is no daemon do we open the store directly.
    match selene_mcp::daemon::route_to_daemon(&root, "sync").await {
        Ok(Some(reply)) => {
            if reply.ok {
                if !quiet {
                    if reply.changed == 0 && reply.removed == 0 {
                        eprintln!(
                            "up to date ({} files unchanged) [via daemon]",
                            reply.unchanged
                        );
                    } else {
                        eprintln!(
                            "synced: {} changed, {} removed, {} unchanged [via daemon]",
                            reply.changed, reply.removed, reply.unchanged
                        );
                    }
                }
                return Outcome::Ok;
            }
            if !quiet {
                eprintln!(
                    "selene sync: {}",
                    reply.error.as_deref().unwrap_or("daemon reported failure")
                );
            }
            return Outcome::Failure;
        }
        // No daemon (or connect failed) — fall through to the direct path.
        Ok(None) => {}
        Err(e) => {
            if !quiet {
                eprintln!("selene sync: could not reach the daemon ({e}); syncing directly");
            }
        }
    }

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

/// `selene embed` — add semantic (vector) search to an existing index. Embeds every searchable
/// symbol locally (no API) and builds the HNSW index, so a later search fuses meaning with tokens.
#[cfg(feature = "semantic-search")]
pub async fn embed(path: PathBuf) -> Outcome {
    use selene_core::NodeKind;
    let root = match query_root(Some(path)) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let store = match SurrealStore::open(&root.join(".selene")).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("selene embed: {e:#}");
            return Outcome::Failure;
        }
    };
    let nodes = match store.all_nodes().await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("selene embed: {e:#}");
            return Outcome::Failure;
        }
    };
    // Skip the non-searchable noise — you never search for a file node, an import, or a parameter.
    let searchable = |k: NodeKind| {
        !matches!(
            k,
            NodeKind::File | NodeKind::Import | NodeKind::Export | NodeKind::Parameter
        )
    };
    let targets: Vec<&selene_core::Node> = nodes.iter().filter(|n| searchable(n.kind)).collect();
    if targets.is_empty() {
        eprintln!("selene embed: nothing to embed (empty index?)");
        return Outcome::ExpectedNoOp;
    }

    if let Err(e) = store.define_embedding_field().await {
        eprintln!("selene embed: {e:#}");
        return Outcome::Failure;
    }
    eprintln!(
        "embedding {} symbols locally (first run downloads the ~30MB model once) …",
        targets.len()
    );
    let mut embedder = match selene_embed::Embedder::load() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("selene embed: could not load the model: {e:#}");
            return Outcome::Failure;
        }
    };
    let t = std::time::Instant::now();
    const BATCH: usize = 512;
    let mut done = 0usize;
    for chunk in targets.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|n| selene_db::embedding_text(n)).collect();
        let vecs = match embedder.embed_documents(&texts) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("\nselene embed: {e:#}");
                return Outcome::Failure;
            }
        };
        let rows: Vec<(String, Vec<f32>)> =
            chunk.iter().zip(vecs).map(|(n, v)| (n.id.clone(), v)).collect();
        if let Err(e) = store.store_embeddings(&rows).await {
            eprintln!("\nselene embed: {e:#}");
            return Outcome::Failure;
        }
        done += rows.len();
        eprint!("\r  {done}/{} embedded", targets.len());
    }
    eprintln!();
    eprintln!("building the HNSW vector index …");
    if let Err(e) = store.define_embedding_index(selene_embed::EMBED_DIM).await {
        eprintln!("selene embed: index build failed: {e:#}");
        return Outcome::Failure;
    }
    eprintln!(
        "done: {done} symbols embedded + HNSW index built in {:.1}s. Semantic search is on.",
        t.elapsed().as_secs_f64()
    );
    Outcome::Ok
}

/// Without the `semantic-search` feature, `embed` explains how to get it.
#[cfg(not(feature = "semantic-search"))]
pub async fn embed(_path: PathBuf) -> Outcome {
    eprintln!(
        "selene embed: this binary was built without semantic search.\n  \
         Rebuild with: cargo build --release -p selene --features semantic-search"
    );
    Outcome::Failure
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
    let root = match query_root(path) {
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

/// `selene unlock` — remove SeleneCode's own app-level lock marker, NEVER SurrealDB's engine LOCK
/// (deleting the engine lock under a live holder corrupts the store — map OQ3). No lock is a no-op.
pub fn unlock(path: PathBuf) -> Outcome {
    let Ok(root) = path.canonicalize() else {
        return Outcome::ExpectedNoOp;
    };
    let marker = root.join(".selene").join("selene.lock");
    if !marker.exists() {
        eprintln!("no selene lock at {}", marker.display());
        return Outcome::ExpectedNoOp;
    }
    match std::fs::remove_file(&marker) {
        Ok(()) => {
            eprintln!("removed {}", marker.display());
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene unlock: {e}");
            Outcome::Failure
        }
    }
}

/// `selene install` — wire SeleneCode into one or more agents' MCP configs. `--target` accepts
/// `auto` (agents whose config exists), `all`, `none`, or a list of ids; empty defaults to `claude`.
/// The binary path written is `current_exe()`'s ABSOLUTE path (a static binary is not guaranteed on
/// PATH; a bad path fails silently — map Q8). Only an unknown `--target` or bad `--location` exit 1.
pub async fn install(targets: Vec<String>, location: String, print_config: bool) -> Outcome {
    use selene_installer::Ctx;
    let binary = match std::env::current_exe() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("selene install: cannot find my own path: {e}");
            return Outcome::Failure;
        }
    };
    let ctx = match Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("selene install: {e}");
            return Outcome::Failure;
        }
    };
    if print_config {
        println!("{}", selene_installer::print_config(&binary, &ctx));
        return Outcome::Ok;
    }
    let (loc, ids) = match resolve_targets("install", &targets, &location, &ctx) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if ids.is_empty() {
        eprintln!("selene install: no matching agents (try `--target all` or name one).");
        return Outcome::Ok; // an empty selection is a valid, success-shaped no-op
    }
    let results = selene_installer::install(&ids, loc, &binary, &ctx);
    report_targets("install", &results);
    eprintln!("Restart the agent (or reload its MCP servers) to pick up selene.");
    Outcome::Ok
}

/// `selene uninstall` — remove SeleneCode from agents' MCP configs. Empty `--target` defaults to
/// `all` (strip selene everywhere). Success-shaped even when nothing was configured.
pub async fn uninstall(targets: Vec<String>, location: String) -> Outcome {
    use selene_installer::Ctx;
    let ctx = match Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("selene uninstall: {e}");
            return Outcome::Failure;
        }
    };
    let targets = if targets.is_empty() {
        vec!["all".to_string()]
    } else {
        targets
    };
    let (loc, ids) = match resolve_targets("uninstall", &targets, &location, &ctx) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let results = selene_installer::uninstall(&ids, loc, &ctx);
    report_targets("uninstall", &results);
    Outcome::Ok
}

/// Parse `--location` and resolve the `--target` flag to concrete ids. The ONLY two exit-1 cases in
/// the installer surface: an unknown target id and an invalid location.
fn resolve_targets(
    cmd: &str,
    targets: &[String],
    location: &str,
    ctx: &selene_installer::Ctx,
) -> Result<(selene_installer::Location, Vec<String>), Outcome> {
    let loc = selene_installer::Location::parse(location).map_err(|e| {
        eprintln!("selene {cmd}: {e}");
        Outcome::Failure
    })?;
    // Empty → "claude"; a single special word (auto/all/none) passes through; else a CSV of ids.
    let flag = if targets.is_empty() {
        "claude".to_string()
    } else {
        targets.join(",")
    };
    let ids = selene_installer::resolve_target_flag(&flag, ctx, loc).map_err(|e| {
        eprintln!("selene {cmd}: {e}");
        Outcome::Failure
    })?;
    Ok((loc, ids))
}

/// Print one line per target result.
fn report_targets(cmd: &str, results: &[selene_installer::TargetResult]) {
    for r in results {
        let where_ = r
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let note = r
            .note
            .as_deref()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        eprintln!("  {:<11} {} {}{}", r.id, r.action.as_str(), where_, note);
    }
    if results.is_empty() {
        eprintln!("selene {cmd}: no targets selected.");
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
