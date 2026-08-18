//! The tool handlers — **every one of them classifies inside itself**.
//!
//! A `?` on a store error escaping a handler becomes a JSON-RPC transport failure, not a
//! failed call (the spike's finding #1). So no handler propagates: each opens the store,
//! answers, and returns a [`ToolOutcome`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use selene_context::{
    ContextBuilder, NOT_INDEXED, build_node_view, node_not_found, render_node_view,
};
use selene_db::SurrealStore;
use selene_graph::{GraphError, QueryManager};
use tokio::sync::OnceCell;

use crate::outcome::ToolOutcome;

/// Process-global warm-store cache, keyed by the `.selene` directory.
///
/// Opening the embedded RocksDB store is not free — and until now **every** MCP handler opened it
/// fresh and dropped it, so a single agent session of five tool calls paid five opens. Here the
/// store is opened **once per root** and every later call reuses the same Arc-backed handle. In the
/// daemon (the sole store owner) this is the whole warm-store win; in a one-shot CLI query it is a
/// single map insert the process discards on exit. `SurrealStore` is `Clone` (it wraps only the
/// cheap `Surreal` handle), so handing a clone to each [`QueryManager`] costs nothing.
///
/// Per-root single-init is a [`tokio::sync::OnceCell`] behind a brief `std` lock: we take the lock
/// only to fetch-or-create the cell (no `.await` under it), then initialise the cell outside it, so
/// two concurrent first-callers for the same root open the store exactly once and callers for other
/// roots never block on it.
type WarmCell = Arc<OnceCell<SurrealStore>>;
static WARM: OnceLock<Mutex<HashMap<PathBuf, WarmCell>>> = OnceLock::new();

async fn warm_store(db: &Path) -> Result<SurrealStore, ToolOutcome> {
    let cell = {
        let mut map = WARM
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.entry(db.to_path_buf())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };
    let store = cell
        .get_or_try_init(|| SurrealStore::open(db))
        .await
        .map_err(|e| ToolOutcome::failed(format!("could not open the index: {e}")))?;
    Ok(store.clone())
}

/// Populate the warm-store cache for `root` ahead of the first query. The daemon calls this right
/// after it binds, so the first client's first tool call is already fast. A root with no `.selene/`
/// is a silent no-op — prewarming is best-effort.
pub async fn prewarm(root: &Path) {
    let db = root.join(".selene");
    if db.exists() {
        let _ = warm_store(&db).await;
    }
}

/// The daemon's warm store for `root`, opening (and caching) it if needed. This is the **one** open
/// handle the daemon holds; daemon-routed writes (sync) run through it rather than opening a second
/// one, which the exclusive RocksDB lock would refuse. Errors as a string (the daemon logs it).
pub async fn warm_store_for_root(root: &Path) -> Result<SurrealStore, String> {
    let db = root.join(".selene");
    if !db.exists() {
        return Err(format!("not indexed: {} has no .selene/", root.display()));
    }
    warm_store(&db).await.map_err(|o| o.text().to_string())
}

/// Open the graph for a project root.
///
/// **Every failure here is guidance, not an error.** No `.selene/`, an unreadable database, a
/// root that does not exist — all of them mean "there is nothing to explore yet", which is a
/// thing an agent can act on. An `isError` would end the session.
async fn open(root: &Path) -> Result<QueryManager<SurrealStore>, ToolOutcome> {
    let db = root.join(".selene");
    if !db.exists() {
        return Err(ToolOutcome::guidance(NOT_INDEXED));
    }
    let store = warm_store(&db).await?;
    Ok(QueryManager::new(store, root.to_path_buf()))
}

/// Resolve the project root for a call: the tool's `projectPath`, else the server's default.
pub fn resolve_root(default: Option<&PathBuf>, arg: Option<&str>) -> Option<PathBuf> {
    arg.map(PathBuf::from).or_else(|| default.cloned())
}

/// The guidance for a call with no project at all.
fn no_project() -> ToolOutcome {
    ToolOutcome::guidance(
        "## No project\n\nThis server has no default project, and no `projectPath` was given.\n\n\
         Pass `projectPath` pointing at a directory that has been indexed with `selene index`.\n",
    )
}

