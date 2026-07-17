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

/// [`query_root`] for commands that open the store DIRECTLY (viz and the
/// query class). A running daemon holds RocksDB's exclusive lock, so a direct
/// open dies with a cryptic "lock file … Resource temporarily unavailable" —
/// refuse up front with the actual situation and the two real options instead.
/// (`sync` stays on [`query_root`]: it routes THROUGH the daemon by design.)
fn query_root_direct(path: Option<PathBuf>) -> Result<PathBuf, Outcome> {
    let root = query_root(path)?;
    if let Some(pid) = selene_mcp::daemon::running_pid(&root) {
        eprintln!(
            "a SeleneCode daemon is serving this project (pid {pid}) — the index is locked \
             while your agent is connected.\n  \
             • ask the question through the agent (it uses the same graph), or\n  \
             • run `kill {pid}` and retry — the agent restarts the daemon on its next question."
        );
        return Err(Outcome::Failure);
    }
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
        store
            .bulk_load_finish()
            .await
            .context("the FULLTEXT index build failed")?;
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
    store
        .flush_bulk()
        .await
        .context("flushing the extraction to LadybugDB failed")?;

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
    // a hook failure never fails `init`.
    if !no_hooks && matches!(outcome, Outcome::Ok) {
        install_hooks(&root);
    }
    outcome
}

/// Install the git sync hooks with selene's absolute path, and keep `.selene/`
/// out of git. Never fails the caller.
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
            exclude_selene_dir(root);
        }
        // Not a git repo: say so — the "index maintains itself" promise rides on
        // the hooks, and a silent skip leaves the user with a quietly stale graph.
        Ok(_) => eprintln!(
            "note: not a git repository — sync hooks skipped. Run `selene sync` after \
             changing code to keep the index fresh."
        ),
        Err(e) => eprintln!("selene init: could not install git hooks: {e}"),
    }
}

