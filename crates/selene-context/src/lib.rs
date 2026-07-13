//! `selene-context` — turning a graph into an answer an agent can use **without opening a
//! file**.
//!
//! Target design: PRD §3; build plan: `docs/plans/2026-07-13-phase45-graph-context-mcp.md`;
//! parity source: `docs/reference/from-codegraph/maps/mcp-context.md`.
//!
//! # The one question every line of this crate is judged by
//!
//! **Does the output stop the agent from reading the file?**
//!
//! Not "is it correct" — correct-but-insufficient is a failed product. An answer that is
//! accurate and sends the agent to `Read` has cost more than it saved, because the agent now
//! pays for both.
//!
//! # Nothing here is an error unless it is a malfunction
//!
//! "No relevant context", "the query was all stopwords", "symbol not found" — every one of
//! them is an ordinary answer with a success-shaped value. An `Err` becomes an `isError` at
//! the MCP layer, and one `isError` early makes an agent abandon the tool for the session.
//!
//! # Layering
//!
//! `selene-graph` → **`selene-context`** → `selene-mcp`. No reverse edge, ever: every
//! ranking/flow/budget/render decision lives here as a pure function over the graph API, and
//! `selene-mcp` owns only schemas, dispatch, banners and error classification.

mod budgets;
mod builder;
mod error;
mod flow;
mod relevance;
mod stopwords;

pub use budgets::{
    ExploreBudget, FILE_SECTION_PREFIX, HARD_CEILING, MAX_OUTPUT_LENGTH, TRUNCATION_NOTE,
    budget_for, explore_budget, truncate_output, truncate_to_ceiling,
};
pub use builder::{CALL_PATH_KINDS, ContextBuilder, NOT_INDEXED, thousands};
pub use error::{ContextError, Result};
pub use flow::{FLOW_KINDS, FlowStep, build_flow_from_named_symbols, describe_hop, render_flow};
pub use relevance::{
    Confidence, DominantFile, FindOptions, HIGH_VALUE_NODE_KINDS, RelevantContext, ScoredNode,
    brevity, confidence_of, find_relevant_context, is_test_file, score_candidates, sort_candidates,
    term_groups, weights,
};
pub use stopwords::{STOPWORDS, extract_search_terms, extract_symbols_from_query};