/// **`explore` — the PRIMARY tool.** One call: the flow, the source, the blast radius.
pub async fn explore(root: Option<PathBuf>, query: &str) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };

    // Under `semantic-search`, embed the query (warm model) so explore's seed picking fuses semantic
    // candidates with lexical ones — bridging the vocabulary gap on the PRIMARY tool. A lexical-only
    // index ignores the vector, so this is always safe.
    #[cfg(feature = "semantic-search")]
    let builder = match crate::semantic::embed_query_for(&qm, query).await {
        Some(qvec) => ContextBuilder::new(qm).with_query_vec(qvec),
        None => ContextBuilder::new(qm),
    };
    #[cfg(not(feature = "semantic-search"))]
    let builder = ContextBuilder::new(qm);

    match builder.build_context(query).await {
        Ok(text) => {
            // Journal the exploration (F5) — best-effort, never fails the answer.
            crate::memory::remember(&root, query, &text);
            ToolOutcome::guidance(text)
        }
        // The ONLY things that reach here are a store malfunction and a #527 path refusal —
        // `selene-context` returns every recoverable condition as an Ok value, on purpose.
        Err(e) => ToolOutcome::from_error(&e),
    }
}

/// **`node` — Read parity + symbol mode.** A missing symbol is guidance, never an error (#173).
pub async fn node(root: Option<PathBuf>, symbol: &str) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };

    match build_node_view(&qm, symbol).await {
        Ok(Some(view)) => ToolOutcome::guidance(render_node_view(&view)),
        // Not found is an ANSWER, and it says what to do next.
        Ok(None) => ToolOutcome::guidance(node_not_found(symbol)),
        Err(e) => ToolOutcome::from_error(&e),
    }
}

/// **`search`** — symbols by name.
pub async fn search(root: Option<PathBuf>, query: &str) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };

    // When the index has embeddings, hybrid (lexical + semantic) search bridges the vocabulary gap —
    // a query for `keypress` reaches a `keybinding` symbol it shares no token with. Falls back to
    // lexical when there are no embeddings or the query can't be embedded.
    #[cfg(feature = "semantic-search")]
    if let Some(hits) = crate::semantic::hybrid_nodes(&qm, query, 50).await {
        if hits.is_empty() {
            return ToolOutcome::guidance(format!("## No symbol matches `{query}`\n"));
        }
        let mut out = format!("## Symbols matching `{query}` (hybrid: semantic + lexical)\n\n");
        for n in hits.iter().take(50) {
            out.push_str(&format!(
                "- `{}` — {} ({}:{})\n",
                n.name,
                n.kind.as_str(),
                n.file_path,
                n.start_line
            ));
        }
        return ToolOutcome::guidance(out);
    }

    let hits = match qm.find_all_symbols(query).await {
        Ok(h) => h,
        Err(e) => return graph_outcome(&e),
    };
    if hits.is_empty() {
        return ToolOutcome::guidance(format!(
            "## No symbol matches `{query}`\n\nTry the `explore` tool with a description — it \
             searches by relevance and finds symbols whose exact name you do not know.\n"
        ));
    }

    let mut out = format!("## Symbols matching `{query}`\n\n");
    for n in hits.iter().take(50) {
        out.push_str(&format!(
            "- `{}` — {} ({}:{})\n",
            n.name,
            n.kind.as_str(),
            n.file_path,
            n.start_line
        ));
    }
    ToolOutcome::guidance(out)
}

/// **`callers` / `callees`** — grouped by definition site (#764).
///
/// ⚠ The Tasks 1–4 review flagged `group_by_definition` as the likeliest fifth inert seam: it
/// dies quietly if these render a flat list. **It is used here**, and `callers_are_grouped_by_
/// definition_site` asserts the grouping reaches the output — so the seam is closed rather
/// than merely noted.
pub async fn adjacency(root: Option<PathBuf>, symbol: &str, callers: bool) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };

    let Some(node) = (match qm.find_all_symbols(symbol).await {
        Ok(h) => h.into_iter().next(),
        Err(e) => return graph_outcome(&e),
    }) else {
        return ToolOutcome::guidance(node_not_found(symbol));
    };

    let entries = if callers {
        qm.callers(&node.id, 2).await
    } else {
        qm.callees(&node.id, 2).await
    };
    let entries = match entries {
        Ok(e) => e,
        Err(e) => return graph_outcome(&e),
    };

    let nodes: Vec<selene_core::Node> = entries.into_iter().map(|e| e.node).collect();
    if nodes.is_empty() {
        let what = if callers { "callers" } else { "callees" };
        return ToolOutcome::guidance(format!("## `{symbol}` has no {what}.\n"));
    }

    // GROUPED — one heading per definition site, not a flat list (#764).
    let groups = qm.group_by_definition(nodes).await;
    // "Callers of X" / "X calls" — the old "Called by X" heading read as its
    // own inverse (things X calls) above a list of X's callers.
    let mut out = if callers {
        format!("## Callers of `{symbol}`\n\n")
    } else {
        format!("## `{symbol}` calls\n\n")
    };
    for g in groups {
        out.push_str(&format!("### `{}` ({})\n\n", g.qualified_name, g.file_path));
        for n in &g.nodes {
            out.push_str(&format!("- `{}` (:{})\n", n.name, n.start_line));
        }
        out.push('\n');
    }
    ToolOutcome::guidance(out)
}