/// Append `.selene/` to `.git/info/exclude` (idempotent). The index is a raw
/// RocksDB store; without this, a beginner's next `git add -A` commits a binary
/// database that then churns on every post-commit sync. `info/exclude` is the
/// per-clone ignore file — it protects the repo without editing the user's
/// tracked `.gitignore`.
fn exclude_selene_dir(root: &Path) {
    let exclude = root.join(".git").join("info").join("exclude");
    let Some(dir) = exclude.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if current.lines().any(|l| l.trim() == ".selene/") {
        return;
    }
    let mut updated = current;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("# SeleneCode index (added by `selene init`)\n.selene/\n");
    if std::fs::write(&exclude, updated).is_ok() {
        eprintln!("added .selene/ to .git/info/exclude (the index stays out of git)");
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
    let root = match query_root_direct(Some(path)) {
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
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    render(handlers::explore(Some(root), &query.join(" ")).await)
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
    watch: bool,
) -> Outcome {
    if watch {
        // Watch mode WANTS a daemon (it reads through it, no lock fight) — so no
        // `query_root_direct` refusal here.
        let root = match query_root(path) {
            Ok(r) => r,
            Err(o) => return o,
        };
        return viz_watch(root, max_nodes, all_kinds, open).await;
    }
    let root = match query_root_direct(path) {
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
        watch: false,
    };
    let doc = crate::viz::build_html(&nodes, &edges, &opts);

    let dest = out.unwrap_or_else(|| root.join("selene-graph.html"));
    std::fs::write(&dest, &doc.html)
        .with_context(|| format!("could not write {}", dest.display()))?;
    Ok((dest, doc))
}

// ---- viz --watch: the live map --------------------------------------------------------------

/// Read the whole graph, through whichever door is open: a running daemon (it
/// owns the RocksDB lock — ask it over the control socket) or, with no daemon,
/// a brief direct open that is dropped immediately so a daemon can still start.
async fn fetch_graph(
    root: &Path,
) -> Result<(Vec<selene_core::Node>, Vec<selene_core::Edge>, bool)> {
    if let Ok(Some(reply)) = selene_mcp::daemon::route_to_daemon(root, "graph").await {
        if !reply.ok {
            // A live daemon that errored: do NOT fall through to a direct open —
            // that is a guaranteed lock fight. Surface and let the caller retry.
            anyhow::bail!(
                "daemon graph dump failed: {}",
                reply.error.unwrap_or_else(|| "unknown error".into())
            );
        }
        let mut data = reply.data.context("daemon graph reply carried no data")?;
        let nodes: Vec<selene_core::Node> =
            serde_json::from_value(data.get_mut("nodes").map(serde_json::Value::take).context(
                "daemon graph reply missing nodes",
            )?)
            .context("daemon graph nodes unparseable")?;
        let edges: Vec<selene_core::Edge> =
            serde_json::from_value(data.get_mut("edges").map(serde_json::Value::take).context(
                "daemon graph reply missing edges",
            )?)
            .context("daemon graph edges unparseable")?;
        return Ok((nodes, edges, true));
    }
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("could not open the index")?;
    let nodes = store.all_nodes().await.context("read nodes")?;
    let edges = store.all_edges().await.context("read edges")?;
    drop(store); // release the lock NOW — an agent's daemon may want to start
    Ok((nodes, edges, false))
}

/// A cheap freshness fingerprint of the index: (name, len, mtime) of the
/// RocksDB data files under `.selene/` (WAL/SSTs are `<digits>.log`/`.sst`,
/// plus MANIFEST/CURRENT). Info logs (`LOG*`, `daemon.log`) are excluded — they
/// churn without the graph changing.
fn index_fingerprint(selene_dir: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    let Ok(entries) = std::fs::read_dir(selene_dir) else {
        return 0;
    };
    let mut rows: Vec<(String, u64, u128)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_data = name.starts_with(|c: char| c.is_ascii_digit())
            || name.starts_with("MANIFEST")
            || name == "CURRENT";
        if !is_data {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        rows.push((name, md.len(), mtime));
    }
    rows.sort();
    rows.hash(&mut h);
    h.finish()
}

/// Total bytes on disk under `dir`, recursively. Cheap for `.selene/` (a flat
/// RocksDB dir plus a handful of daemon files).
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(md) if md.is_dir() => dir_size(&e.path()),
            Ok(md) => md.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Current RSS of a process in bytes, via `ps` (KiB on both macOS and Linux).
fn rss_of(pid: i32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kib| kib * 1024)
}

/// The live memory line the watch page shows: the index's on-disk footprint,
/// and the RAM of whoever is actually holding the graph — the daemon when one
/// is up (the number that matters while an agent works), else this server.
fn memory_probe(root: &Path) -> serde_json::Value {
    let index_bytes = dir_size(&root.join(".selene"));
    if let Some(rss) = selene_mcp::daemon::running_pid(root).and_then(rss_of) {
        return serde_json::json!({ "index": index_bytes, "rss": rss, "src": "daemon" });
    }
    let own = rss_of(std::process::id() as i32).unwrap_or(0);
    serde_json::json!({ "index": index_bytes, "rss": own, "src": "server" })
}

/// `selene viz --watch` — serve the live map over local HTTP. The page polls
/// `/data`; every index change (agent edits, syncs, commits) re-reads the graph
/// and bumps the generation, and the page animates the difference in place.
async fn viz_watch(root: PathBuf, max_nodes: usize, all_kinds: bool, open: bool) -> Outcome {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    let opts = crate::viz::VizOptions {
        max_nodes,
        all_kinds,
        root_label: root.display().to_string(),
        watch: true,
    };

    let (nodes, edges, via_daemon) = match fetch_graph(&root).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("selene viz --watch: {e:#}");
            return Outcome::Failure;
        }
    };
    let mut data = crate::viz::build_data(&nodes, &edges, &opts);
    // The compare/serve line stays mem-free (memory is spliced in fresh at
    // serve time — inside `last_content` it would fake graph changes); the
    // initial page embed gets it so the first paint shows memory immediately.
    let data_line = serde_json::to_string(&data.json).unwrap_or_default();
    data.json["mem"] = memory_probe(&root);
    let html = crate::viz::render(&data.json, &opts.root_label);

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("selene viz --watch: could not bind a local port: {e}");
            return Outcome::Failure;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}");

    let generation = Arc::new(AtomicU64::new(0));
    let latest = Arc::new(tokio::sync::RwLock::new(data_line));
    let html = Arc::new(html);

    eprintln!(
        "selene viz --watch: {} nodes, {} edges · source: {}",
        data.shown_nodes,
        data.shown_edges,
        if via_daemon {
            "daemon (live index)"
        } else {
            "direct reads (no daemon)"
        }
    );
    eprintln!("selene viz --watch: serving {url}  — Ctrl-C to stop");
    println!("{url}");
    if open {
        open_url_in_browser(&url);
    }

    // The HTTP loop: tiny, GET-only, connection-per-request.
    {
        let generation = generation.clone();
        let latest = latest.clone();
        let html = html.clone();
        let probe_root = root.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let (generation, latest, html) = (generation.clone(), latest.clone(), html.clone());
                let probe_root = probe_root.clone();
                tokio::spawn(async move {
                    let _ = serve_watch_conn(stream, generation, latest, html, probe_root).await;
                });
            }
        });
    }

    // The refresh loop: fingerprint the index every 1.2s; on change, wait for
    // one quiet tick (writes settled), re-read, re-transform, bump the gen.
    let selene_dir = root.join(".selene");
    let mut last_fp = index_fingerprint(&selene_dir);
    let mut dirty = false;
    // The gen-0 serialization of what we're serving — the content-change probe.
    // (`latest` itself carries a bumped `gen`, so it can't be the comparand.)
    let mut last_content = latest.read().await.clone();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let fp = index_fingerprint(&selene_dir);
        if fp != last_fp {
            last_fp = fp;
            dirty = true;
            continue; // still moving — let the writes settle one tick
        }
        if !dirty {
            continue;
        }
        match fetch_graph(&root).await {
            Ok((nodes, edges, _)) => {
                // Our own direct open just churned the RocksDB files (fresh WAL,
                // MANIFEST bump) — rebase the fingerprint on the post-read state
                // or we would chase our own tail forever.
                last_fp = index_fingerprint(&selene_dir);
                dirty = false;

                let mut data = crate::viz::build_data(&nodes, &edges, &opts);
                let line_probe = serde_json::to_string(&data.json).unwrap_or_default();
                // Only bump the generation when the GRAPH changed — file churn
                // with identical content (compactions, no-op syncs) stays silent.
                if line_probe == last_content {
                    continue;
                }
                last_content = line_probe;
                let g = generation.fetch_add(1, Ordering::SeqCst) + 1;
                data.json["gen"] = serde_json::json!(g);
                *latest.write().await = serde_json::to_string(&data.json).unwrap_or_default();
                eprintln!(
                    "selene viz --watch: ↻ update {g}: {} nodes, {} edges",
                    data.shown_nodes, data.shown_edges
                );
            }
            Err(e) => {
                // Index mid-write or daemon busy — keep serving the old data, retry.
                eprintln!("selene viz --watch: refresh failed ({e:#}); retrying");
            }
        }
    }
}

