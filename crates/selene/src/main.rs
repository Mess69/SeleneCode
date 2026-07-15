//! `selene` — the single static binary: indexer and MCP server.
//!
//! # The binary exists BEFORE the handlers, on purpose
//!
//! The plan puts `serve --mcp` at Task 14 — before a single tool handler — so that every
//! handler lands into a **live production path**. This project has shipped four seams whose
//! unit tests passed while nothing called them; a tool that is written but never listed, or
//! listed but never dispatched, is the same bug wearing a different hat.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use selene_db::SurrealStore;
use selene_extract::Indexer;
use selene_mcp::SeleneMcp;

#[derive(Parser)]
#[command(
    name = "selene",
    version,
    about = "Local-first code intelligence, in Rust."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a project into `.selene/`.
    Index {
        /// The project root (defaults to the current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Serve the knowledge graph over MCP (stdio).
    Serve {
        /// Speak MCP over stdio. (The only transport in v1.)
        #[arg(long)]
        mcp: bool,
        /// The project root (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Show what's indexed: file / node / edge counts, languages, last-indexed time.
    Status {
        /// The project root (defaults to the current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // The libraries emit `tracing` spans — including `resolve_and_persist_batched`'s
    // per-phase timings — and a span with no subscriber installed goes into the void. That
    // is not hypothetical: it is exactly how a resolution cost that dominated indexing
    // survived three phases unmeasured. Off by default (an MCP server must never write to
    // stdout, and stderr noise is its own bug); opt in with `RUST_LOG=info`.
    //
    // stdio MCP: stdout is the JSON-RPC transport. The subscriber MUST write to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Index { path } => index(path).await,
        Command::Serve { mcp, path } => serve(mcp, path).await,
        Command::Status { path } => status(path).await,
    }
}

/// `selene status` — what the graph holds, read from the store. Not indexed ⇒ guidance + exit 1
/// (the CLI's not-indexed is a refusal; over MCP the same condition is success-shaped guidance —
/// the deliberate asymmetry: an agent must not see `isError`, a shell script wants a non-zero exit).
async fn status(path: PathBuf) -> Result<()> {
    let root = path
        .canonicalize()
        .with_context(|| format!("no such path: {}", path.display()))?;
    let dir = root.join(".selene");
    if !dir.exists() {
        eprintln!(
            "not indexed: {} has no .selene/. Run `selene index {}` first.",
            root.display(),
            root.display()
        );
        std::process::exit(1);
    }

    let store = SurrealStore::open(&dir)
        .await
        .context("could not open the index")?;
    let stats = store.stats().await.context("could not read stats")?;
    let last = store.last_indexed_at().await.ok().flatten();

    println!("{}", root.display());
    println!("  files:  {}", stats.files);
    println!("  nodes:  {}", stats.nodes);
    println!("  edges:  {}", stats.edges);
    if let Some(ms) = last {
        // Seconds since epoch is enough; no chrono dependency for one line.
        println!("  indexed: {} (unix ms)", ms);
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
    // The kinds that carry the flow, most-common first — a quick shape-of-the-graph read.
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

/// `selene index` — extract, then resolve. **Both**: an index without resolution is symbols
/// with no flow, which is the dead-product shape.
async fn index(path: PathBuf) -> Result<()> {
    let root = path
        .canonicalize()
        .with_context(|| format!("no such path: {}", path.display()))?;
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
    // The FULLTEXT build is DEFERRED and runs alongside the resolve below. It takes 3.2 s on
    // django (a `DEFINE INDEX … CONCURRENTLY` that returns at once, then a poll to completion), and
    // nothing in the resolve phase reads it — `search_fts` has exactly one consumer, `explore`.
    let result = indexer.index_all_deferring_fts(None).await;
    let store = indexer.into_store();
    eprintln!(
        "  {} files, {} nodes",
        result.files_indexed, result.nodes_created
    );

    eprintln!("resolving …");
    // The references come from `index_all` in memory. They are NOT round-tripped through the
    // store — see `resolve_and_persist_in_memory`.
    // **Resolve and build the FULLTEXT indexes AT THE SAME TIME.** They do not touch the same
    // work: the resolver reads nodes by name (ordinary indexes) and writes edges; the FULLTEXT
    // build is a background scan of the node table, issued `CONCURRENTLY`. Serialised, its poll was
    // 3.2 s of pure waiting on django, in front of a resolve that needs none of it — `search_fts`
    // has exactly one consumer, and it is `explore`.
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

/// `selene serve --mcp` — stdio, and **the handshake answers before any heavy init**.
///
/// The server is constructed with an `Option<root>` and opens the graph lazily, so
/// `initialize` and `tools/list` succeed at a root that has never been indexed (#964/#172).
async fn serve(mcp: bool, path: Option<PathBuf>) -> Result<()> {
    if !mcp {
        anyhow::bail!("only `--mcp` is supported in v1 (stdio transport)");
    }

    let root = path
        .or_else(|| std::env::current_dir().ok())
        .and_then(|p| p.canonicalize().ok());

    // No store is opened here — see the doc comment.
    let service = SeleneMcp::new(root)
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP handshake failed")?;

    service.waiting().await.context("MCP server stopped")?;
    Ok(())
}
