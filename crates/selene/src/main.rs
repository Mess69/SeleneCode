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
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Index { path } => index(path).await,
        Command::Serve { mcp, path } => serve(mcp, path).await,
    }
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
    let result = indexer.index_all(None).await;
    let store = indexer.into_store();
    eprintln!(
        "  {} files, {} nodes",
        result.files_indexed, result.nodes_created
    );

    eprintln!("resolving …");
    let stats = selene_resolve::resolve_and_persist_batched(&store, &root, None)
        .await
        .context("resolution failed")?;

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
