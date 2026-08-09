//! The subcommand bodies. Query-class commands reuse `selene_mcp::handlers` — the same code the MCP
//! surface runs, so the CLI and the agent see the same graph the same way; only the exit-code
//! mapping (the CLI's `$?` contract) differs from MCP's `isError`.

mod install;
mod lifecycle;
mod query;
mod report;
mod serve;
mod viz;

pub use install::{install, uninstall, upgrade, version};
pub use lifecycle::{embed, index, init, purge, status, sync, uninit, unlock};
pub use query::{affected, callees, callers, explore, files, impact, node, query};
pub use report::report;
pub use serve::{daemon, serve};
pub use viz::viz;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use selene_mcp::ToolOutcome;

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

/// A body that is not built yet. Prints a clear line to stderr and returns `Failure` — the
/// anti-inert-seam shape: the arm is wired and reachable, its test fails until a task fills it.
pub fn not_yet(name: &str, task: &str) -> Outcome {
    eprintln!("selene {name}: not yet implemented ({task})");
    Outcome::Failure
}
