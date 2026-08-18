//! `raw_select` — the read-only SurrealQL passthrough behind `selene query --raw`
//! (graph-platform PRD 2026-08-18 §7 / F6).
//!
//! # A convenience guard, not a security boundary
//!
//! The caller owns the database file; nothing here defends against them. The
//! guard defends against *fat fingers*: a statement that is not a bare
//! `SELECT` (mutations, DDL, transactions) is refused before it reaches the
//! engine. Screening is lexical (leading keyword + a banned-keyword sweep of
//! the whole text) — cruder than an AST check, and deliberately biased to
//! REFUSE on doubt: a false refusal costs a rewrite, a false accept could cost
//! the index. CLI-only by design: the MCP surface keeps its curated tools
//! (anti-Read: an agent improvising SurrealQL is the failure mode, not a
//! feature).

use crate::error::{Error, Result};
use crate::surreal::SurrealStore;

/// Injected when the query has no `LIMIT` of its own.
pub const RAW_DEFAULT_LIMIT: usize = 1000;
/// Engine-side time budget for one raw query.
pub const RAW_TIMEOUT_SECS: u64 = 5;

/// Words that end the conversation, wherever they appear. Upper-cased compare
/// over identifier-ish tokens, so `deleted_at` (a field) stays legal while
/// `DELETE` (a statement) never runs.
const BANNED: &[&str] = &[
    "UPDATE", "DELETE", "CREATE", "INSERT", "UPSERT", "RELATE", "DEFINE", "REMOVE", "ALTER",
    "BEGIN", "COMMIT", "CANCEL", "LET", "USE", "INFO", "KILL", "LIVE", "SLEEP", "REBUILD",
];

/// Refuse anything that is not a single read-only `SELECT`.
pub fn validate_raw_select(query: &str) -> Result<()> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(Error::Validation("empty query".into()));
    }
    // One statement only — a `;` separator means a second one follows (a
    // trailing `;` is tolerated).
    let without_trailing = trimmed.strip_suffix(';').unwrap_or(trimmed);
    if without_trailing.contains(';') {
        return Err(Error::Validation(
            "one statement per --raw call (`;` found)".into(),
        ));
    }
    if !without_trailing
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("SELECT")
    {
        return Err(Error::Validation(
            "--raw runs read-only SELECT statements only".into(),
        ));
    }
    // Token-wise banned sweep: split on non-identifier chars so string
    // literals' contents still trip the guard (bias to refuse — a SELECT with
    // 'DELETE' in a literal is rare; a mutation smuggled past the guard is
    // fatal).
    for tok in without_trailing.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        if BANNED.contains(&tok.to_ascii_uppercase().as_str()) {
            return Err(Error::Validation(format!(
                "--raw refuses `{tok}` (read-only SELECT only)"
            )));
        }
    }
    Ok(())
}

impl SurrealStore {
    /// Run one validated read-only `SELECT`; rows come back as JSON. A missing
    /// `LIMIT` gets [`RAW_DEFAULT_LIMIT`]; every query gets an engine-side
    /// `TIMEOUT` of [`RAW_TIMEOUT_SECS`].
    pub async fn raw_select(&self, query: &str) -> Result<Vec<serde_json::Value>> {
        validate_raw_select(query)?;
        let trimmed = query.trim();
        let without_trailing = trimmed.strip_suffix(';').unwrap_or(trimmed);
        let upper = without_trailing.to_ascii_uppercase();
        let mut q = without_trailing.to_string();
        if !upper.contains("LIMIT") {
            q.push_str(&format!(" LIMIT {RAW_DEFAULT_LIMIT}"));
        }
        if !upper.contains("TIMEOUT") {
            q.push_str(&format!(" TIMEOUT {RAW_TIMEOUT_SECS}s"));
        }
        let mut resp = self.db().query(q).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn only_bare_select_passes() {
        assert!(validate_raw_select("SELECT * FROM node").is_ok());
        assert!(validate_raw_select("  select id, name from node where kind = 'route';").is_ok());
        for bad in [
            "UPDATE node SET name = 'x'",
            "DELETE node",
            "DEFINE TABLE evil",
            "REMOVE INDEX node_kind ON node",
            "SELECT * FROM node; DELETE node",
            "BEGIN TRANSACTION",
            "SELECT * FROM (DELETE node RETURN BEFORE)",
            "",
        ] {
            assert!(validate_raw_select(bad).is_err(), "must refuse: {bad}");
        }
    }

    #[test]
    fn field_names_that_contain_banned_words_stay_legal() {
        assert!(validate_raw_select("SELECT deleted_at, created_by FROM node").is_ok());
    }
}
