//! `ToolOutcome` — **the isError discipline, in one place**.
//!
//! # The spike found three outcomes, not two. This type is why that matters.
//!
//! `rmcp` 2.2 (`handler/server/tool.rs:94-112`):
//!
//! | What a `#[tool]` returns | Wire shape |
//! |---|---|
//! | `Ok(CallToolResult::success(..))` | `{"content":[…], "isError":false}` |
//! | `Ok(CallToolResult::error(..))` | `{"content":[…], "isError":true}` |
//! | **`Err(rmcp::ErrorData)`** | **a JSON-RPC `-32603` TRANSPORT failure — not `isError`** |
//!
//! So a `?` on a store error escaping a handler does not produce a failed *call* — it
//! produces a **broken server**, which some clients surface as exactly that. Every handler
//! must therefore classify **inside** itself and return a `ToolOutcome`.
//!
//! # `isError` is RESERVED
//!
//! Only two things earn it: a **`PathRefusal`** (security) and a **genuine malfunction**.
//!
//! Everything else — not indexed, symbol not found, file absent, nothing relevant, an offset
//! past the end — is **success-shaped guidance**. This is the single most load-bearing rule
//! in the phase: **one `isError` early and an agent abandons the tool for the whole session.**
//! It does not retry with a better query; it goes back to `Read`, and every call after that is
//! one we never see.
//!
//! Living in one file means three handlers cannot each invent their own classification — the
//! rule is applied once, and `a_store_error_is_the_only_thing_that_sets_is_error` guards it.

use rmcp::model::{CallToolResult, ContentBlock};

/// The outcome of a tool call, before it becomes a wire shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// An answer, or **guidance that reads like an answer**. `isError: false`.
    Ok(String),
    /// A path refusal or a genuine malfunction. `isError: true` — and nothing else may be.
    Failed(String),
}

impl ToolOutcome {
    /// Guidance — the not-indexed / not-found / nothing-relevant path. **Success-shaped.**
    pub fn guidance(text: impl Into<String>) -> Self {
        Self::Ok(text.into())
    }

    /// The reserved path. Use it for `GraphError::PathRefusal` and genuine malfunctions only.
    pub fn failed(text: impl Into<String>) -> Self {
        Self::Failed(text.into())
    }

    /// Classify an error from the layers below.
    ///
    /// A **store malfunction** is a real failure. A **path refusal** is a real refusal. There
    /// is nothing else — `selene-graph` and `selene-context` were built so that every
    /// recoverable condition is an `Ok` value, precisely so this function has only two arms.
    pub fn from_error(err: &selene_context::ContextError) -> Self {
        Self::Failed(format!("{err}"))
    }

    /// To the wire. **Never `Err`** — see the module docs: an `Err` here is a transport
    /// failure, not a failed call.
    pub fn to_call_result(self) -> CallToolResult {
        match self {
            Self::Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Self::Failed(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_is_success_shaped() {
        let r = ToolOutcome::guidance("not indexed — run `selene index`").to_call_result();
        assert_eq!(
            r.is_error,
            Some(false),
            "'not indexed' is the most common FIRST CONTACT an agent has with this tool. One \
             isError there and it never calls us again — it goes back to Read, and every call \
             after that is one we never see."
        );
    }

    #[test]
    fn only_a_refusal_or_a_malfunction_sets_is_error() {
        let r = ToolOutcome::failed("path refused: outside project root").to_call_result();
        assert_eq!(r.is_error, Some(true));
    }
}
