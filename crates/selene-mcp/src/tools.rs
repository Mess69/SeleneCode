//! The seven tools — and **the visibility gate**.
//!
//! # `explore` is the ONLY default-visible tool. The other six are hidden.
//!
//! All seven are implemented and callable. Six are hidden behind `SELENE_MCP_TOOLS` — and the
//! *reason* matters more than the mechanism:
//!
//! **An agent facing seven tools reaches for the wrong one.** It sees `callers`, `impact`,
//! `search`, and it composes a little research plan: search, then callers, then node, then
//! maybe impact. Four round-trips, four partial answers it has to stitch. The product bet is
//! that **`explore` answers the whole thing in one call** — the flow, the source, the blast
//! radius, together — and the way to make an agent take that bet is to give it one door.
//!
//! The server-instructions say, in the ported text, *"There is a single tool"*. That sentence
//! is **only true under this gate**. The two move together: unhide the six and the
//! instructions become a lie.
//!
//! `SELENE_MCP_TOOLS=explore,callers,node` opts back in — for a human debugging, or a client
//! that genuinely wants the primitives.

use std::collections::BTreeSet;

/// Every tool, in a stable order.
pub const ALL_TOOLS: &[&str] = &[
    "explore", "node", "search", "callers", "callees", "impact", "files", "insights",
];

/// The one tool an agent sees by default.
pub const DEFAULT_VISIBLE: &[&str] = &["explore"];

/// The env var that opts the other six back in.
pub const TOOLS_ENV: &str = "SELENE_MCP_TOOLS";

/// Which tools are visible, given the environment.
///
/// Unset ⇒ `explore` only. Set ⇒ exactly the named ones (unknown names ignored — a typo must
/// not silently hide everything).
pub fn visible_tools(env: Option<&str>) -> BTreeSet<&'static str> {
    let Some(raw) = env else {
        return DEFAULT_VISIBLE.iter().copied().collect();
    };

    let requested: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let chosen: BTreeSet<&'static str> = ALL_TOOLS
        .iter()
        .copied()
        .filter(|t| requested.iter().any(|r| r == t))
        .collect();

    // An empty or all-typo value falls back to the default rather than exposing NOTHING — a
    // server with no tools is indistinguishable from a broken one.
    if chosen.is_empty() {
        return DEFAULT_VISIBLE.iter().copied().collect();
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explore_is_the_only_default_visible_tool() {
        let visible = visible_tools(None);
        assert_eq!(
            visible.iter().copied().collect::<Vec<_>>(),
            vec!["explore"],
            "an agent facing seven tools reaches for the wrong one and composes a four-call \
             research plan. The bet is that explore answers in ONE call — and the way to make \
             an agent take that bet is to give it one door."
        );
    }

    #[test]
    fn the_env_var_opts_the_others_back_in() {
        let visible = visible_tools(Some("explore,callers,node"));
        assert_eq!(
            visible.iter().copied().collect::<Vec<_>>(),
            vec!["callers", "explore", "node"]
        );
    }

    /// A typo must not silently produce a server with no tools — that is indistinguishable
    /// from a broken one.
    #[test]
    fn an_all_typo_value_falls_back_to_the_default() {
        assert_eq!(
            visible_tools(Some("explor,nodes"))
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec!["explore"]
        );
        assert_eq!(
            visible_tools(Some("")).iter().copied().collect::<Vec<_>>(),
            vec!["explore"]
        );
    }

    #[test]
    fn there_are_exactly_eight_tools() {
        assert_eq!(
            ALL_TOOLS.len(),
            8,
            "seven ported from the map + `insights` (2026-08-18, PRD graph-platform F3)"
        );
    }
}