/// Answer one HTTP connection: `/` → the page, `/data?known=N` → the latest
/// data (or a tiny `{"gen":N}` when the client is already current).
async fn serve_watch_conn(
    mut stream: tokio::net::TcpStream,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    latest: std::sync::Arc<tokio::sync::RwLock<String>>,
    html: std::sync::Arc<String>,
    probe_root: PathBuf,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (rh, mut wh) = stream.split();
    let mut reader = BufReader::new(rh);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    // Drain the headers so the client's socket isn't reset mid-request.
    let mut hdr = String::new();
    while reader.read_line(&mut hdr).await? > 2 {
        hdr.clear();
    }

    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let (status, ctype, body): (&str, &str, String) = if path == "/" {
        ("200 OK", "text/html; charset=utf-8", html.as_ref().clone())
    } else if let Some(q) = path.strip_prefix("/data") {
        let cur = generation.load(std::sync::atomic::Ordering::SeqCst);
        let known: Option<u64> = q
            .strip_prefix("?known=")
            .and_then(|v| v.parse().ok());
        // Memory is spliced in FRESH on every poll — the stored data line only
        // changes when the graph does, but RAM/disk move continuously.
        let mem = memory_probe(&probe_root);
        if known == Some(cur) {
            ("200 OK", "application/json", format!(r#"{{"gen":{cur},"mem":{mem}}}"#))
        } else {
            let stored = latest.read().await.clone();
            // stored is a JSON object ("{...}") — prepend mem inside it.
            ("200 OK", "application/json", format!("{{\"mem\":{mem},{}", &stored[1..]))
        }
    } else {
        ("404 Not Found", "text/plain", "not found".to_string())
    };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    wh.write_all(head.as_bytes()).await?;
    wh.write_all(body.as_bytes()).await?;
    wh.flush().await?;
    Ok(())
}

/// Best-effort: open a URL in the OS default browser (same contract as
/// [`open_in_browser`], different argument shape).
fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let program = "xdg-open";

    if let Err(e) = std::process::Command::new(program).arg(url).spawn() {
        eprintln!("selene viz --watch: could not open a browser ({e}); open {url} yourself");
    }
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
    let root = match query_root_direct(Some(path)) {
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
        let rows: Vec<(String, Vec<f32>)> = chunk
            .iter()
            .zip(vecs)
            .map(|(n, v)| (n.id.clone(), v))
            .collect();
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

    // `install` IS the one-command onboarding: a project that has no index yet
    // gets `init` (index + git sync hooks) before the MCP config is written.
    // Without this, the first agent question hits "not indexed" guidance and
    // the user has to come back for a second command nobody told them about.
    if !Path::new(".selene").exists() {
        eprintln!("no index here yet — running `selene init` first…");
        if let Outcome::Failure = init(PathBuf::from("."), false, false).await {
            eprintln!("selene install: init failed — MCP config not written.");
            return Outcome::Failure;
        }
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

/// `selene upgrade [version] [--check] [--force]` — self-update from GitHub
/// Releases (axoupdater, the `uv self update` engine).
///
/// Two install identities, detected structurally:
/// - **Installer/receipt install** (`curl … selene-installer.sh | sh`): the dist
///   receipt says where the binary lives and which release channel it came
///   from — upgrade replaces it in place, checksum-verified.
/// - **No receipt** (a source build, `cargo build`): upgrading in place would
///   overwrite a build product the user compiled themselves — refuse with the
///   exact commands instead. `--check` still works via the repo release feed.
pub async fn upgrade(version: Option<String>, check: bool, force: bool) -> Outcome {
    use axoupdater::{AxoUpdater, ReleaseSource, ReleaseSourceType, UpdateRequest, Version};

    let current = env!("CARGO_PKG_VERSION");
    let mut updater = AxoUpdater::new_for("selene");

    // The receipt is the source of truth for WHERE to upgrade. Without one,
    // fall back to the repo's release feed — enough for `--check`, and enough
    // to name the installer for everything else.
    let has_receipt = updater.load_receipt().is_ok();
    if !has_receipt {
        // `repository` from Cargo.toml, overridable for forks/mirrors.
        let repo = std::env::var("SELENE_GITHUB_REPO")
            .unwrap_or_else(|_| env!("CARGO_PKG_REPOSITORY").to_string());
        let (owner, name) = match repo
            .trim_start_matches("https://github.com/")
            .split_once('/')
        {
            Some((o, n)) => (o.to_string(), n.to_string()),
            None => {
                eprintln!("selene upgrade: cannot parse repository from `{repo}`");
                return Outcome::Failure;
            }
        };
        updater.set_release_source(ReleaseSource {
            release_type: ReleaseSourceType::GitHub,
            owner,
            name,
            app_name: "selene".to_string(),
        });
    }

    if let Some(v) = &version {
        let tag = if v.starts_with('v') {
            v.clone()
        } else {
            format!("v{v}")
        };
        updater.configure_version_specifier(UpdateRequest::SpecificTag(tag));
    }

    if check {
        // `query_new_version` needs only the release source — it works for
        // receipt installs AND source builds (is_update_needed does not: it
        // insists on a receipt's install_prefix).
        return match updater.query_new_version().await {
            Ok(Some(latest)) => {
                let newer = Version::parse(current)
                    .map(|cur| *latest > cur)
                    .unwrap_or(true);
                if newer {
                    println!("selene {current} → {latest} is available. Run `selene upgrade`.");
                } else {
                    println!("selene {current} is up to date (latest release: {latest}).");
                }
                Outcome::Ok
            }
            Ok(None) => {
                println!("selene {current}: no published release found.");
                Outcome::Ok
            }
            Err(e) => {
                eprintln!(
                    "selene upgrade --check: could not reach the release feed: {e}\n\
                     (no release published yet, offline, or the repository in Cargo.toml \
                     is not live — override with SELENE_GITHUB_REPO=owner/name)"
                );
                Outcome::Failure
            }
        };
    }

    if !has_receipt {
        eprintln!(
            "selene upgrade: this binary was built from source (no install receipt), so \
             upgrading in place would overwrite your own build.\n\
             - source build:  git pull && cargo build --release -p selene\n\
             - or switch to the managed install:  curl -fsSL \
             {}/releases/latest/download/selene-installer.sh | sh",
            env!("CARGO_PKG_REPOSITORY")
        );
        return Outcome::ExpectedNoOp;
    }

    if force {
        updater.always_update(true);
    }
    match updater.run().await {
        Ok(Some(result)) => {
            println!(
                "upgraded: selene {current} → {} (restart running agents to pick it up)",
                result.new_version
            );
            Outcome::Ok
        }
        Ok(None) => {
            println!("selene {current} is up to date.");
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene upgrade: {e}");
            Outcome::Failure
        }
    }
}

/// A body that is not built yet. Prints a clear line to stderr and returns `Failure` — the
/// anti-inert-seam shape: the arm is wired and reachable, its test fails until a task fills it.
pub fn not_yet(name: &str, task: &str) -> Outcome {
    eprintln!("selene {name}: not yet implemented ({task})");
    Outcome::Failure
}
