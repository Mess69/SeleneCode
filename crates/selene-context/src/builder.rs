//! `ContextBuilder` — the bytes an agent actually reads.
//!
//! # This is where the product bet is won or lost
//!
//! Passes 1–11 decide *what* to show. This file decides whether what is shown lets the agent
//! answer **without opening the file**. Correct-but-insufficient is a failed product: an
//! answer that is accurate and sends the agent to `Read` has cost more than it saved,
//! because the agent now pays for both.
//!
//! So every section here exists to remove a reason to Read:
//!
//! - **The source is verbatim and line-numbered** (`selene_graph::number_lines`), so the
//!   agent can cite `file:line` exactly as if it had Read the file. That is why the numbering
//!   is byte-identical to `Read` — an off-by-one and the agent opens the file to check us.
//! - **The call paths are rendered**, so "what calls this" needs no second tool call.
//! - **The relationships are named**, so the agent does not go looking for the other end.
//!
//! # The low-confidence handoff — the most important string in this crate
//!
//! When the graph genuinely cannot answer, the output says so **and says what to do next**.
//! It does *not* return thin context that looks like an answer.
//!
//! That is not politeness. A confident wrong answer is the one failure mode an agent cannot
//! detect: it will build on it. An honest "I don't have this — here is how to ask better"
//! costs one turn; a plausible wrong answer costs the whole task. And the handoff never says
//! "read the file" — it says how to *ask this tool* a better question, because sending the
//! agent to `Read` is losing the bet on purpose.

use selene_core::EdgeKind;
use selene_db::GraphStore;
use selene_graph::{QueryManager, number_lines};

use crate::budgets::{ExploreBudget, budget_for, truncate_to_ceiling};
use crate::error::Result;
use crate::relevance::{Confidence, FindOptions, RelevantContext, find_relevant_context};

/// The section prefix a file block starts with. **Load-bearing**: truncation cuts on it, so
/// it must stay unique and greppable (`budgets::FILE_SECTION_PREFIX`).
const FILE_HEADER: &str = "**`";

/// Builds the context string for a query.
pub struct ContextBuilder<S: GraphStore> {
    qm: QueryManager<S>,
}

impl<S: GraphStore> ContextBuilder<S> {
    /// Wrap a query manager.
    pub fn new(qm: QueryManager<S>) -> Self {
        Self { qm }
    }

    /// The query surface, for callers that need it directly.
    pub fn queries(&self) -> &QueryManager<S> {
        &self.qm
    }

    /// **Build the answer.**
    ///
    /// Never `Err` for "not indexed", "nothing relevant" or "low confidence" — each is an
    /// ordinary answer, rendered as guidance. An `Err` here becomes an `isError` at the MCP
    /// layer, and one of those early ends the session.
    pub async fn build_context(&self, query: &str) -> Result<String> {
        // Not indexed: an answer, and the single most common first contact an agent has.
        if !self.qm.is_indexed().await? {
            return Ok(NOT_INDEXED.to_string());
        }

        let file_count = self.qm.file_count().await?;
        let budget = budget_for(file_count);

        let ctx = find_relevant_context(&self.qm, query, &FindOptions::default(), None).await?;

        if ctx.subgraph.nodes.is_empty() {
            return Ok(self.no_results_handoff(query));
        }

        let mut out = String::new();
        out.push_str(&self.render_summary(query, &ctx, file_count));

        // The honest half: say it BEFORE the content, so the agent reads the caveat first and
        // weighs what follows — not after, where it reads like a footnote.
        if ctx.confidence == Confidence::Low {
            out.push_str(&self.low_confidence_handoff(query));
        }

        out.push_str(&self.render_relationships(&ctx, &budget));
        out.push_str(&self.render_files(&ctx, &budget).await?);

        Ok(truncate_to_ceiling(&out, &budget))
    }