/// **`impact`** — what breaks if this changes.
pub async fn impact(root: Option<PathBuf>, symbol: &str, depth: u32) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };

    let Some(node) = (match qm.find_all_symbols(symbol).await {
        Ok(h) => h.into_iter().next(),
        Err(e) => return graph_outcome(&e),
    }) else {
        return ToolOutcome::guidance(node_not_found(symbol));
    };

    let sub = match qm.impact(&node.id, depth).await {
        Ok(s) => s,
        Err(e) => return graph_outcome(&e),
    };

    let mut out = format!(
        "## Blast radius of `{}`\n\n**{}** symbols affected.\n\n",
        node.name,
        sub.nodes.len()
    );
    for n in sub.nodes.values().take(50) {
        out.push_str(&format!(
            "- `{}` ({}:{})\n",
            n.name, n.file_path, n.start_line
        ));
    }
    ToolOutcome::guidance(out)
}

/// **`files`** — the indexed file list.
pub async fn files(root: Option<PathBuf>, filter: Option<&str>) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };

    let files = match qm.files().await {
        Ok(f) => f,
        Err(e) => return graph_outcome(&e),
    };
    let needle = filter.map(selene_graph::normalize_path).unwrap_or_default();

    let mut out = String::from("## Indexed files\n\n");
    let mut shown = 0usize;
    for f in &files {
        if !needle.is_empty() && !f.path.contains(&needle) {
            continue;
        }
        out.push_str(&format!(
            "- `{}` — {} ({} symbols)\n",
            f.path, f.language, f.node_count
        ));
        shown += 1;
        if shown >= 200 {
            break;
        }
    }
    if shown == 0 {
        return ToolOutcome::guidance(format!(
            "## No indexed files match `{}`\n\nRun `selene index` if this project has not been \
             indexed.\n",
            needle
        ));
    }
    ToolOutcome::guidance(out)
}

/// **`insights`** — the structural summary: betweenness bottlenecks, Louvain
/// clusters, import cycles, rare bridges, orphan modules. One deterministic
/// recipe shared with `selene insights`/viz/report ([`selene_graph::analysis`]).
pub async fn insights(root: Option<PathBuf>) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    let qm = match open(&root).await {
        Ok(qm) => qm,
        Err(outcome) => return outcome,
    };
    let (nodes, edges) = match (qm.store().all_nodes().await, qm.store().all_edges().await) {
        (Ok(n), Ok(e)) => (n, e),
        (Err(e), _) | (_, Err(e)) => return ToolOutcome::failed(format!("{e}")),
    };
    if nodes.is_empty() {
        return ToolOutcome::guidance(NOT_INDEXED);
    }
    let ins = selene_graph::analysis::compute_insights(&nodes, &edges);
    let text = crate::insights::render_insights(&ins, &root.display().to_string());
    ToolOutcome::guidance(selene_context::truncate_output(&text))
}

/// **`recall`** — the session-memory journal (F5): past explorations relevant
/// to `query`. Success-shaped on every path, including an empty journal.
pub async fn recall(root: Option<PathBuf>, query: Option<&str>) -> ToolOutcome {
    let Some(root) = root else {
        return no_project();
    };
    ToolOutcome::guidance(crate::memory::render_recall(&root, query))
}

/// A graph error: a #527 refusal or a genuine malfunction — those, and only those, set
/// `isError`.
fn graph_outcome(e: &GraphError) -> ToolOutcome {
    ToolOutcome::failed(format!("{e}"))
}
