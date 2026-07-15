//! The one clap tree — every subcommand `selene` exposes, and exactly the flags the CodeGraph map
//! pins. Defining every arm here (even the not-yet-built ones) is the anti-inert-seam move: a
//! command is reachable from `main()` before its body is written, so a later task *replaces a body*
//! and never has to remember to wire it up.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "selene",
    version,
    about = "Local-first code intelligence, in Rust."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize SeleneCode in a project directory and build the initial index.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Deprecated: indexing runs by default; the flag is accepted and ignored.
        #[arg(short, long)]
        index: bool,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        verbose: bool,
        /// Do not install the git hooks that re-sync the index after commit/merge/checkout.
        #[arg(long)]
        no_hooks: bool,
    },
    /// Remove SeleneCode from a project (deletes `.selene/`).
    Uninit {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        force: bool,
    },
    /// Rebuild the full index from scratch.
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        quiet: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Sync changes since the last index (incremental re-index of touched files).
    Sync {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Show index status and statistics.
    Status {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        json: bool,
    },
    /// Search for symbols by name.
    Query {
        search: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        #[arg(short, long)]
        kind: Option<String>,
        #[arg(short, long)]
        json: bool,
    },
    /// Explore an area: relevant symbols' source + call paths in one shot.
    Explore {
        query: Vec<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long)]
        max_files: Option<usize>,
    },
    /// One symbol's source + caller/callee trail.
    Node {
        name: Option<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        file: Option<String>,
        #[arg(long)]
        offset: Option<usize>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        symbols_only: bool,
    },
    /// Who calls this symbol.
    Callers {
        symbol: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        json: bool,
    },
    /// What this symbol calls.
    Callees {
        symbol: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        json: bool,
    },
    /// What breaks if this symbol changes.
    Impact {
        symbol: String,
        #[arg(short, long, default_value_t = 2)]
        depth: u32,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        json: bool,
    },
    /// The indexed files, optionally filtered by path.
    Files {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        filter: Option<String>,
        #[arg(short, long)]
        json: bool,
    },
    /// Files whose graph depends on the given files (BFS over dependents).
    Affected {
        files: Vec<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long)]
        stdin: bool,
        #[arg(short, long, default_value_t = 5)]
        depth: u32,
        #[arg(short, long)]
        filter: Option<String>,
        #[arg(short, long)]
        json: bool,
    },
    /// Render the code graph as a self-contained interactive HTML "galaxy".
    Viz {
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Output HTML file. Default: `./selene-graph.html`.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Cap the rendered node count (most-connected first) so the page stays light.
        #[arg(long, default_value_t = 2000)]
        max_nodes: usize,
        /// Keep low-signal kinds (file/import/variable/parameter) that are dropped by default.
        #[arg(long)]
        all_kinds: bool,
        /// Open the written file in the default browser.
        #[arg(long)]
        open: bool,
    },
    /// List / manage running daemons.
    #[command(alias = "daemons")]
    Daemon,
    /// Serve the knowledge graph over MCP (stdio). Hidden — agents wire this, humans do not.
    #[command(hide = true)]
    Serve {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long)]
        mcp: bool,
        #[arg(long)]
        no_watch: bool,
    },
    /// Remove a stale app-level lock (never SurrealDB's own engine lock).
    Unlock {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// The agent prompt-hook. Hidden; never breaks the prompt (always exits 0).
    #[command(hide = true)]
    PromptHook,
    /// Wire SeleneCode into an agent's MCP config (claude, cursor, codex, opencode, hermes, …).
    Install {
        /// Agents to install into: `auto` (detected), `all`, `none`, or ids. Default: claude.
        #[arg(short, long)]
        target: Vec<String>,
        #[arg(short, long, default_value = "local")]
        location: String,
        #[arg(short = 'y', long)]
        yes: bool,
        /// Print the JSON snippet that would be written and touch no file.
        #[arg(long)]
        print_config: bool,
    },
    /// Remove SeleneCode from an agent's MCP config.
    Uninstall {
        /// Agents to remove from: `auto`, `all`, `none`, or ids. Default: all.
        #[arg(short, long)]
        target: Vec<String>,
        #[arg(short, long, default_value = "local")]
        location: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show or set telemetry (status | on | off).
    Telemetry { action: Option<String> },
    /// Upgrade the selene binary.
    Upgrade {
        version: Option<String>,
        #[arg(long)]
        check: bool,
        #[arg(short, long)]
        force: bool,
    },
    /// Print the version.
    Version,
}
