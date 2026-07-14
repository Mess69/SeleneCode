//! Input caps — the one thing that is `isError` because the *caller* is wrong, not the index.
//!
//! Every other `isError` in this crate is a store malfunction or a path refusal ([`outcome`]).
//! These are the exception the invariant explicitly carves out: a tool argument that is empty, or
//! so large it could only be a mistake or an attack, is rejected **before** it reaches the graph.
//! It is `isError: true` — and unlike a not-indexed or not-found condition, that is correct here,
//! because there is no query for the agent to reformulate: the argument itself is malformed.
//!
//! The caps are the TS build's (`maps/mcp-context.md`): free-form strings (a query, a symbol name)
//! at **10 000** bytes, path-like arguments at **4 096**. A 10 000-byte symbol name is not a
//! symbol; a 10 MB "query" is not a question. Rejecting them keeps a single bad call from driving
//! the whole relevance pipeline over megabytes of input.
//!
//! [`outcome`]: crate::outcome

use crate::outcome::ToolOutcome;

/// Max length of a free-form string argument (a `query`, a `symbol`).
pub const MAX_INPUT_LENGTH: usize = 10_000;
/// Max length of a path-like argument (`projectPath`, a file `path`).
pub const MAX_PATH_LENGTH: usize = 4_096;

/// A required free-form argument: **non-empty** and within [`MAX_INPUT_LENGTH`].
///
/// Empty is its own message because it is the common mistake (a client that sent `""` for a field
/// it meant to omit), and the agent's fix is different from "your query was too long".
pub fn free_form(name: &str, value: &str) -> Result<(), ToolOutcome> {
    if value.is_empty() {
        return Err(ToolOutcome::failed(format!(
            "Error: {name} must be a non-empty string"
        )));
    }
    over(name, value, MAX_INPUT_LENGTH)
}

/// An optional path-like argument: if present, within [`MAX_PATH_LENGTH`]. Absent is fine.
pub fn path_like(name: &str, value: Option<&str>) -> Result<(), ToolOutcome> {
    match value {
        Some(v) => over(name, v, MAX_PATH_LENGTH),
        None => Ok(()),
    }
}

fn over(name: &str, value: &str, max: usize) -> Result<(), ToolOutcome> {
    if value.len() > max {
        return Err(ToolOutcome::failed(format!(
            "Error: {name} is {} bytes; the maximum is {max}.",
            value.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_free_form_is_rejected_with_the_non_empty_message() {
        let e = free_form("query", "").unwrap_err();
        assert_eq!(
            e,
            ToolOutcome::failed("Error: query must be a non-empty string")
        );
    }

    #[test]
    fn the_boundary_is_exact() {
        // 10 000 accepted, 10 001 rejected — the cap is inclusive.
        assert!(free_form("query", &"x".repeat(MAX_INPUT_LENGTH)).is_ok());
        assert!(free_form("query", &"x".repeat(MAX_INPUT_LENGTH + 1)).is_err());
        // path cap is the smaller one, and empty-optional is allowed (absent, not "").
        assert!(path_like("projectPath", None).is_ok());
        assert!(path_like("projectPath", Some(&"p".repeat(MAX_PATH_LENGTH))).is_ok());
        assert!(path_like("projectPath", Some(&"p".repeat(MAX_PATH_LENGTH + 1))).is_err());
    }

    #[test]
    fn a_rejection_is_reserved_is_error() {
        // The whole point: these are the sanctioned `isError: true` — a malformed argument, not a
        // recoverable "nothing found". `to_call_result` must set the flag.
        assert_eq!(
            free_form("query", "")
                .unwrap_err()
                .to_call_result()
                .is_error,
            Some(true)
        );
    }
}
