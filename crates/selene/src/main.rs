//! SeleneCode binary entry point.
//!
//! Scaffold: today it only proves the workspace links end-to-end. The real CLI
//! (clap) + MCP server (rmcp) land in `selene-cli` / `selene-mcp` per the PRD
//! (`docs/specs/2026-07-11-rust-graph-db-migration-design.md`, §3 & §6).

use selene_core::{EdgeKind, NodeKind};

fn main() {
    println!("SeleneCode v{} — scaffold", env!("CARGO_PKG_VERSION"));
    println!(
        "graph model: {} node kinds, {} edge kinds",
        NodeKind::ALL.len(),
        EdgeKind::ALL.len()
    );
    println!("Target architecture: docs/specs/2026-07-11-rust-graph-db-migration-design.md");
}
