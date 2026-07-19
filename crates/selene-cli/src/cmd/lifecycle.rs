//! The lifecycle subcommands: index/init/uninit/purge/status/sync/embed/unlock.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use selene_db::SurrealStore;
use selene_extract::Indexer;

use crate::exit::Outcome;

use super::install::{report_targets, resolve_targets};
use super::{query_root, query_root_direct, resolve};

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

/// `selene purge` — the one-shot full removal: everything SeleneCode ever
/// added to a project, gone. Source files are never touched (extraction is
/// read-only by design; the only artifacts are the ones removed here).
///
/// Order matters: the daemon dies first (it holds the RocksDB lock inside
/// `.selene/`), then the files, then the MCP config entries. Every step is
/// best-effort and reported — a missing piece is a no-op, not an error.
pub async fn purge(path: PathBuf, global_mcp: bool) -> Outcome {
    let Ok(root) = path.canonicalize() else {
        eprintln!("nothing to purge: no such path {}", path.display());
        return Outcome::ExpectedNoOp;
    };

    // 1 — stop the daemon (it owns .selene/'s RocksDB lock). TERM first; a
    // daemon with a live agent session shuts down gracefully only when that
    // session closes, and purge must not wait on an agent — escalate to KILL.
    if let Some(pid) = selene_mcp::daemon::running_pid(&root) {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
        let mut waited = 0u32;
        while selene_mcp::daemon::proc::is_alive(pid) && waited < 30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            waited += 1;
        }
        if selene_mcp::daemon::proc::is_alive(pid) {
            // SIGKILL is unconditional; afterwards the pid can only linger as a
            // zombie (unreaped), and a zombie holds no locks — treat as stopped.
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
            std::thread::sleep(std::time::Duration::from_millis(300));
            eprintln!("purge: stopped the daemon (pid {pid}, forced)");
        } else {
            eprintln!("purge: stopped the daemon (pid {pid})");
        }
    }

    // 2 — strip our block from the git hooks (surgical: user hook content stays).
    match selene_sync::hooks::remove(&root) {
        Ok(results) => {
            let stripped = results.iter().filter(|r| r.action != "not-found").count();
            if stripped > 0 {
                eprintln!("purge: removed the selene block from {stripped} git hook(s)");
            }
        }
        Err(e) => eprintln!("purge: could not remove git hooks: {e}"),
    }

    // 3 — the index itself.
    let dir = root.join(".selene");
    if dir.exists() {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => eprintln!("purge: removed {}", dir.display()),
            Err(e) => {
                eprintln!("purge: could not remove {}: {e}", dir.display());
                return Outcome::Failure;
            }
        }
    }

    // 4 — the `.git/info/exclude` lines `selene init` added.
    let exclude = root.join(".git").join("info").join("exclude");
    if let Ok(current) = std::fs::read_to_string(&exclude) {
        let cleaned: String = current
            .lines()
            .filter(|l| {
                let t = l.trim();
                t != ".selene/" && !t.contains("SeleneCode index (added by `selene init`)")
            })
            .map(|l| format!("{l}\n"))
            .collect();
        if cleaned != current && std::fs::write(&exclude, cleaned).is_ok() {
            eprintln!("purge: cleaned .selene/ out of .git/info/exclude");
        }
    }

    // 5 — the viz page, if one was written at the default location.
    let viz_page = root.join("selene-graph.html");
    if viz_page.exists() && std::fs::remove_file(&viz_page).is_ok() {
        eprintln!("purge: removed {}", viz_page.display());
    }

    // 6 — the MCP config entries. Local always (they live in the project);
    // global only on request — other projects may still use SeleneCode.
    match selene_installer::Ctx::from_env() {
        Ok(mut ctx) => {
            ctx.cwd = root.clone(); // `-p` must purge THAT project's configs, not the cwd's
            let all = ["all".to_string()];
            let (_, ids) = match resolve_targets("purge", &all, "local", &ctx) {
                Ok(v) => v,
                Err(o) => return o,
            };
            let results =
                selene_installer::uninstall(&ids, selene_installer::Location::Local, &ctx);
            report_targets("purge (local mcp)", &results);
            if global_mcp {
                let results =
                    selene_installer::uninstall(&ids, selene_installer::Location::Global, &ctx);
                report_targets("purge (global mcp)", &results);
            } else {
                eprintln!(
                    "purge: global agent configs left alone (other projects may use selene) — \
                     add --global-mcp to strip those too"
                );
            }
        }
        Err(e) => eprintln!("purge: skipped MCP configs: {e}"),
    }

    // 7 — the empty shells uninstall leaves behind when selene was the only
    // entry: a bare {"mcpServers": {}} (or {"mcp": {}} for opencode). If the
    // user has OTHER servers in a file, it keeps them and stays. Emptied
    // selene-created dirs (.cursor/rules, .kiro/settings, …) go too.
    let shells: [(&str, &str); 6] = [
        (".mcp.json", "mcpServers"),
        (".cursor/mcp.json", "mcpServers"),
        (".gemini/settings.json", "mcpServers"),
        (".kiro/settings/mcp.json", "mcpServers"),
        ("opencode.json", "mcp"),
        ("opencode.jsonc", "mcp"),
    ];
    for (rel, container) in shells {
        let path = root.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let is_empty_shell = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                let obj = v.as_object()?;
                Some(
                    obj.keys().all(|k| k == container)
                        && obj
                            .get(container)
                            .and_then(|s| s.as_object())
                            .is_none_or(|s| s.is_empty()),
                )
            })
            .unwrap_or(false);
        if is_empty_shell && std::fs::remove_file(&path).is_ok() {
            eprintln!("purge: removed the now-empty {rel}");
        }
    }
    // remove_dir only succeeds on EMPTY dirs — anything of the user's stays.
    for rel in [
        ".cursor/rules",
        ".cursor",
        ".gemini",
        ".kiro/settings",
        ".kiro/steering",
        ".kiro",
    ] {
        let _ = std::fs::remove_dir(root.join(rel));
    }

    eprintln!("purge: done — your source files were never touched");
    Outcome::Ok
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
