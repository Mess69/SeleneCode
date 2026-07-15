//! `selene-mcp` — the MCP surface: tool schemas, dispatch, error classification, banners.
//!
//! **It owns nothing else.** Every ranking/flow/budget/render decision lives in
//! `selene-context` as a pure function over the graph API. A tool handler that starts making
//! ranking decisions is the beginning of the same logic existing in two places.
//!
//! # The one rule that outranks the rest
//!
//! **`isError` is RESERVED** — a `PathRefusal` or a genuine malfunction, nothing else. Every
//! recoverable condition is success-shaped guidance. See [`ToolOutcome`]: one `isError` early
//! and an agent abandons the tool for the whole session.

pub mod daemon;
pub mod handlers;
mod instructions;
mod outcome;
#[cfg(feature = "semantic-search")]
mod semantic;
mod server;
mod tools;
mod validate;

pub use instructions::SERVER_INSTRUCTIONS;
pub use outcome::ToolOutcome;
pub use server::SeleneMcp;
pub use tools::{ALL_TOOLS, DEFAULT_VISIBLE, TOOLS_ENV, visible_tools};
