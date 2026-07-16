//! `selene` — the single static binary. `main` is a shim over `selene_cli::run`.
//!
//! # The binary exists BEFORE the handlers, on purpose
//!
//! `serve --mcp` and `index` are driven by the Phase 5 dogfood gate; every CLI subcommand lands
//! into a live production path. The whole surface — the clap tree, the exit-code contract, the
//! dispatch — lives in `selene-cli`; this file only wires it to `main`.

use clap::Parser;
use selene_cli::Cli;

// TEMP-DHAT: heap profiling at t-gmax (`--features dhat-heap`).
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();
    // The libraries emit `tracing` spans (index/resolve/explore timings). Off by default — an MCP
    // server must never write to stdout, and stderr noise is its own bug; opt in with `RUST_LOG`.
    // stdio MCP: stdout is the JSON-RPC transport, so the subscriber writes to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    selene_cli::run(Cli::parse()).await
}
