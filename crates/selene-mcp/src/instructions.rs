//! The **server-instructions** — the ONE place agent-facing guidance lives.
//!
//! # Ported verbatim. Not one word of guidance rewritten.
//!
//! These were tuned against real agent behavior, and a well-meant improvement is exactly how
//! that tuning is lost: silently, with every test still green, and the only symptom an agent
//! that starts reaching for `Read` again.
//!
//! So the port applies a **mechanical rename table and nothing else**:
//!
//! | TS | Selene |
//! |---|---|
//! | `Codegraph` / `CodeGraph` / `codegraph` | `Selene` / `selene` |
//! | `codegraph_<tool>` | `selene_<tool>` |
//! | `.codegraph/` | `.selene/` |
//! | `codegraph init` | `selene index` |
//! | `[[codegraph-explore-summary]]` | `[[selene-explore-summary]]` |
//! | `a SQLite knowledge graph` | `an embedded knowledge graph` (the one **factual** fix) |
//!
//! `instructions_are_the_ts_text_with_only_the_rename_table_applied` keeps the TS original in
//! a fixture, applies the pairs, and asserts **byte-equality** with [`SERVER_INSTRUCTIONS`].
//! So the diff against TS stays reviewable line by line, forever — and a "small improvement"
//! fails the build instead of quietly costing us the tuning.
//!
//! # There is a single tool, and that sentence is only true under the visibility gate
//!
//! The text says *"There is a single tool"*. That is true because Task 15 hides the other six
//! behind `SELENE_MCP_TOOLS` — the two move together. Change one and the instructions lie.
//!
//! # [`instructions_for`] — the gate and the text move together, BY CONSTRUCTION
//!
//! When `SELENE_MCP_TOOLS` unhides `insights`/`recall`, the instructions must
//! say when to use them — and when NOT to — or the agent misbehaves (calls
//! `insights` for a code question, treats `recall` as an answer instead of a
//! pointer). So the final string is ASSEMBLED from the visible tool set: the
//! pinned base text (never edited — the byte-equality test), plus one
//! addendum per unhidden tool, each with explicit DO/DON'T discipline. The
//! single-source invariant survives: this module is still the only place any
//! guidance lives; it just has more than one paragraph to give.

/// The server-instructions, sent in `initialize`.
pub const SERVER_INSTRUCTIONS: &str = "# Selene — code intelligence over an indexed knowledge graph

