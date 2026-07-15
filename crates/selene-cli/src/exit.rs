//! **The exit-code contract, in one place.** A subcommand does not get to invent a code — it
//! returns an [`Outcome`], and this file maps it. (CodeGraph map §Exit-code semantics.)
//!
//! | Code | Meaning |
//! |---|---|
//! | **0** | success, **or an expected no-op** (`unlock` with no lock; `uninit` on an un-init project; `affected` with no inputs; `daemon` with none running; `init` on an already-init project; `prompt-hook` on every path) |
//! | **1** | genuine failure, **or "not initialized" for a query-class command** (`status`/`query`/`callers`/… on an un-init project; `explore`/`node` when the tool result `isError`, incl. un-indexed → the agent-facing refusal text) |
//! | *(its own)* | `upgrade` returns whatever `run_upgrade` returns |
//!
//! **The one deliberate asymmetry:** an un-indexed project is *success-shaped over MCP* (guidance,
//! `isError: false`) and *exit 1 over the CLI* (`explore`/`node` print the refusal text and exit 1).
//! A shell caller checks `$?`; an agent mid-session must not be told "error". Both are correct; do
//! not unify them.

use std::process::ExitCode;

/// What a subcommand did, before it becomes a process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The command succeeded.
    Ok,
    /// The command had nothing to do, and that is fine (see the table). Exit 0.
    ExpectedNoOp,
    /// A genuine failure, or a query-class command on an un-initialized project. Exit 1.
    Failure,
    /// A subcommand that owns its own code (`upgrade`).
    Code(u8),
}

impl From<Outcome> for ExitCode {
    fn from(o: Outcome) -> Self {
        match o {
            Outcome::Ok | Outcome::ExpectedNoOp => ExitCode::SUCCESS,
            Outcome::Failure => ExitCode::FAILURE,
            Outcome::Code(c) => ExitCode::from(c),
        }
    }
}
