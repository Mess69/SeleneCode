//! `selene serve` (the MCP stdio surface) and `selene daemon`.

use std::path::PathBuf;

use anyhow::Result;

use crate::exit::Outcome;

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