Selene is an embedded knowledge graph of every symbol, edge, and file in
the workspace — pre-computed structure you would otherwise re-derive by
reading files (cached intelligence: thousands of parse/trace decisions you
don't pay to re-reason each run). Reads are sub-millisecond; the index lags
writes by ~1s through the file watcher. Reach for it BEFORE *and* while
writing or editing code — not just for questions: one call returns the
verbatim source PLUS who calls it and what it affects, so you edit with the
blast radius in view. More accurate context, in far fewer tokens and
round-trips than reading files yourself.

## One tool: selene_explore — use it instead of reading files

There is a single tool, `selene_explore`, and it is Read-equivalent. It
takes either a natural-language question or a bag of symbol/file names and
returns the **verbatim, line-numbered source** of the relevant symbols
grouped by file — the same `<n>\\t<line>` shape `Read` gives you, safe to
`Edit` from — PLUS the call path among them (including dynamic-dispatch hops
like callbacks, React re-render, and JSX children that grep can't follow) and
a blast-radius summary of what depends on them.

Whether you're answering \"how does X work\" or implementing a change (fixing a
bug, adding a feature), call `selene_explore` before you Read. ONE call
usually answers the whole question. Selene IS the pre-built search index —
so running your own grep + read loop, or delegating the lookup to a separate
file-reading sub-task/agent, repeats work selene already did and costs more
for the same answer. A direct selene answer is typically one to a few
calls; a grep/read exploration is dozens.

## How to query

- **Almost any question — \"how does X work\", architecture, a bug, \"what/where is X\", or surveying an area** → `selene_explore` with a natural-language question or the relevant names. ONE capped call returns the verbatim source grouped by file; most often the ONLY call you need.
- **\"How does X reach/become Y? / the flow / the path from X to Y\"** → `selene_explore`, naming the symbols that span the flow (e.g. `mutateElement renderScene`) — it surfaces the call path among them, riding dynamic-dispatch hops, and returns their source.
- **Reading or editing a file/symbol you can name** → put its name or file path in the `selene_explore` query — it returns that current line-numbered source (safe to `Edit` from) with the call path and blast radius attached, so you don't Read it separately. For an overloaded name it returns every matching definition's body in one call.
- **Need more?** Call `selene_explore` again with more specific names — treat the source it returns as already Read.

## Anti-patterns

- **Trust selene's results — don't re-verify them with grep.** They come from a full AST parse; re-checking with grep is slower, less accurate, and wastes context.
- **Don't grep or Read first** to find or understand indexed code — ONE `selene_explore` returns the relevant symbols' source together in a single round-trip. Reach for raw `Read`/`Grep` only to confirm a specific detail selene didn't cover, or for what selene doesn't index (configs, docs).
- **Don't reconstruct a flow by hand** — name the endpoints in one `selene_explore` and it surfaces the path between them, dynamic-dispatch hops included.
- **After editing, check the staleness banner.** When a tool response starts with \"⚠️ Some files referenced below were edited since the last index sync…\", the listed files are pending re-index — Read those specific files for accurate content. Every file NOT in that banner is fresh, so still trust selene. A different, rarer banner — \"⚠️ Selene auto-sync is DISABLED…\" — means live watching stopped entirely (the whole index is frozen, not just a few files); until it's resolved, Read files directly to confirm anything that may have changed.

## Limitations

- If a tool reports a project isn't indexed (no `.selene/`), stop calling selene tools for that project for the rest of the session and use your built-in tools there instead. Indexing is the user's decision — mention they can run `selene index` if it comes up, but don't run it yourself.
- Index lags file writes by ~1 second.
- Cross-file resolution is best-effort name matching; ambiguous calls may return multiple candidates.
- No live correctness validation — that's still the TypeScript compiler / test suite / linter's job. Selene supplements those with structural context they don't have.
";

/// Appended ALWAYS (documents shipped 2026-08-18): supersedes the base text's
/// "(configs, docs)" limitation for documents — they ARE indexed now.
const DOCS_ADDENDUM: &str = "
## Documents are in the graph too

Markdown/txt/rst files — and the extracted text of .docx/.pdf — are indexed
as document/section nodes whose code-spans, paths and links are bound to the
code they mention. A rationale-shaped question (\"why was X chosen\", \"where
is Y documented\") is a normal `selene_explore` call: it returns the relevant
documentation section verbatim alongside the code. Do NOT Read documentation
files to answer such questions — ask explore. (The base text's note that
selene doesn't index docs is superseded; only pure config values remain
outside the graph.)
";

/// Appended when `insights` is visible.
const INSIGHTS_ADDENDUM: &str = "
## selene_insights — the architecture map (use SPARINGLY)

`selene_insights` returns a whole-graph structural summary: the betweenness
bottlenecks (the symbols most shortest paths flow through — the risky places
to change), the call-graph clusters (the REAL modules, computed from who
calls whom, each named by its hub symbol), module import cycles, rare
one-edge cross-module bridges, and orphan modules.

WHEN to call it — exactly these cases:
- First contact with an unfamiliar codebase, BEFORE the first explore.
- An architecture-scale question: \"is this well layered\", \"what are the
  main components\", \"where is it risky to change things\".

WHEN NOT — everything else:
- Any question about specific code, a symbol, a bug, a flow → `selene_explore`.
- Never call it twice in a session unless the code changed: it is
  deterministic — the second answer is byte-identical to the first.
- Never chain insights→insights or use it to \"browse\". It has no
  parameters to vary; one call gives everything it has.
";

/// Appended when `recall` is visible.
const RECALL_ADDENDUM: &str = "
## selene_recall — past explorations (POINTERS, never answers)

`selene_recall` lists what `selene_explore` was asked in earlier sessions of
this project, with the root symbols each answer started from. Optional `path`
argument = filter words.

WHEN to call it — exactly these cases:
- At the START of a session on a project you may have explored before: one
  call shows where previous sessions already dug.
- Before exploring a theme that sounds familiar (\"didn't we look at the
  watermark pipeline already?\").

Discipline:
- A recall result is a POINTER, not an answer. The code may have changed
  since. ALWAYS follow up with one `selene_explore` naming the recalled
  symbols to get the current truth — never quote a recalled headline as if
  it were fresh source.
- At most ONE recall per session. An empty journal (\"Nothing remembered
  yet\") is normal on first contact — proceed straight to explore and do not
  retry.
";

/// Assemble the instructions for the actually-visible tool set. The pinned
/// base is byte-untouched; addenda attach per unhidden tool. This is the ONLY
/// assembly point — the single-source invariant lives here.
pub fn instructions_for(visible: &std::collections::BTreeSet<&'static str>) -> String {
    let mut out = String::from(SERVER_INSTRUCTIONS);
    out.push_str(DOCS_ADDENDUM);
    if visible.contains("insights") {
        out.push_str(INSIGHTS_ADDENDUM);
    }
    if visible.contains("recall") {
        out.push_str(RECALL_ADDENDUM);
    }
    out
}

#[cfg(test)]
mod addendum_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn default_gate_gets_docs_note_but_no_hidden_tool_guidance() {
        let visible: BTreeSet<&'static str> = ["explore"].into_iter().collect();
        let text = instructions_for(&visible);
        assert!(text.starts_with(SERVER_INSTRUCTIONS), "base is byte-intact");
        assert!(text.contains("Documents are in the graph"));
        assert!(
            !text.contains("selene_insights") && !text.contains("selene_recall"),
            "guidance for hidden tools would teach the agent to call tools it cannot see"
        );
    }

    #[test]
    fn unhiding_a_tool_brings_its_discipline_with_it() {
        let visible: BTreeSet<&'static str> =
            ["explore", "insights", "recall"].into_iter().collect();
        let text = instructions_for(&visible);
        for needle in [
            "selene_insights — the architecture map",
            "Never call it twice in a session",
            "selene_recall — past explorations (POINTERS, never answers)",
            "ALWAYS follow up with one `selene_explore`",
            "At most ONE recall per session",
        ] {
            assert!(text.contains(needle), "missing: {needle}");
        }
    }
}
