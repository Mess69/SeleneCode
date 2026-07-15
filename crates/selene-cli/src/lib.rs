//! `selene-cli` — the CLI surface: the clap tree, the exit-code contract, and the one dispatch.
//!
//! `selene`'s `main` is a shim over [`run`]. Every subcommand arm is present and reachable here
//! before its body is written (the anti-inert-seam move); a later task replaces a body, never a
//! wire. Query-class commands reuse `selene_mcp::handlers`, so the CLI and the agent see the same
//! graph identically — only the exit-code mapping differs (see [`exit`]).

pub mod cli;
mod cmd;
pub mod exit;

pub use cli::{Cli, Command};

use std::process::ExitCode;

use exit::Outcome;

/// The one dispatch — 22 arms. Returns the process exit code (see [`exit`] for the contract).
pub async fn run(cli: Cli) -> ExitCode {
    let outcome = match cli.command {
        // -- lifecycle --
        Command::Init { path, force, .. } => cmd::init(path, force).await,
        Command::Uninit { path, force } => cmd::uninit(path, force),
        Command::Index { path, .. } => cmd::index(path).await,
        Command::Sync { .. } => cmd::not_yet("sync", "Phase 6 Task 6/13"),
        Command::Status { path, json } => cmd::status(path, json).await,

        // -- query-class (reuse the MCP handlers) --
        Command::Query { search, path, .. } => cmd::query(search, path).await,
        Command::Explore { query, path, .. } => cmd::explore(query, path).await,
        Command::Node {
            name, path, file, ..
        } => cmd::node(name, path, file).await,
        Command::Callers { symbol, path, .. } => cmd::callers(symbol, path).await,
        Command::Callees { symbol, path, .. } => cmd::callees(symbol, path).await,
        Command::Impact {
            symbol,
            depth,
            path,
            ..
        } => cmd::impact(symbol, depth, path).await,
        Command::Files { path, filter, .. } => cmd::files(path, filter).await,
        Command::Affected { .. } => cmd::not_yet("affected", "Phase 6 Task 10"),

        // -- serve / daemon / hooks --
        Command::Serve { mcp, path, .. } => cmd::serve(mcp, path).await,
        Command::Daemon => cmd::not_yet("daemon", "Phase 6 Task 17"),
        Command::Unlock { .. } => cmd::not_yet("unlock", "Phase 6 Task 4"),
        // prompt-hook NEVER breaks the prompt: exit 0 on every path (map §Subcommands).
        Command::PromptHook => Outcome::ExpectedNoOp,

        // -- installer (Phase 7) --
        Command::Install { .. } => cmd::not_yet("install", "Phase 7"),
        Command::Uninstall { .. } => cmd::not_yet("uninstall", "Phase 7"),

        // -- misc --
        Command::Telemetry { .. } => cmd::not_yet("telemetry", "Phase 6 Task 21"),
        Command::Upgrade { .. } => cmd::not_yet("upgrade", "Phase 8"),
        Command::Version => cmd::version(),
    };
    ExitCode::from(outcome)
}