    /// The header: what was asked, what was found, and how confident we are.
    fn render_summary(&self, query: &str, ctx: &RelevantContext, file_count: u64) -> String {
        let roots: Vec<&str> = ctx.roots.iter().map(|r| r.node.name.as_str()).collect();
        format!(
            "## Context for: {query}\n\n\
             Found **{}** relevant symbols across **{}** indexed files. Starting from: {}.\n\n",
            ctx.subgraph.nodes.len(),
            thousands(file_count),
            roots
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// The relationships — **the section that removes a whole tool call**. An agent that can
    /// see "who calls this" here does not need to ask for callers.
    fn render_relationships(&self, ctx: &RelevantContext, budget: &ExploreBudget) -> String {
        if !budget.include_relationships || ctx.subgraph.edges.is_empty() {
            return String::new();
        }

        let mut out = String::from("### Relationships\n\n");
        let mut shown_per_kind: indexmap::IndexMap<&str, usize> = indexmap::IndexMap::new();

        for e in &ctx.subgraph.edges {
            let kind = e.kind.as_str();
            let count = shown_per_kind.entry(kind).or_insert(0);
            if *count >= budget.max_edges_per_relationship_kind {
                continue;
            }
            let (Some(src), Some(dst)) = (
                ctx.subgraph.nodes.get(&e.source),
                ctx.subgraph.nodes.get(&e.target),
            ) else {
                continue;
            };

            // A synthesized edge is a DYNAMIC hop — the agent cannot find it by reading, which
            // is precisely why it is marked rather than rendered as an ordinary call.
            let marker = if e.provenance == Some(selene_core::Provenance::Heuristic) {
                " *(dynamic)*"
            } else {
                ""
            };
            out.push_str(&format!(
                "- `{}` **{}** `{}`{marker}\n",
                src.name, kind, dst.name
            ));
            *count += 1;
        }
        out.push('\n');
        out
    }

    /// The file sections — verbatim, line-numbered source.
    async fn render_files(&self, ctx: &RelevantContext, budget: &ExploreBudget) -> Result<String> {
        // Group the gathered nodes by file, keeping first-seen (rank) order.
        let mut by_file: indexmap::IndexMap<String, Vec<&selene_core::Node>> =
            indexmap::IndexMap::new();
        for node in ctx.subgraph.nodes.values() {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }

        let mut out = String::new();
        for (path, nodes) in by_file.iter().take(budget.default_max_files) {
            let symbols: Vec<&str> = nodes
                .iter()
                .take(budget.max_symbols_in_file_header)
                .map(|n| n.name.as_str())
                .collect();

            out.push_str(&format!(
                "{FILE_HEADER}{path}`** — {}\n\n",
                symbols.join(", ")
            ));

            // The source, verbatim and numbered, up to the per-file budget.
            let mut spent = 0usize;
            for node in nodes.iter() {
                if spent >= budget.max_chars_per_file {
                    break;
                }
                let Some(code) = self.qm.code_of(node)? else {
                    continue; // the file is gone — a fact, not an error
                };
                let numbered = number_lines(&code, node.start_line as usize);
                let room = budget.max_chars_per_file - spent;
                let slice = if numbered.len() > room {
                    &numbered[..floor_boundary(&numbered, room)]
                } else {
                    &numbered
                };
                out.push_str("```\n");
                out.push_str(slice);
                out.push_str("```\n\n");
                spent += slice.len();
            }
        }
        Ok(out)
    }

    /// **The low-confidence handoff.** Honest, and actionable — and it never says "Read".
    fn low_confidence_handoff(&self, query: &str) -> String {
        format!(
            "> ⚠️ **Low confidence.** Nothing in the graph matched more than one term of \
             \"{query}\", and nothing came back that you did not already name — so the \
             symbols below may not be what you meant.\n\
             >\n\
             > **What to do next:** run `selene_explore` again naming a specific symbol, \
             file, or route from the list below (for example the exact function name you \
             expect to exist). Do **not** open these files to check — a narrower explore \
             sees more than a file does, because it follows the call graph across files.\n\n"
        )
    }

    /// Nothing matched. Still an answer, and still not a reason to Read.
    fn no_results_handoff(&self, query: &str) -> String {
        format!(
            "## Context for: {query}\n\n\
             **No relevant symbols found.** The query did not name anything the index \
             recognizes.\n\n\
             **What to do next:**\n\
             - Name a symbol, file, or route directly (`handleLogin`, `src/auth.ts`, \
             `POST /login`) — common English words are filtered out because they match \
             thousands of unrelated symbols.\n\
             - If you expected this code to exist, it may not be indexed yet: run \
             `selene index`.\n\n\
             Do **not** fall back to reading files — a `selene_explore` naming one concrete \
             symbol will find the flow around it, which reading a file cannot.\n"
        )
    }
}

/// The not-indexed guidance. Success-shaped, always.
pub const NOT_INDEXED: &str = "## Not indexed\n\nThis project has no index yet, so there is nothing to explore.\n\n**Run `selene index`** in the project root, then try again.\n";

/// `1234` → `1,234`. Rust has no `toLocaleString`, so the format is pinned here (and by a
/// test) rather than differing between call sites.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The edge kinds a call path may traverse.
pub const CALL_PATH_KINDS: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::References];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_is_pinned_because_rust_has_no_locale_string() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
