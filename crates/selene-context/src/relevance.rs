//! Relevance scoring — **the ordered pass list. Order IS behavior.**
//!
//! ```text
//!  0. corpus-derived terms — the codebase's own sub-words the query contains ✅
//!  1. symbol extraction from the query                        (Task 5) ✅
//!  2. exact-name lookup + co-location boost   (+20 per extra) (Task 5) ✅
//!  3. TitleCase prefix search over definitions (+15 + brevity)(Task 5) ✅
//!  4. per-term FTS (NORMALIZED to 25) + multi-term boost (+5),
//!     test dampen (×0.3), dominant-file core-dir boost (+25)  (Task 5) ✅
//!  6. camelCase-boundary LIKE matches  (8 + brevity) × tier             ✅
//!  7. compound ≥2-term LIKE matches    10 + 20(n−1) + brevity           ✅
//!  5. term-group rerank — ×(1+0.5n) / ×0.3 / ×0.6  ⚠ AFTER the merge    ✅
//! 12. graph connectivity — which symbols BRIDGE the query's concepts     ✅
//!  8. sort → slice(search_limit*3) → min-score                (Task 6) ✅
//!  9. confidence: LOW iff ≥2 terms AND nothing with 2 hits or a distinctive name (T6) ✅
//! 13. root diversity — one root per NAME                                 ✅
//! 10. type-hierarchy expansion (budget max_nodes/4)            (Task 6) ✅
//! 11. BFS both directions → trims → edge recovery              (Task 6) ✅
//! ```
//!
//! **The list is not in numeric order, and that is the point — order IS behavior.** Three of
//! these placements are load-bearing and each one was a bug:
//!
//! - **5 runs AFTER 6/7**, not before. The merge between the channels takes `max()`, so a
//!   rerank that ran first was simply undone by it: the penalty scaled a noise candidate down,
//!   and `max()` restored the un-penalized score from the LIKE channel. A rescale `max()`-ed
//!   against its own input is a no-op.
//! - **12 runs BEFORE the cut** at pass 8. It *admits* nodes no lexical pass found — run it
//!   after the truncate and the truncate has already thrown the answer away.
//! - **13 replaces the old `take(search_limit)`.** Three nodes named `insert_edges` (a trait
//!   decl and two impls) are three *spellings*, not three answers.
//!
//! Passes 1–11 are all lexical — they match the query's WORDS against symbol NAMES. **Pass 12
//! is the only one that consults the graph**, and without it a natural-language flow question
//! is answered by word-matching alone. See [`apply_graph_connectivity`].
//!
//! # Pass 0 exists because every other pass tests containment in only ONE direction
//!
//! Passes 1–11 all ask *"does the symbol's name contain the query's word?"* — and for a question
//! like *"how does an unresolved reference become a graph edge"* the answer is `resolve_one`,
//! whose name contains **none** of those words. `"unresolved"` contains `"resolve"`; nothing ever
//! asked. So the four symbols that answer the milestone question were **not candidates at all**,
//! and every previous attempt to fix this re-ranked a set that did not contain the answer.
//!
//! Pass 0 asks the reverse question, and asks it of the **corpus, not of English**. See
//! [`derive_corpus_terms`].
//!
//! # The weights are ported — and the SCALE they are weighed against was not
//!
//! `+20` for co-location is what makes `scrapeLoop` and `run` in the same file outrank two
//! unrelated symbols of the same name; `×0.3` on test files is what keeps a test double from
//! outranking the thing it doubles. But a ported weight only means something against a ported
//! *input scale*, and two of ours were wrong by an order of magnitude in opposite directions:
//! the store's FTS `raw_score` arrives at ~150 (it is `20·bm25(name) + …`) while its
//! `search_name_like` `raw_score` arrives at 0.5–1.0 (it is a *tier*, not a score). Fed in
//! raw, FTS *was* the ranking and the LIKE channel was numerically switched off — so every
//! weight below, however faithfully ported, was rounding error. Both are normalized at their
//! call sites now. See [`weights::FTS_MAX`] and [`weights::LIKE_BASE`].

use indexmap::{IndexMap, IndexSet};
use selene_core::{Edge, EdgeKind, Node, NodeKind};
use selene_db::{GraphStore, SearchCandidate, Subgraph};
use selene_graph::QueryManager;

use crate::error::Result;
use crate::stopwords::extract_search_terms;

/// The kinds worth showing an agent. Excludes `import`/`export`/`parameter` — they flood FTS
/// with qualified-name matches and are almost never what an exploration query wants.
pub const HIGH_VALUE_NODE_KINDS: &[NodeKind] = &[
    NodeKind::File,
    NodeKind::Module,
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Property,
    NodeKind::Field,
    NodeKind::Variable,
    NodeKind::Constant,
    NodeKind::Enum,
    NodeKind::EnumMember,
    NodeKind::TypeAlias,
    NodeKind::Namespace,
    NodeKind::Route,
    NodeKind::Component,
];

/// The kinds that can be a **step in a flow** — TS's `CALLABLE`
/// (`maps/mcp-context.md` §3). Pass 12 will only *volunteer* a node of one of these kinds: a
/// constant may sit right next to the answer, but it is not a hop, and an agent asking how
/// something works cannot follow it anywhere.
pub const CALLABLE_KINDS: &[NodeKind] =
    &[NodeKind::Function, NodeKind::Method, NodeKind::Component];

/// The kinds pass 3 searches — a "definition" is a type, not a function.
const DEFINITION_KINDS: &[NodeKind] = &[
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::Struct,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
    NodeKind::TypeAlias,
];

/// Ported weights. Named, so a reader sees them as a contract rather than as magic numbers.
pub mod weights {
    /// Pass 2: per *extra* co-named symbol in the same file.
    pub const CO_LOCATION: f64 = 20.0;
    /// Pass 3: a TitleCase prefix hit on a definition kind.
    pub const PREFIX_HIT: f64 = 15.0;
    /// Pass 4: per *extra* term a node matches.
    pub const MULTI_TERM: f64 = 5.0;
    /// Pass 4: a test file, when the query is not itself about tests.
    pub const TEST_DAMPEN: f64 = 0.3;
    /// Pass 4: sharing a directory with the repo's dominant file.
    pub const CORE_DIR: f64 = 25.0;
    /// Pass 4: the dominant file must hold ≥ this multiple of the runner-up's edges.
    pub const DOMINANCE_RATIO: u64 = 3;
    /// Pass 4: **what the best FTS hit in a query is worth.**
    ///
    /// The store's raw FTS score is `20·bm25(name) + …`, which lands near 150 — five times the
    /// scale of every other weight here, so it silently *became* the ranking. Normalizing the
    /// channel to this ceiling is what lets the rest of the passes function at all. Deliberately
    /// **below** a 2-term compound hit ([`COMPOUND_BASE`] + [`COMPOUND_PER_EXTRA_TERM`] = 30):
    /// matching two of the query's concepts must beat matching one word very well.
    pub const FTS_MAX: f64 = 25.0;
    /// Semantic (vector) candidates enter scoring at this FRACTION of `FTS_MAX`, normalized among
    /// themselves. < 1 so a strong lexical hit still leads; > 0 so a symbol the query *means* but
    /// does not *name* (the vocabulary gap) becomes a seed at all. Only ever applied when the index
    /// has embeddings and a query vector was supplied — a lexical index is untouched.
    pub const SEMANTIC_WEIGHT: f64 = 0.8;

    // ── Pass 5's rerank. MULTIPLICATIVE — see `rerank_by_term_groups`. ──────────────
    /// Per matched concept, when a node matched ≥2: `×(1 + 0.5n)`.
    pub const GROUP_SCALE: f64 = 0.5;
    /// **The penalty the port dropped.** One concept out of several ⇒ probably noise.
    pub const SINGLE_CONCEPT: f64 = 0.6;

    // ── Passes 6 & 7's LIKE scaling. ───────────────────────────────────────────────
    /// Pass 6 & 7: **how many names the LIKE channel may even look at.** TS: *"CamelCase-boundary
    /// LIKE matches (**limit 200**…)"* (`maps/mcp-context.md:132`).
    ///
    /// This was `search_limit * 2` — **sixteen**, and the third ported constant in this file to
    /// arrive an order of magnitude too small (see [`FTS_MAX`] and `FindOptions::explore`). It is
    /// not a budget knob; it is a *blindfold*. The term `resolve` matches **74** callables in this
    /// corpus, so a cap of 16 hands back an arbitrary sixteenth of them, tier-sorted — and
    /// `resolve_one`, `resolve_all` and `resolve_and_persist_batched` were simply **not
    /// candidates**, no matter what any downstream pass did with the ones that were.
    ///
    /// The cap must be sized against the corpus's answer, not against how many roots we intend to
    /// keep. Ranking cuts to `search_limit` at the end; that is where the narrowing belongs.
    pub const LIKE_LIMIT: usize = 200;
    /// Pass 6's base, before brevity and the match tier (TS: `8 + brevity + pathScore`).
    pub const LIKE_BASE: f64 = 8.0;
    /// Pass 6: per *extra* term, added after the `×(1+termCount)` scale.
    pub const LIKE_MULTI_TERM: f64 = 30.0;
    /// Pass 7: the floor a ≥2-term compound name is worth.
    pub const COMPOUND_BASE: f64 = 10.0;
    /// Pass 7: per *extra* term in a compound name. **4× the `+5` the port used.**
    pub const COMPOUND_PER_EXTRA_TERM: f64 = 20.0;

    /// Pass 12: what a *maximally* corroborated node earns. Sized against the lexical weights
    /// above on purpose — a node the whole lexical seed set points at must be able to outrank
    /// a node that merely shares a word with the query.
    pub const CONNECTIVITY: f64 = 30.0;
}

/// How deep pass 13 may look for a root that covers an as-yet-uncovered concept, as a multiple
/// of `search_limit`. Governs **reach, not count**.
const ROOT_POOL_MULTIPLE: usize = 10;

/// **Pass 0 — corpus-derived terms. The direction every other pass forgets to look.**
///
/// Every lexical pass in this file asks the same question: does the **symbol's name contain the
/// query's word**? For *"how does an unresolved reference become a graph edge"* that is:
///
/// ```text
/// "resolve_one".contains("unresolved")   ->  false      <- the question we ask
/// "unresolved".contains("resolve")       ->  TRUE       <- the question we never ask
/// ```
///
/// The agent asked about an *un-**resolv**-ed* reference. The subsystem that turns one into an
/// edge is called ***resolve***. They share a stem, this codebase says so out loud in 52 symbol
/// names — and not one pass can see it, because containment is only ever tested one way.
///
/// That is why the milestone question returned **zero of its four required symbols**. It was
/// never a ranking bug: `resolve_and_persist_batched`, `resolve_all` and `resolve_one` were not
/// *candidates at all*, so no amount of re-scoring downstream could have surfaced them. Every
/// previous attempt tuned the ranking of a set that did not contain the answer.
///
/// # The corpus is the dictionary — not English
///
/// The trap here is to reach for morphology: a negation-prefix list (`un-`, `de-`, `non-`), a
/// snowball stemmer, an English verb table. All of them are wrong, and this crate's language
/// contract is why — they bake *one human language's grammar* into a tool whose users write
/// prompts in any of them.
///
/// So we never guess what a word *means*. We ask **the codebase which of its own sub-words the
/// query happens to contain**: enumerate the substrings of a query term and keep only the ones
/// this project actually names things with. `unresolved` yields exactly `resolve` (52 witnesses)
/// and `resolved` (15) — and nothing else, because `olved`, `nresolv` and `esolve` name nothing.
/// **The vocabulary does the filtering**, so no dictionary is needed and none is language-bound.
/// On a Spring codebase the same rule turns `unauthenticated` into `authenticate`; on Django,
/// `unmigrated` into `migrate`. It holds for one reason, and the reason is not English: **code
/// names the negation of a concept by prefixing the concept.**
///
/// # Validation is free
///
/// A candidate segment is real iff some symbol's name *starts* with it — an **indexed prefix
/// lookup** (`get_nodes_by_name_prefix`), not a scan. That is also exactly the shape of TS's own
/// primitive, down to its caveat: `getStemVariants` *"mints non-words by design (prefix-match
/// only)"* (`maps/db-graph-search.md:115`). TS ports it, threads it through
/// `extractSearchTerms(query, {stems})`, and backs it with a materialized `name_segment_vocab`
/// table. **SeleneCode ported neither.** The name-prefix index we already maintain *is* that
/// vocabulary; it had simply never been asked.
///
/// # Why nothing downstream needs to change
///
/// A derived term is a *substring* of the literal term it came from, and [`term_groups`] already
/// groups terms that contain one another — so `resolve` folds into the `unresolved` **concept**
/// on its own. The derived term therefore widens the candidate pool *without inventing a
/// concept*: `resolve_one` enters ranking already carrying *unresolved*, which is precisely what
/// pass 12 needs to bridge it against the *edge* pole. Pass 5's concept count is unchanged, so
/// nothing is double-counted.
///
/// Returns the derived terms. **Empty is the common case and the correct one** — a query whose
/// words are already the codebase's words (*"how are edges created during resolution"*) derives
/// nothing, and this pass is a measured no-op on it. It fires where we are blind, and nowhere
/// else.
pub async fn derive_corpus_terms<S: GraphStore>(
    qm: &QueryManager<S>,
    terms: &[String],
) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();

    for term in terms {
        let lower = term.to_lowercase();
        if lower.len() < segments::MIN_TERM {
            continue;
        }

        // Every substring long enough to name something, short enough to be a real derivation.
        // Longest first — `resolve` is a better term than `resolv`, and we keep few.
        // **Both directions, and it must be both.** Two real queries need opposite trims:
        //
        //   "un|resolved"  -> drop the FRONT  -> `resolve`   (code negates by prefixing)
        //   "resolut|ion"  -> drop the BACK   -> `resolut`   (code inflects by suffixing)
        //
        // An earlier cut of this pass allowed only the front-trim, on the reasoning that a
        // back-trim admits nothing the literal term does not. That reasoning is right about
        // *`unresolv`* and wrong about the pass: it fixed the milestone query and took
        // *"how are edges created during resolution"* from 3-of-3 to 0-of-3 in the same build.
        // Direction is not the discriminator.
        let chars: Vec<char> = lower.chars().collect();
        let mut candidates: IndexSet<String> = IndexSet::new();
        for start in 0..chars.len() {
            for end in (start + segments::MIN_SEGMENT)..=chars.len() {
                if chars.len() - (end - start) < segments::MIN_TRIM {
                    continue; // a one-character shave is a spelling, not a stem
                }
                let cand: String = chars[start..end].iter().collect();
                if !terms.iter().any(|t| t.to_lowercase() == cand) && !out.contains(&cand) {
                    candidates.insert(cand);
                }
            }
        }

        // **The discriminator is how much of the codebase a segment NAMES.**
        //
        // `unresolved` yields both `resolve` and `unresolv`. They are the same length class and
        // both "valid" by any structural rule — but `resolve` names **52** symbols in this project
        // and `unresolv` names a handful of truncations. One is a word this codebase speaks; the
        // other is a spelling accident. So we do not reason about morphology at all: we ask the
        // corpus which sub-words it actually uses, and keep the ones it uses most.
        //
        // This is the whole reason the pass is language-agnostic. No prefix list, no stemmer, no
        // verb table — the vocabulary of the project under the cursor is the only dictionary, and
        // it is right about Java, Python and Go for exactly the same reason it is right here.
        let mut scored: Vec<(String, usize)> = Vec::new();
        for cand in candidates {
            let hits = qm
                .store()
                .get_nodes_by_name_prefix(&cand, segments::WITNESS_LOOKUP)
                .await
                .map_err(selene_graph::GraphError::from)?;

            let witnesses: IndexSet<String> = hits
                .iter()
                .filter(|n| !is_test_file(&n.file_path))
                .map(|n| n.name.to_lowercase())
                .collect();

            if witnesses.len() >= segments::MIN_WITNESSES {
                scored.push((cand, witnesses.len()));
            }
        }

        // Most-named first; longer wins a tie (a longer stem is a more specific claim); then
        // alphabetical, because the output is rendered and ties must not reorder between runs.
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| b.0.len().cmp(&a.0.len()))
                .then_with(|| a.0.cmp(&b.0))
        });

        for (cand, _) in scored.into_iter().take(segments::MAX_PER_TERM) {
            out.push(cand);
        }
    }

    Ok(out)
}

/// Pass 0's constants. Every one exists to keep **the corpus**, not English, in charge.
pub mod segments {
    /// Shorter than this and a segment names nothing: `dge`, `raph`, `olve`.
    pub const MIN_SEGMENT: usize = 5;
    /// A term must be long enough to contain a segment *and still be a different word*.
    pub const MIN_TERM: usize = 7;
    /// The derivation must drop at least this many characters. Without it `reference` derives
    /// `referenc` — a spelling, not a stem, and it doubles every score it touches for nothing.
    pub const MIN_TRIM: usize = 2;
    /// How many symbol names must start with a segment before we believe it is a word this
    /// project speaks. **3, not 1** — one hit is a coincidence (a lone `solver` helper would
    /// mint `solve`); three is a vocabulary.
    pub const MIN_WITNESSES: usize = 3;
    /// How many names to pull when counting witnesses. Only distinctness matters, so this is a
    /// ceiling, not a budget.
    pub const WITNESS_LOOKUP: usize = 20;
    /// At most this many derivations per query word. `unresolved` yields `resolve` and
    /// `resolved`; a third would be `resolv`, which finds no symbol the first two did not.
    pub const MAX_PER_TERM: usize = 2;
}

/// **Pass 12's adjacency** — TS's `computeGraphRelevance` walks these nine kinds *undirected*
/// (`maps/mcp-context.md` §6). Undirected is the point: the caller of a matched symbol is as
/// relevant as its callee, and a flow question is usually asking about the caller.
pub const RWR_EDGE_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::Overrides,
    EdgeKind::Instantiates,
    EdgeKind::Returns,
    EdgeKind::TypeOf,
    EdgeKind::Imports,
];

/// Pass 12's constants.
pub mod rwr {
    /// How many lexical candidates seed the walk.
    pub const MAX_SEEDS: usize = 20;
    /// How far the walk carries a seed's concepts. **Two, and it must be two**: `create_edges`
    /// is one hop from `Edge` and *two* from `UnresolvedRef`, so a one-hop pass cannot see the
    /// symbol that bridges them — which is the symbol the question is about.
    pub const HOPS: usize = 2;
    /// Neighbors expanded per node — a hub with 3 000 callers must not define the answer.
    pub const MAX_NEIGHBORS_PER_NODE: usize = 60;
    /// Above this many neighbors, a node is a **hub**: its neighbors are still collected, but it
    /// may not be walked *through* to a further hop. See the hub guard in
    /// [`apply_graph_connectivity`] — without it, `UnresolvedRef` (168 neighbors) hands its
    /// concept to half the repository.
    pub const HUB_DEGREE: usize = 24;
    /// A *discovered* node (one no lexical pass found) must carry at least this share of the
    /// best bridge's score to be admitted. Below it, the walk is just leaking into the repo at
    /// large. **Deliberately strict**: pass 12 volunteers symbols the query never named, so it
    /// must volunteer only the ones it is sure of.
    pub const ADMISSION: f64 = 0.35;
}

/// How much graph to gather.
#[derive(Debug, Clone)]
pub struct FindOptions {
    /// How many roots to keep.
    pub search_limit: usize,
    /// How far to walk from each root.
    pub traversal_depth: u32,
    /// The hard ceiling on gathered nodes.
    pub max_nodes: usize,
    /// Below this, a candidate is noise.
    pub min_score: f64,
    /// Which kinds to consider.
    pub node_kinds: Vec<NodeKind>,
    /// An optional embedding of the query, supplied by a caller that can generate one (the MCP
    /// server under `semantic-search`). When present, semantic (vector) candidates join the lexical
    /// ones — the vocabulary-gap bridge. `None` (the default, and every lexical-only index) leaves
    /// scoring exactly as it was.
    pub query_vec: Option<Vec<f32>>,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self {
            search_limit: 3,
            traversal_depth: 1,
            max_nodes: 20,
            min_score: 0.3,
            node_kinds: HIGH_VALUE_NODE_KINDS.to_vec(),
            query_vec: None,
        }
    }
}

impl FindOptions {
    /// **What `explore` asks for — and it is NOT [`Default`].**
    ///
    /// TS has two option sets and they are ten times apart. `buildContext`'s defaults are
    /// `{maxNodes:20, searchLimit:3, traversalDepth:1, minScore:0.3}`; the **explore handler
    /// never uses them** — it passes `{searchLimit:8, traversalDepth:3, maxNodes:200,
    /// minScore:0.2}` explicitly (`maps/mcp-context.md:115` vs `:132`). We ported the defaults
    /// and then called `explore` through them, so the product ran the whole time on the option
    /// set TS reserves for its *other*, smaller surface.
    ///
    /// It is not a tuning difference; it is what made the question unanswerable. Asked *"how
    /// does an unresolved reference become a graph edge"* — **four** concepts — ranking got
    /// **three** roots to span them with, and a 20-node budget divided across those roots gives
    /// each one ~6 neighbors at depth 1. The chain the answer needs
    /// (`resolve_and_persist_batched → resolve_all → resolve_one → create_edges`) is four hops
    /// long and its middle is made of symbols no ranking would ever surface on their own. A
    /// depth-1 walk on a 20-node budget cannot reach it, so no downstream pass could have fixed
    /// this: the answer was never gathered.
    ///
    /// **This also retires a "measured" conclusion that was measuring the wrong thing.** Pass 13
    /// records that a 4th root was tried and reverted because it *thinned the other three* — true,
    /// and only because `max_nodes / roots` was `20/4`. At 200 the trade-off it describes does
    /// not exist. A measurement is only as good as the configuration it ran under.
    ///
    /// Output size is unaffected: what an agent receives is capped by [`crate::budgets`]
    /// (file-count tiered) and cut by `truncate_to_ceiling` — `max_nodes` governs how much graph
    /// is *considered*, not how many bytes are *sent*.
    pub fn explore() -> Self {
        Self {
            search_limit: 8,
            traversal_depth: 3,
            max_nodes: 200,
            min_score: 0.2,
            node_kinds: HIGH_VALUE_NODE_KINDS.to_vec(),
            query_vec: None,
        }
    }
}

/// A candidate, with the score the passes built up.
#[derive(Debug, Clone)]
pub struct ScoredNode {
    /// The node.
    pub node: Node,
    /// Its score after every pass that has run.
    pub score: f64,
    /// How many distinct query terms it matched (pass 4).
    pub term_hits: usize,
    /// Whether its name is distinctive enough to carry a low-confidence answer (pass 9).
    pub distinctive: bool,
}

/// The repo's busiest file, if one file dominates (pass 4's core-dir boost).
///
/// ⚠ **`GraphStore` has no primitive for this** — the spike's audit list did not include
/// TS's `getDominantFile()`, so it was not caught there. Pass 4's boost is therefore
/// implemented as a **pure function over this struct** and wired to `None` until the
/// primitive exists (an edge-count-per-file aggregate). TS itself guards the call with
/// `?.()` and a `try/catch` — *"scoring works without the boost"* — so `None` is the same
/// degradation TS takes on a SQL failure, not a new one. Reported.
#[derive(Debug, Clone)]
pub struct DominantFile {
    /// The file.
    pub file_path: String,
    /// Its internal edge count.
    pub edge_count: u64,
    /// The runner-up's.
    pub next_edge_count: u64,
}

/// A test/spec file — dampened, unless the query is itself about tests.
pub fn is_test_file(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    p.starts_with("test/")
        || p.starts_with("tests/")
        || p.starts_with("spec/")
        || p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("/spec/")
        || p.contains("__tests__")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_test.")
        || p.ends_with("_test.go")
        || p.ends_with("_spec.rb")
}

/// Favor shorter names: `AllocationService` (18) over `AllocationBalancingRoundMetrics` (31).
/// Core classes have concise names; test/helper classes are verbose. **Ported formula.**
pub fn brevity(name: &str, prefix: &str) -> f64 {
    let extra = name.len().saturating_sub(prefix.len()) as f64;
    (10.0 - extra / 3.0).max(0.0)
}

/// TitleCase a term: `REST` → `Rest`, `bulk` → `Bulk`.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// **Passes 1–4.** Returns candidates in insertion order with their scores.
pub async fn score_candidates<S: GraphStore>(
    qm: &QueryManager<S>,
    query: &str,
    opts: &FindOptions,
    dominant: Option<&DominantFile>,
) -> Result<Vec<ScoredNode>> {
    let terms = extract_search_terms(query);
    score_candidates_with_terms(qm, query, &terms, opts, dominant).await
}

/// [`score_candidates`], over a term list the caller has already built.
///
/// **The terms are a parameter because they are no longer a pure function of the query.**
/// [`derive_corpus_terms`] widens them with the codebase's own sub-words (`unresolved` ⇒
/// `resolve`), and those derived terms have to reach *every* lexical pass. The extraction
/// therefore happens once, above. It used to be re-derived independently here and in
/// [`find_relevant_context`] — which is exactly the shape of bug where one caller gets the
/// enriched list and the other quietly does not.
pub async fn score_candidates_with_terms<S: GraphStore>(
    qm: &QueryManager<S>,
    query: &str,
    terms: &[String],
    opts: &FindOptions,
    dominant: Option<&DominantFile>,
) -> Result<Vec<ScoredNode>> {
    // --- pass 1: the terms ---------------------------------------------------
    if terms.is_empty() {
        // A stopword-only query. **Empty is an ANSWER**, and the caller renders guidance.
        return Ok(Vec::new());
    }
    let terms: Vec<String> = terms.to_vec();

    // Insertion-ordered: the order reaches ranking, so never a HashMap.
    let mut by_id: IndexMap<String, ScoredNode> = IndexMap::new();

    // --- pass 2: exact names + co-location boost ------------------------------
    // (`find_by_exact_names` takes no kind filter — the store's signature differs from TS's,
    // so the kind gate is applied here instead. Same result, one fewer store parameter.)
    let exact: Vec<Node> = qm
        .store()
        .find_by_exact_names(&terms, opts.search_limit * 5)
        .await
        .map_err(selene_graph::GraphError::from)?
        .into_iter()
        .filter(|n| opts.node_kinds.contains(&n.kind))
        .collect();

    // How many DISTINCT query symbols matched inside each file? Two of them in one file is a
    // far stronger signal than two of them scattered — `scrapeLoop` + `run` in `scrape.go`.
    let mut names_per_file: IndexMap<String, Vec<String>> = IndexMap::new();
    for n in &exact {
        let names = names_per_file.entry(n.file_path.clone()).or_default();
        let lower = n.name.to_lowercase();
        if !names.contains(&lower) {
            names.push(lower);
        }
    }

    for n in exact {
        let distinct = names_per_file.get(&n.file_path).map_or(1, Vec::len);
        let boost = if distinct > 1 {
            (distinct - 1) as f64 * weights::CO_LOCATION
        } else {
            0.0
        };
        let distinctive = is_distinctive(&n.name, &terms);
        upsert(&mut by_id, n, boost, 1, distinctive);
    }

    // --- pass 3: TitleCase prefix over DEFINITION kinds -----------------------
    // "REST"/"bulk"/"allocation" usually mean `RestController`, `BulkRequest`,
    // `AllocationService` — not a node named exactly that.
    for term in &terms {
        let titled = title_case(term);
        if titled == *term {
            continue; // already TitleCase — pass 2 owns it
        }
        let hits = qm
            .store()
            .get_nodes_by_name_prefix(&titled, 30)
            .await
            .map_err(selene_graph::GraphError::from)?;

        let mut matched: Vec<(Node, f64)> = hits
            .into_iter()
            .filter(|n| DEFINITION_KINDS.contains(&n.kind))
            .filter(|n| n.name.to_lowercase().starts_with(&titled.to_lowercase()))
            .map(|n| {
                let score = weights::PREFIX_HIT + brevity(&n.name, &titled);
                (n, score)
            })
            .collect();

        matched.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (n, score) in matched.into_iter().take(opts.search_limit) {
            let distinctive = is_distinctive(&n.name, &terms);
            upsert(&mut by_id, n, score, 1, distinctive);
        }
    }

    // --- pass 4: per-term FTS + multi-term boost ------------------------------
    //
    // ⚠ **The FTS score is NORMALIZED before it enters scoring, and it must be.**
    //
    // `search_fts` returns `20·bm25(name) + 5·bm25(qualifiedName) + 1·bm25(docstring) +
    // 2·bm25(signature)` — a number that lands around **150** on this corpus. Every other
    // weight in this module is **5–30**. The FTS channel was therefore ~5× the scale of
    // everything it was supposed to be combined WITH, so it decided the ranking by itself and
    // every ported weight — co-location +20, prefix +15, compound +30 — was rounding error.
    //
    // That is how *"how does an unresolved reference become a graph edge"* returned
    // `graph_outcome`, an MCP error helper: it is the best BM25 hit for the bare word "graph"
    // (161.5), and `UnresolvedReference` — which matches TWO of the query's concepts — scored
    // 32. No downstream rerank can climb a 5× deficit, so none of them did.
    //
    // The weights were ported from TS. **The scale of the input they are weighed against was
    // not.** Normalizing the channel to [`weights::FTS_MAX`] is what makes the ported numbers
    // mean what they meant: a hit on ONE word, however strong, is now worth less than a name
    // that matches TWO concepts.
    let mut fts_hits: Vec<SearchCandidate> = Vec::new();
    for term in &terms {
        fts_hits.extend(
            qm.store()
                .search_fts(
                    std::slice::from_ref(term),
                    &opts.node_kinds,
                    &[],
                    opts.search_limit * 2,
                    0,
                )
                .await
                .map_err(selene_graph::GraphError::from)?,
        );
    }

    let max_raw = fts_hits
        .iter()
        .map(|c| c.raw_score)
        .fold(0.0f64, f64::max)
        .max(f64::EPSILON);

    for c in fts_hits {
        let distinctive = is_distinctive(&c.node.name, &terms);
        // The FIRST hit for a node contributes its (normalized) FTS score; each EXTRA term it
        // matches adds +5. A node matching "shard" + "search" + "request" beats one matching
        // only "execution", which is the whole reason this boost exists.
        let entry = by_id.get(&c.node.id).map(|e| e.term_hits);
        let bonus = match entry {
            Some(_) => weights::MULTI_TERM, // an extra term hit
            None => weights::FTS_MAX * (c.raw_score / max_raw),
        };
        upsert(&mut by_id, c.node, bonus, 1, distinctive);
    }

    // --- pass 4½: semantic candidates (the vocabulary-gap bridge) --------------
    // Only fires when the caller supplied a query embedding (MCP under `semantic-search`) AND the
    // index has vectors. It ADMITS candidates the lexical passes cannot reach — a query for
    // `keypress` finds a `keybinding` symbol it shares no token with — scored as a fraction of
    // `FTS_MAX` (normalized among the vector hits), so a strong lexical match still leads. On a
    // lexical-only index `query_vec` is `None` and this is a no-op: the tuned ranking is unchanged.
    if let Some(qvec) = &opts.query_vec
        && let Ok(vhits) = qm
            .store()
            .vector_search(qvec, &opts.node_kinds, &[], opts.search_limit * 4)
            .await
    {
        let vmax = vhits
            .iter()
            .map(|c| c.raw_score)
            .fold(0.0f64, f64::max)
            .max(f64::EPSILON);
        for c in vhits {
            let distinctive = is_distinctive(&c.node.name, &terms);
            let bonus = weights::FTS_MAX * weights::SEMANTIC_WEIGHT * (c.raw_score / vmax);
            upsert(&mut by_id, c.node, bonus, 1, distinctive);
        }
    }

    // --- pass 4¾: document sections — a DEDICATED, CAPPED admission slot -------
    // (doc-ingestion PRD §6.) Sections never compete with code in the shared
    // pool: one kind-scoped query, at most DOC_SECTION_SLOT survivors. The
    // weight is INTENT-GATED: on an ordinary code question sections score at
    // half FTS_MAX (docs enrich, never crowd — the anti-Read bet is on code
    // first); on a RATIONALE-shaped question ("why …", "where is … documented")
    // the document IS the answer, and the slot outranks a bare name hit.
    // Best-effort: an index with no sections (or a pre-wave-A store) skips this.
    {
        const DOC_SECTION_SLOT: usize = 2;
        let ql = query.to_lowercase();
        let rationale_intent = [
            "why ",
            "pourquoi",
            "rationale",
            "documented",
            "decision",
            "décision",
            "design doc",
            "adr",
        ]
        .iter()
        .any(|m| ql.contains(m));
        let doc_weight = if rationale_intent { 1.2 } else { 0.5 };
        let doc_kinds = [NodeKind::Section];
        let mut doc_hits: Vec<SearchCandidate> = Vec::new();
        for term in &terms {
            if let Ok(hits) = qm
                .store()
                .search_fts(
                    std::slice::from_ref(term),
                    &doc_kinds,
                    &[],
                    DOC_SECTION_SLOT * 2,
                    0,
                )
                .await
            {
                doc_hits.extend(hits);
            }
        }
        doc_hits.sort_by(|a, b| {
            b.raw_score
                .partial_cmp(&a.raw_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.node.id.cmp(&b.node.id))
        });
        doc_hits.dedup_by(|a, b| a.node.id == b.node.id);
        doc_hits.truncate(DOC_SECTION_SLOT);
        for c in doc_hits {
            let bonus = weights::FTS_MAX * doc_weight;
            upsert(&mut by_id, c.node, bonus, 1, rationale_intent);
        }
    }

    // --- pass 4b: test-file dampen -------------------------------------------
    // …unless the agent is asking ABOUT tests, in which case the test files are the answer.
    let q = query.to_lowercase();
    let asking_about_tests = q.contains("test") || q.contains("spec");
    if !asking_about_tests {
        for scored in by_id.values_mut() {
            if is_test_file(&scored.node.file_path) {
                scored.score *= weights::TEST_DAMPEN;
            }
        }
    }

    // --- pass 4c: dominant-file core-directory boost --------------------------
    apply_core_dir_boost(&mut by_id, dominant);

    // Deterministic order out: score desc, then a total tie-break so equal scores never
    // reorder between runs.
    let mut out: Vec<ScoredNode> = by_id.into_values().collect();
    sort_candidates(&mut out);
    Ok(out)
}

/// Pass 4c as a pure function — see [`DominantFile`] for why it takes the value rather than
/// querying for it.
pub fn apply_core_dir_boost(
    candidates: &mut IndexMap<String, ScoredNode>,
    dominant: Option<&DominantFile>,
) {
    let Some(d) = dominant else { return };
    if d.edge_count < weights::DOMINANCE_RATIO * d.next_edge_count {
        return; // not dominant enough — the boost would be noise
    }
    let Some(slash) = d.file_path.rfind('/') else {
        return;
    };
    let core_dir = &d.file_path[..=slash];

    for scored in candidates.values_mut() {
        if scored.node.file_path.starts_with(core_dir) {
            scored.score += weights::CORE_DIR;
        }
    }
}

/// **The tie-break is a contract**: score desc, then `(file_path, start_line, name)`. Equal
/// scores must never reorder between runs — the output is rendered, and a reordering diff is
/// indistinguishable from a ranking change.
pub fn sort_candidates(v: &mut [ScoredNode]) {
    v.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.node.file_path.cmp(&b.node.file_path))
            .then_with(|| a.node.start_line.cmp(&b.node.start_line))
            .then_with(|| a.node.name.cmp(&b.node.name))
    });
}

/// A name is distinctive when it is not simply one of the query's own terms — i.e. the graph
/// knew something the query did not. Pass 9 uses it to decide confidence.
fn is_distinctive(name: &str, terms: &[String]) -> bool {
    let lower = name.to_lowercase();
    lower.len() > 3 && !terms.iter().any(|t| t.to_lowercase() == lower)
}

fn upsert(
    by_id: &mut IndexMap<String, ScoredNode>,
    node: Node,
    add: f64,
    hits: usize,
    distinctive: bool,
) {
    match by_id.get_mut(&node.id) {
        Some(existing) => {
            existing.score += add;
            existing.term_hits += hits;
            existing.distinctive |= distinctive;
        }
        None => {
            by_id.insert(
                node.id.clone(),
                ScoredNode {
                    node,
                    score: add,
                    term_hits: hits,
                    distinctive,
                },
            );
        }
    }
}

// =============================================================================
// Passes 5–11 — `find_relevant_context`
// =============================================================================

/// How sure we are that the graph actually answered the question.
///
/// **`Low` is not an error.** It is the honest half of the product: when the graph cannot
/// answer, saying so — and saying what to do next — beats returning thin context that *looks*
/// like an answer. A confident wrong answer is the one failure mode an agent cannot detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Something matched two terms, or matched a name the query did not contain.
    High,
    /// ≥2 terms were asked for and nothing matched more than one of them, and no result had
    /// a name the query did not already contain. The graph is guessing.
    Low,
}

/// The gathered context, and how much to trust it.
#[derive(Debug, Clone)]
pub struct RelevantContext {
    /// The nodes and edges worth showing.
    pub subgraph: Subgraph,
    /// See [`Confidence`] — `Low` is guidance, never an error.
    pub confidence: Confidence,
    /// The roots the walk started from, best first.
    pub roots: Vec<ScoredNode>,
    /// The search terms this gather actually ran on — the query's words **plus** the corpus-derived
    /// stems pass 0 found (`unresolved` ⇒ `resolve`). Carried out so the Flow section can group
    /// them into the query's concepts without re-deriving them (pass 0 costs a store round-trip
    /// per term). See [`term_groups`].
    pub terms: Vec<String>,
}

/// **Passes 1–11.** The whole gather.
///
/// Returns an EMPTY context — never an `Err` — when nothing is relevant, when the query was
/// all stopwords, or when the project is not indexed. Those are answers, and the caller turns
/// them into guidance. An `Err` here becomes an `isError` at the MCP layer (and the rmcp
/// spike proved an escaping `?` becomes a JSON-RPC *transport* failure), and one `isError`
/// early makes an agent abandon the tool for the session.
pub async fn find_relevant_context<S: GraphStore>(
    qm: &QueryManager<S>,
    query: &str,
    opts: &FindOptions,
    dominant: Option<&DominantFile>,
    large_repo: bool,
) -> Result<RelevantContext> {
    let literal = extract_search_terms(query);

    // --- pass 0: corpus-derived terms -----------------------------------------
    //
    // **Runs FIRST, and it must.** It re-scores nothing — it *admits* candidates no other pass
    // can reach, because every other pass asks whether a symbol's name contains a query word,
    // and the answer to a "how does X become Y" question is routinely a symbol whose name
    // contains only a *stem* of one (`unresolved` ⇒ `resolve_one`). Run it any later and the
    // candidate set it exists to widen has already been scored, cut and ranked without it.
    //
    // Empty on most queries, and that is correct — see [`derive_corpus_terms`].
    // **Pass 0 is skipped on a large repo.** It does a prefix scan PER candidate substring PER
    // term — ~30-50 unindexed scans of the node table, 10 s on VS Code. Its job is stem widening
    // (`unresolved` ⇒ `resolve`), a quality nicety; on a large repo the FTS index (passes 1-4,
    // index-backed, 2.3 s) carries candidate generation, and paying 10 s for stem widening makes
    // `explore` unusable. Small repos are byte-identical — the gate is purely on size.
    let __t = std::time::Instant::now();
    let derived = if large_repo {
        Vec::new()
    } else {
        derive_corpus_terms(qm, &literal).await?
    };
    tracing::info!(target: "selene::explore", ms = __t.elapsed().as_millis(), large_repo, "  gather: pass0 derive_corpus_terms");
    let terms: Vec<String> = literal.iter().cloned().chain(derived).collect();

    // Passes 1–4.
    let __t = std::time::Instant::now();
    let scored = score_candidates_with_terms(qm, query, &terms, opts, dominant).await?;
    tracing::info!(target: "selene::explore", ms = __t.elapsed().as_millis(), "  gather: pass1-4 score_candidates");

    // --- passes 6 & 7: LIKE matches -------------------------------------------
    let __t = std::time::Instant::now();
    // **Passes 6-7 are skipped on a large repo.** `search_name_like` uses `CONTAINS`, which the
    // SurrealDB docs are explicit never uses an index — a full 349k-row substring scan per term,
    // 4.4 s. FTS (passes 1-4) already covers candidate generation index-backed.
    let like = if large_repo {
        Vec::new()
    } else {
        like_passes_pub(qm, &terms, opts).await?
    };
    tracing::info!(target: "selene::explore", ms = __t.elapsed().as_millis(), "  gather: pass6-7 LIKE (CONTAINS)");
    let mut by_id: IndexMap<String, ScoredNode> = IndexMap::new();
    for s in scored.into_iter().chain(like) {
        match by_id.get_mut(&s.node.id) {
            // The channels are alternatives, not additions: take the BEST score any of them
            // gave a node, exactly as the merge in pass 4 does.
            Some(existing) => {
                existing.score = existing.score.max(s.score);
                existing.term_hits = existing.term_hits.max(s.term_hits);
                existing.distinctive |= s.distinctive;
            }
            None => {
                by_id.insert(s.node.id.clone(), s);
            }
        }
    }
    let mut scored: Vec<ScoredNode> = by_id.into_values().collect();

    // --- pass 5: term-group co-occurrence rerank ------------------------------
    //
    // **Runs AFTER the merge, and it must.** It used to run before, and the merge above then
    // erased it: the rerank scales a noise candidate by ×0.6, the LIKE channel re-supplies the
    // SAME node at its un-penalized score, and `max()` restores exactly what the penalty just
    // removed. A rerank whose result is `max()`-ed against its own input is a no-op — which is
    // why the ported penalty appeared to do nothing when it was first restored.
    //
    // (TS reranks before its LIKE passes because in TS the LIKE channels *add* to a node's score.
    // Ours take the max. With a `max()` merge, the rescale has to come last, or it cannot survive
    // the merge. The deviation is in the ORDER; the weights are TS's.)
    rerank_by_term_groups(&mut scored, &terms);

    // --- pass 12: graph connectivity ------------------------------------------
    // BEFORE the sort and BEFORE the truncate, because it both re-scores what the lexical passes
    // found AND admits what they could not. Running it after the cut would let the cut throw the
    // answer away first.
    sort_candidates(&mut scored); // seed order = lexical rank
    let __t = std::time::Instant::now();
    apply_graph_connectivity(qm, &mut scored, &terms, opts).await?;
    tracing::info!(target: "selene::explore", ms = __t.elapsed().as_millis(), "  gather: pass12 graph connectivity");

    // --- pass 8: sort → slice → min-score → cap roots --------------------------
    sort_candidates(&mut scored);
    scored.retain(|s| s.score >= opts.min_score);

    // The candidate pool the ROOT PICK may reach into. TS slices to `searchLimit*3` here — nine
    // candidates — which is ample when you only ever take the top three off the front, and far
    // too tight the moment the pick has to *span the query's concepts*: the best `edge` symbol
    // for "how does an unresolved reference become a graph edge" sits around rank 8–10, and a
    // pool of 9 cannot reliably see it. The pool is widened; **the root count is unchanged**,
    // and so is the subgraph budget below it.
    let mut pool: Vec<ScoredNode> = scored
        .iter()
        .take(opts.search_limit * ROOT_POOL_MULTIPLE)
        .cloned()
        .collect();

    // **Pass 14 gets a WIDER universe than the root pool, and it must.** Measured: the root pool
    // is the top 80 by lexical score, and `resolve_and_persist_batched` — the answer to the
    // milestone question — scores ≈12 after pass 5's ×0.6 and ranks *below* it. The pass built to
    // rescue the symbol could not see the symbol. It reached instead for the nearest thing it
    // could see, the un-batched `resolve_and_persist` (≈15, rank <80), and seated that.
    //
    // Corroboration is preserved: this is still only what the lexical passes MATCHED (post
    // `min_score`), never the repository at large. Topology may re-order the query's own hits;
    // it may not volunteer a symbol the query never touched.
    let bridge_universe: Vec<ScoredNode> = scored.iter().take(BRIDGE_UNIVERSE).cloned().collect();

    scored.truncate(opts.search_limit * 3);

    // **A test may not be a ROOT.** Pass 12 already refuses to *seed* from test files, and the
    // reason it gives applies with more force here: a test named
    // `delete_file_cascades_nodes_edges_and_unresolved` lexically matches half of any query about
    // edges or unresolved refs — it is a sentence, not a symbol — and it is never the answer to
    // "how does this work". Measured: it took root 1 for *"how are edges created during
    // resolution"* and dragged the whole answer into the test suite, because a root is not merely
    // ranked, it *steers* — the flow seeds, the BFS, and the blast radius all start from it.
    //
    // The ×0.3 dampen is not enough on its own and cannot be: a long test name matches many terms,
    // so the compound bonus it earns outruns the penalty. The dampen keeps tests *rankable*; this
    // keeps them from *steering*. (TS gates the same way — `maps/mcp-context.md:119` — including
    // the escape below.)
    let q = query.to_lowercase();
    let asking_about_tests = q.contains("test") || q.contains("spec");
    if !asking_about_tests && pool.iter().any(|s| !is_test_file(&s.node.file_path)) {
        // …and never prune to nothing: if the honest answer really does live in the tests, an
        // empty root set would turn a found answer into a "nothing relevant" handoff.
        pool.retain(|s| !is_test_file(&s.node.file_path));
    }

    // --- pass 9: confidence ---------------------------------------------------
    let confidence = confidence_of(&scored, &terms);

    // --- pass 13: root diversity ----------------------------------------------
    //
    // ⚠ **The root count stays at `search_limit`.** Growing it to cover every concept a query
    // names was tried, measured, and reverted: a 4th root for "how does an unresolved reference
    // become a graph edge" pulled in a variable literally named `edge`, and because `max_nodes`
    // is divided across the roots (pass 11), the extra root *thinned the other three* — the Flow
    // section stopped rendering and a second probe lost a file it had been finding. A wider
    // answer is not a better one when the budget is fixed. See `relevance-report.md`.
    let mut roots: Vec<ScoredNode> = pick_diverse_roots(&pool, &terms, opts.search_limit);

    // --- pass 14: orchestrator reservation ------------------------------------
    // The lexical ranking cannot reach the answer to a flow question, and no re-weighting of it
    // can. See [`reserve_orchestrator_roots`] for the measurement.
    let __t = std::time::Instant::now();
    reserve_orchestrator_roots(qm, &bridge_universe, &terms, &mut roots, opts.search_limit).await?;
    tracing::info!(target: "selene::explore", ms = __t.elapsed().as_millis(), "  gather: pass14 orchestrator");

    // --- pass 14b: document reservation (doc-ingestion wave A) ----------------
    // Pass 4¾'s weight admits sections as CANDIDATES; on a rationale-shaped
    // question no weight makes one a ROOT past the stacked code passes — the
    // exact lesson pass 14 already recorded ("no re-weighting can"). Same cure:
    // the best-scored Section takes ONE root slot, behind root 1, replacing the
    // weakest — the root budget stays fixed (pass 11 divides `max_nodes` across
    // roots; an ADDED root thins the others, the failure pick_diverse_roots
    // documents).
    {
        let ql = query.to_lowercase();
        let rationale_intent = [
            "why ",
            "pourquoi",
            "rationale",
            "documented",
            "decision",
            "décision",
            "design doc",
            "adr",
        ]
        .iter()
        .any(|m| ql.contains(m));
        if rationale_intent
            && !roots.is_empty()
            && !roots.iter().any(|r| r.node.kind == NodeKind::Section)
            && let Some(best_doc) = pool
                .iter()
                .filter(|c| c.node.kind == NodeKind::Section)
                .max_by(|a, b| {
                    a.score
                        .partial_cmp(&b.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.node.id.cmp(&a.node.id))
                })
        {
            let slot = roots.len().saturating_sub(1).max(1).min(roots.len() - 1);
            if roots.len() == 1 {
                roots.push(best_doc.clone());
            } else {
                roots[slot] = best_doc.clone();
            }
        }
    }

    if roots.is_empty() {
        // NOT an error. "Nothing relevant" is an answer, and the caller renders guidance.
        return Ok(RelevantContext {
            subgraph: Subgraph {
                nodes: IndexMap::new(),
                edges: Vec::new(),
                roots: Vec::new(),
            },
            confidence,
            roots,
            terms,
        });
    }

    // --- pass 10: type-hierarchy expansion ------------------------------------
    let mut nodes: IndexMap<String, Node> = IndexMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    for r in &roots {
        nodes.insert(r.node.id.clone(), r.node.clone());
    }

    let hierarchy_budget = opts.max_nodes / 4;
    let mut added = 0usize;
    for r in &roots {
        if added >= hierarchy_budget {
            break;
        }
        if let Ok(sub) = qm.type_hierarchy(&r.node.id).await {
            for (id, n) in sub.nodes {
                if added >= hierarchy_budget {
                    break;
                }
                if !nodes.contains_key(&id) {
                    nodes.insert(id, n);
                    added += 1;
                }
            }
            edges.extend(sub.edges);
        }
    }

    // --- pass 11: BFS both directions, per root -------------------------------
    let per_root = (opts.max_nodes / roots.len()).max(1);
    for r in &roots {
        for entry in qm
            .callees(&r.node.id, opts.traversal_depth)
            .await
            .unwrap_or_default()
            .into_iter()
            .chain(
                qm.callers(&r.node.id, opts.traversal_depth)
                    .await
                    .unwrap_or_default(),
            )
            .take(per_root * 2)
        {
            nodes
                .entry(entry.node.id.clone())
                .or_insert_with(|| entry.node.clone());
            edges.push(entry.edge);
        }
    }

    // --- pass 11's trims ------------------------------------------------------
    let root_ids: Vec<String> = roots.iter().map(|r| r.node.id.clone()).collect();
    trim_nodes(&mut nodes, &root_ids, opts);

    // --- pass 11's edge recovery ----------------------------------------------
    // The BFS only recorded the edges it walked. Anything ALREADY in the gathered set that
    // is connected to something else in it is a relationship the agent should see — and it
    // would otherwise be invisible, which is a rendered graph with missing lines.
    let ids: Vec<String> = nodes.keys().cloned().collect();
    if let Ok(recovered) = qm
        .store()
        .edges_between(
            &ids,
            &[
                EdgeKind::Calls,
                EdgeKind::Extends,
                EdgeKind::Implements,
                EdgeKind::References,
                EdgeKind::Overrides,
            ],
        )
        .await
    {
        edges.extend(recovered);
    }

    // Deterministic, deduplicated edges.
    edges.sort_by(|a, b| {
        (&a.source, &a.target, a.kind.as_str()).cmp(&(&b.source, &b.target, b.kind.as_str()))
    });
    edges.dedup_by(|a, b| a.source == b.source && a.target == b.target && a.kind == b.kind);
    // …and only edges whose BOTH ends survived the trims (a dangling edge renders as a line
    // to nowhere).
    edges.retain(|e| nodes.contains_key(&e.source) && nodes.contains_key(&e.target));

    Ok(RelevantContext {
        subgraph: Subgraph {
            nodes,
            edges,
            roots: root_ids,
        },
        confidence,
        roots,
        terms,
    })
}

/// **Pass 12 — graph connectivity: which symbols BRIDGE the query's concepts.**
///
/// # This is the pass whose absence made the product answer confidently and wrongly
///
/// Passes 1–7 are **entirely lexical**: they match the query's *words* against symbol *names*.
/// That is fine when the agent names a symbol, and it is useless when it asks a question:
///
/// > *"how does an unresolved reference become a graph edge"*
///
/// The words are `unresolved`, `reference`, `graph`, `edge`. The symbols that answer it are
/// `resolve_and_persist_batched`, `create_edges`, `insert_edges` — and **not one of them
/// contains a single one of those words**. No lexical pass can ever find them. What the lexical
/// passes find is `graph_outcome`, an MCP *error helper*, because it contains "graph".
///
/// # A "how does X become Y" question is asking for the code that touches BOTH
///
/// That is the whole insight, and it is what this pass computes. The query's terms group into
/// **concepts** ([`term_groups`]). Each lexical seed carries the concepts its name matched:
/// `UnresolvedRef` carries *unresolved*, `Edge` carries *edge*. Walk out from every seed and
/// collect, per reached node, **the union of the concepts that reached it**. A node reached by
/// an *unresolved*-seed AND an *edge*-seed is, quite literally, the code where an unresolved
/// reference becomes a graph edge.
///
/// Measured on the real 328-file corpus, asked *"how are edges created during resolution"*,
/// the ≥2-concept bridges are exactly: `persist`, `resolve_and_persist_batched`,
/// `run_synthesis_with` — the three functions that *are* the answer, with no hubs and no noise.
///
/// # Why not TS's RWR mass directly
///
/// TS runs a random walk with restart (`computeGraphRelevance`, `maps/mcp-context.md` §6) —
/// but it uses the mass to rank **files**, gated by a term hit, as *one tier among several*,
/// and its symbol seeds are names the agent typed. Ported here as a symbol-level score, raw RWR
/// mass fails twice, and both failures were measured on the real corpus, not predicted:
///
/// 1. **Seeds hoard their own restart mass.** Every iteration teleports `α·p(n)` back to seed
///    `n`, so the top lexical hit necessarily ends the walk holding the most mass — and gets
///    the biggest "connectivity" boost. `claims_reference` went 149 → 209 and *stayed* rank 1.
///    The walk ratified the noise it was added to overrule.
/// 2. **Mass flows to the utility layer.** Whatever the seeds all call is, in any real
///    codebase, `clamp_i64` and `all_ddl`. Both landed in the top 3. Everything calls them —
///    which is exactly why being called by the seeds says nothing *about* the seeds.
///
/// Concept-bridging has neither failure by construction: a seed cannot bridge itself, and a
/// utility reached by twenty seeds that all carry the *same* concept bridges nothing. The
/// inverse-degree damp below is the belt to that suspenders.
pub async fn apply_graph_connectivity<S: GraphStore>(
    qm: &QueryManager<S>,
    candidates: &mut Vec<ScoredNode>,
    terms: &[String],
    opts: &FindOptions,
) -> Result<IndexMap<String, usize>> {
    if candidates.is_empty() {
        return Ok(IndexMap::new());
    }

    // Nothing to bridge unless the query names at least two distinct concepts.
    let groups = term_groups(terms);
    if groups.len() < 2 {
        return Ok(IndexMap::new());
    }

    /// Which of the query's concepts does this name (and its path) carry?
    fn concepts_of(name: &str, path: &str, groups: &[Vec<String>]) -> Vec<usize> {
        let hay = format!("{name} {path}").to_lowercase();
        groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.iter().any(|t| hay.contains(&t.to_lowercase())))
            .map(|(i, _)| i)
            .collect()
    }

    // --- the seeds ------------------------------------------------------------------------
    //
    // **Test files are not seeds.** A test named `class_constants_become_constant_nodes`
    // lexically matches half of any query about constants or nodes, and seeding from it
    // diffuses into the test suite — which is never the answer to "how does this work". (The
    // ×0.3 dampen keeps tests *rankable*; this keeps them from steering.)
    let seeds: Vec<(String, Vec<usize>)> = candidates
        .iter()
        .filter(|s| !is_test_file(&s.node.file_path))
        .take(rwr::MAX_SEEDS)
        .map(|s| {
            (
                s.node.id.clone(),
                concepts_of(&s.node.name, &s.node.file_path, &groups),
            )
        })
        .filter(|(_, c)| !c.is_empty())
        .collect();

    // Two seeds carrying two *different* concepts, or there is no bridge to look for.
    let distinct: IndexSet<usize> = seeds.iter().flat_map(|(_, c)| c.iter().copied()).collect();
    if seeds.len() < 2 || distinct.len() < 2 {
        return Ok(IndexMap::new());
    }

    // --- walk out, carrying each seed's concepts with it ------------------------------------
    //
    // Two hops. One is not enough: `create_edges` is one hop from `Edge` and *two* from
    // `UnresolvedRef`, and a pass that cannot see it is a pass that cannot answer the question.
    let mut bridged: IndexMap<String, IndexSet<usize>> = IndexMap::new();
    let mut known: IndexMap<String, Node> = IndexMap::new();

    for (seed_id, seed_concepts) in &seeds {
        let mut frontier: Vec<String> = vec![seed_id.clone()];
        let mut seen: IndexSet<String> = IndexSet::from_iter([seed_id.clone()]);

        for _hop in 0..rwr::HOPS {
            if frontier.is_empty() {
                break;
            }
            let (out, inc) = (
                qm.store()
                    .outgoing_batch(&frontier, RWR_EDGE_KINDS)
                    .await
                    .map_err(selene_graph::GraphError::from)?,
                qm.store()
                    .incoming_batch(&frontier, RWR_EDGE_KINDS)
                    .await
                    .map_err(selene_graph::GraphError::from)?,
            );

            let mut next: Vec<String> = Vec::new();
            for id in &frontier {
                let neighbors: Vec<_> = out
                    .get(id)
                    .into_iter()
                    .flatten()
                    .chain(inc.get(id).into_iter().flatten())
                    .collect();

                // **THE HUB GUARD. Do not route a concept THROUGH a node everything touches.**
                //
                // `UnresolvedRef` has 168 neighbors and `Edge` has 98 — they are the domain's
                // hub types, and they are (correctly) seeds. But expanding a second hop through
                // them pours the seed's concept over half the repository: every function that
                // so much as mentions an `UnresolvedRef` inherits the *unresolved* concept, and
                // then any of them that also brushes the *edge* side scores as a "bridge".
                //
                // Measured, that is exactly what happened — `key_of`, `as_str`, `name_tail`,
                // `normalize_posix`, `is_builtin_type` all scored 40–60 and crowded the real
                // answer out of the pool. They bridge nothing; they are simply *near* everything.
                //
                // A node's neighbors are always COLLECTED (a hub's neighbors are legitimate
                // one-hop bridges — `create_edges` is a neighbor of `Edge`). What a hub may not
                // do is serve as a *corridor* to a further hop. Concepts travel through
                // specifics, not through hubs.
                let is_corridor = neighbors.len() <= rwr::HUB_DEGREE;

                for entry in neighbors.into_iter().take(rwr::MAX_NEIGHBORS_PER_NODE) {
                    if !opts.node_kinds.contains(&entry.node.kind)
                        || is_test_file(&entry.node.file_path)
                    {
                        continue;
                    }
                    if is_corridor && seen.insert(entry.node.id.clone()) {
                        next.push(entry.node.id.clone());
                    }
                    known
                        .entry(entry.node.id.clone())
                        .or_insert_with(|| entry.node.clone());
                    bridged
                        .entry(entry.node.id.clone())
                        .or_default()
                        .extend(seed_concepts.iter().copied());
                }
            }
            frontier = next;
        }
    }

    // --- score: concepts bridged, damped by global degree ------------------------------------
    let reached: Vec<String> = bridged
        .iter()
        .filter(|(_, c)| c.len() >= 2)
        .map(|(id, _)| id.clone())
        .collect();
    if reached.is_empty() {
        return Ok(IndexMap::new());
    }

    // Inverse-degree damping. A node reached by many seeds is interesting **in proportion to
    // how rarely it is reached by everyone else** — `resolve_and_persist_batched` has a handful
    // of callers, `clamp_i64` has dozens.
    let (deg_out, deg_in) = (
        qm.store()
            .outgoing_batch(&reached, RWR_EDGE_KINDS)
            .await
            .map_err(selene_graph::GraphError::from)?,
        qm.store()
            .incoming_batch(&reached, RWR_EDGE_KINDS)
            .await
            .map_err(selene_graph::GraphError::from)?,
    );

    let total = groups.len() as f64;
    let gains: Vec<(String, f64)> = reached
        .iter()
        .map(|id| {
            let n = bridged[id].len() as f64;
            let degree =
                (deg_out.get(id).map_or(0, Vec::len) + deg_in.get(id).map_or(0, Vec::len)) as f64;
            let gain = (n / total) / (std::f64::consts::E + degree).ln();
            (id.clone(), gain)
        })
        .collect();

    let max_gain = gains.iter().map(|(_, g)| *g).fold(0.0f64, f64::max);
    if max_gain <= 0.0 {
        return Ok(IndexMap::new());
    }

    // --- apply: boost what we had, ADMIT what the graph found --------------------------------
    let existing: IndexMap<String, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.node.id.clone(), i))
        .collect();

    for (id, gain) in gains {
        let normalized = gain / max_gain;
        match existing.get(&id) {
            Some(&i) => candidates[i].score += weights::CONNECTIVITY * normalized,
            None => {
                // A node NO lexical pass found. Admitting it is the entire point of this pass:
                // it is the symbol the query did not contain and the answer needs.
                if normalized < rwr::ADMISSION {
                    continue;
                }
                let Some(node) = known.get(&id) else { continue };

                // **Only callables are admitted.** A graph-discovered node earns its place by
                // being a step in the flow the agent asked about — and a constant cannot be a
                // step. Without this, the inverse-degree damp (which rewards *low* degree)
                // promotes exactly the wrong thing: asked how edges are created, the pass
                // returned `RESOLVE_BATCH` and `INSERT_CHUNK` — two `usize` chunk-size consts
                // that sit next to the answer and explain none of it. They rank high precisely
                // *because* nothing calls them.
                //
                // TS applies the same gate when it seeds from named symbols
                // (CALLABLE = {method, function, component, constructor} — `maps/mcp-context.md`
                // §3). A lexical hit still ranks on its own merits whatever its kind; this gate
                // governs only what the GRAPH is allowed to volunteer.
                if !CALLABLE_KINDS.contains(&node.kind) {
                    continue;
                }

                candidates.push(ScoredNode {
                    node: node.clone(),
                    score: weights::CONNECTIVITY * normalized,
                    term_hits: 0,
                    // The graph knew a name the query did not — that is the definition of
                    // distinctive, and pass 9 should read it as confidence, not as a guess.
                    distinctive: true,
                });
            }
        }
    }

    // **The verdict, handed to pass 5.** Which of the query's concepts each symbol was PROVEN to
    // touch — not by how it is spelled, but by what it calls and what calls it. Pass 5 reads this
    // as evidence of the same kind as a name match, and it is the only reason
    // `resolve_and_persist_batched` can outrank a symbol that merely contains two of the query's
    // words. Only ≥2-concept bridges are reported; a single concept is not a bridge.
    Ok(reached
        .iter()
        .map(|id| (id.clone(), bridged[id].len()))
        .collect())
}

/// How many of the `search_limit` root slots pass 14 may take for orchestrators.
///
/// **Two, and it replaces rather than appends.** The root budget is fixed for a reason recorded
/// in [`pick_diverse_roots`]: `max_nodes` is divided across the roots (pass 11), so a root added
/// is a root *thinned*. These take the two weakest slots.
const BRIDGE_ROOTS: usize = 2;

/// How deep into the lexically-matched candidates pass 14 may look for an orchestrator.
///
/// Wider than the root pool (80) on purpose — the answer to a flow question is *penalized* by
/// pass 5 for spelling only one of the query's words, so it sits well below the types that spell
/// two. Bounded all the same: one `outgoing_batch` over this many ids, and only over candidates
/// the query's own words already matched.
const BRIDGE_UNIVERSE: usize = 500;

/// Which of the query's concepts does this NAME spell?
///
/// **The name only — never the file path.** Pass 5 keys its multiplier on `name + file_path`,
/// and that is the accident by which `create_edges` earns a second concept: it lives under
/// `crates/selene-resolve/`, so its path spells *resolve*. A path records where a symbol was
/// filed, not what it does. `insert_edges` does the same job from `selene-db/` and gets no such
/// gift. Scoring on the path rewards the directory layout.
fn concepts_in_name(name: &str, groups: &[Vec<String>]) -> std::collections::BTreeSet<usize> {
    let lower = name.to_lowercase();
    groups
        .iter()
        .enumerate()
        .filter(|(_, g)| g.iter().any(|t| name_carries(&lower, t)))
        .map(|(i, _)| i)
        .collect()
}

/// Does this (already-lowercased) name carry this term — **folding a plural query word onto the
/// singular the code declares**?
///
/// The agent writes English and the code declares types. Asked *"how are **edges** created"*, the
/// type is `Edge`, and `"edge".contains("edges")` is **false** — so the one symbol the question is
/// named after carried none of its concepts, and the flow could not arrive at it. Measured: the
/// spine for that query walked eight hops into the matcher and ended on `Language`.
///
/// TS folds the same way (`segmentLookupVariants`, `maps/db-graph-search.md:77`: *"bare `-s` not
/// `-ss` → strip1"*). The `-ss` guard is what keeps `class`/`process` from being read as plurals.
pub(crate) fn name_carries(lower_name: &str, term: &str) -> bool {
    let t = term.to_lowercase();
    if lower_name.contains(&t) {
        return true;
    }
    match t.strip_suffix('s') {
        Some(sing) if !t.ends_with("ss") && sing.len() >= 3 => lower_name.contains(sing),
        _ => false,
    }
}

/// **Pass 14 — orchestrator reservation. The one pass that answers a flow question.**
///
/// A question of the shape *"how does X become Y"* names its endpoints as **types**
/// (`UnresolvedReference`, `Edge`). The lexical passes duly find those types — and that is not
/// the bug, it is the correct answer to *"what does this question name"*. The bug is that we
/// then made the types the ROOTS. You cannot call a type, so no chain can run between them, and
/// the four functions that *are* the answer sit in the pool unreachable:
///
/// | symbol | score | why |
/// |---|---|---|
/// | `ReferenceResolver` | ≈143 | spells *reference* + *resolve* → pass 5 ×2.0 |
/// | `UnresolvedReference` | ≈141 | spells both → ×2.0 |
/// | `resolve_one` | ≈18 | spells one → ×0.6 |
/// | `resolve_and_persist_batched` | ≈12 | spells one → ×0.6 |
///
/// **No re-weighting closes a 12-to-143 gap**, and two attempts to make pass 5 smarter were
/// measured and reverted (see [`term_groups`]). Pass 12's channel is additive and capped at
/// `CONNECTIVITY = 30` — a correct verdict there still cannot climb.
///
/// # What the answer actually looks like in the graph
///
/// ```text
/// resolve_and_persist_batched --calls--> resolve_all -> resolve_one    (concept: resolve)
///                             --calls--> create_edges, insert_edges    (concept: edge)
/// ```
///
/// It is not *on a path between* the two poles — `resolve_one` and `create_edges` are siblings
/// under it, not caller and callee. It is the function whose **own outgoing calls span both of
/// the query's concepts**. It orchestrates. That is what a flow question is asking for.
///
/// # Why this is not the pass-12 mistake, and the control that proves it
///
/// The previous attempt let graph evidence amplify pass 5 and it promoted `file_node_id`,
/// `hash_content`, `node_id` — "the utility layer touches every concept, so a reward for
/// touching two is a reward for being plumbing". That verdict was right, and it is a verdict
/// about **undirected** evidence. Pass 12 scores `deg_out + deg_in` and walks both ways, so it
/// cannot tell a driver from a utility.
///
/// Direction separates them, and it separates them completely. Measured over all 1 460 non-test
/// callables in this repo, ranked by concepts spanned through **outgoing calls**:
///
/// ```text
///   #2  resolve_and_persist_batched  {EDGE, RESOLVE}  out=29 in=6     <- the answer
///   #3  resolve_one                  {REFER,RESOLVE}  out=24 in=27
///
///   control — the same repo ranked by RAW DEGREE (what an undirected score rewards):
///       collect  out=15 in=527 · get_node_text out=0 in=205 · as_str out=0 in=177
///       get_child_by_field out=0 in=202 · default out=0 in=95
/// ```
///
/// Every symbol that sank the previous attempt has **`out=0` and a huge `in`**. A utility is
/// *called by* everything; an orchestrator *calls* everything. Restricting the span to outgoing
/// calls does not down-weight plumbing — it makes plumbing **structurally ineligible**, because
/// a function that calls nothing spans nothing. No damping term required.
///
/// It is also immune to the failure recorded in [`pick_diverse_roots`] (reserving a root per
/// concept promoted a local variable literally named `edge`): a variable is not callable and has
/// no outgoing calls, so it can never be a bridge.
///
/// # Topology still does not win on its own
///
/// A bridge must span ≥2 concepts that the query's own words matched **by name**, it must be
/// callable, non-test, and already in the lexical pool — and it may take only [`BRIDGE_ROOTS`]
/// of the slots. That is the corroboration discipline TS holds everywhere ("central" requires a
/// term hit; the relevance gate requires mass OR ≥2 term hits). The force-keep channel itself is
/// TS's too — its change-surface rescue force-keeps what ranking would bury
/// (`maps/mcp-context.md` §7).
async fn reserve_orchestrator_roots<S: GraphStore>(
    qm: &QueryManager<S>,
    pool: &[ScoredNode],
    terms: &[String],
    roots: &mut Vec<ScoredNode>,
    search_limit: usize,
) -> Result<()> {
    let groups = term_groups(terms);
    if groups.len() < 2 || roots.is_empty() {
        return Ok(()); // nothing to bridge between
    }

    // Only a callable can orchestrate. (And a test never explains how the product works —
    // same reason the root pool excludes them above.)
    let cands: Vec<&ScoredNode> = pool
        .iter()
        .filter(|s| CALLABLE_KINDS.contains(&s.node.kind) && !is_test_file(&s.node.file_path))
        .collect();
    if cands.is_empty() {
        return Ok(());
    }

    let ids: Vec<String> = cands.iter().map(|s| s.node.id.clone()).collect();
    let out = qm
        .store()
        .outgoing_batch(&ids, &[EdgeKind::Calls])
        .await
        .map_err(selene_graph::GraphError::from)?;

    // `bearers` = how many of its callees carry a concept. It breaks ties toward the function
    // that actually drives the concept-bearing work, not one that merely mentions it once.
    let mut bridges: Vec<(usize, usize, f64, String, ScoredNode)> = Vec::new();
    for s in &cands {
        let mut spans = concepts_in_name(&s.node.name, &groups);
        let mut bearers = 0usize;
        for e in out.get(&s.node.id).map(Vec::as_slice).unwrap_or(&[]) {
            let c = concepts_in_name(&e.node.name, &groups);
            if !c.is_empty() {
                bearers += 1;
                spans.extend(c);
            }
        }
        if spans.len() >= 2 {
            bridges.push((
                spans.len(),
                bearers,
                s.score,
                s.node.name.clone(),
                (*s).clone(),
            ));
        }
    }
    if bridges.is_empty() {
        return Ok(());
    }

    // Deterministic: concepts, then concept-bearing callees, then lexical score, then name.
    bridges.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.3.cmp(&b.3))
    });

    let winners: Vec<ScoredNode> = bridges
        .into_iter()
        .map(|b| b.4)
        .filter(|w| {
            !roots
                .iter()
                .any(|r| r.node.name.eq_ignore_ascii_case(&w.node.name))
        })
        .take(BRIDGE_ROOTS)
        .collect();

    // **Seat them where a root STEERS.** They were first seated at the tail, to disturb the
    // lexical ranking as little as possible. Measured: `resolve_and_persist_batched` became
    // root 7 of 8 — and `batch.rs` was *still* not rendered. The file sections never reach that
    // far down. A root that does not steer is not a root; it is a name in a list.
    //
    // So: root 1 stays whatever ranking said it was — the query's strongest literal match is
    // never displaced — and the orchestrators take the slots directly behind it. The weakest
    // lexical roots fall off the end, which keeps the budget fixed (pass 11 divides `max_nodes`
    // across the roots, so an *added* root thins every other one — the failure recorded in
    // [`pick_diverse_roots`]).
    for (i, w) in winners.into_iter().enumerate() {
        let at = (1 + i).min(roots.len());
        roots.insert(at, w);
    }
    roots.truncate(search_limit);
    Ok(())
}

/// **Pass 13 — root diversity.** Pick `limit` roots, **never two with the same name**.
///
/// The graph legitimately holds three nodes named `insert_edges` — a trait declaration, its
/// impl, and a delegating impl. Ranking scored all three, and the roots came back as
/// *`insert_edges`, `insert_edges`, `insert_edges`*: the whole root budget spent on three
/// spellings of one symbol, and with it every downstream section — the flow seeds, the file
/// sections, the blast radius — collapsed onto a single point.
///
/// One name, one root. The rest of the budget goes to symbols that say something new.
///
/// # What this pass deliberately does NOT do: force the roots to span the query's concepts
///
/// It is a tempting idea, it was implemented, and **the measurements killed it**. Asked *"how
/// does an unresolved reference become a graph edge"*, all three roots land on the *unresolved*
/// pole and none on *edge* — so the answer explains where the reference came from and never
/// reaches the edge. Reserving a root for each concept looks like the obvious fix.
///
/// It is not, for a reason worth writing down: **the best candidate for a concept is not
/// necessarily a good candidate.** Forcing coverage of `edge` promoted a local variable
/// literally named `edge` over a strong, well-connected symbol — and because `max_nodes` is
/// divided across the roots (pass 11), that junk root also *thinned the good ones*. Two probes
/// that had been rendering a Flow section stopped. A root spent on a concept is a root not
/// spent on an answer.
///
/// Ranking already knows which candidates are strong. Diversity should only stop it repeating
/// itself — not overrule it. See `relevance-report.md` for the before/after that settled this.
pub fn pick_diverse_roots(
    scored: &[ScoredNode],
    _terms: &[String],
    limit: usize,
) -> Vec<ScoredNode> {
    let mut seen: Vec<String> = Vec::new();
    let mut roots: Vec<ScoredNode> = Vec::new();

    for s in scored {
        if roots.len() >= limit {
            break;
        }
        let name = s.node.name.to_lowercase();
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        roots.push(s.clone());
    }
    roots
}

/// **Pass 5 — term-group co-occurrence rerank. MULTIPLICATIVE, and it PENALIZES.**
///
/// Group query terms that are stem variants of one another (`indexed`, `index`) so they count
/// as ONE concept, then rescale every candidate by how many distinct concepts it matched.
///
/// # The penalty is the whole pass, and the port dropped it
///
/// This was ported as `score += (concepts - 1) * 5` — additive, and with **no penalty
/// branch**. TS is `×(1+0.5n)` for ≥2 groups, `×0.3` for a common-word exact match, and
/// **`×0.6` for everything else** (`maps/mcp-context.md` §`ContextBuilder.findRelevantContext`).
///
/// The dropped `×0.6` is not a rounding difference — it is the mechanism. Asked *"how does an
/// unresolved reference become a graph edge"*, the graph returned `graph_outcome` (an MCP error
/// helper) as its top hit, because it contains the word "graph". It matches **one** concept out
/// of four. `UnresolvedReference` matches **two** ("unresolved" + "reference") and ranked below
/// it. With the ported rerank, the one-concept noise is scaled ×0.6 and the two-concept symbol
/// ×2.0 — a 3.3× swing, and the right answer wins.
///
/// A `+5` cannot express "this candidate is probably noise". A `×0.6` can. That is why TS
/// multiplies.
fn rerank_by_term_groups(scored: &mut [ScoredNode], terms: &[String]) {
    if terms.len() < 2 {
        return;
    }
    let groups = term_groups(terms);
    if groups.len() < 2 {
        return;
    }

    for s in scored.iter_mut() {
        let hay = format!("{} {}", s.node.name, s.node.file_path).to_lowercase();
        let lexical = groups
            .iter()
            .filter(|g| g.iter().any(|t| hay.contains(&t.to_lowercase())))
            .count();

        // ⚠ **Pass 12's bridged-concept verdict is DELIBERATELY NOT used here, and it was
        // measured.** It is a tempting idea — a symbol that *reaches* the `edge` concept by
        // calling `create_edges` arguably matches it as truly as one that merely spells it — and
        // feeding `max(lexical, bridged)` into this multiplier does raise the right symbol.
        //
        // It also raises `file_node_id`, `hash_content` and `node_id` into the top roots for
        // *"how are edges created during resolution"*, and took that query from 3-of-3 to 0-of-3.
        // The reason is the one this file already records twice (`rwr`'s hub guard, pass 12's
        // callable gate): **the utility layer touches every concept**, so a *multiplicative*
        // reward for touching two of them is a reward for being plumbing. Pass 12's additive,
        // degree-damped boost is bounded and survives that; a ×2 rescale does not.
        //
        // Graph evidence belongs in pass 12's own bounded channel. It must not be allowed to
        // multiply.
        let concepts = lexical;

        let lower = s.node.name.to_lowercase();
        let exact = terms.iter().any(|t| t.to_lowercase() == lower);

        if concepts >= 2 {
            s.score *= 1.0 + weights::GROUP_SCALE * concepts as f64;
            s.term_hits = s.term_hits.max(concepts);
        } else if exact {
            // **An exact name match is EXEMPT from the penalty.**
            //
            // TS splits this branch: a *distinctive* exact match is exempt, a *common-word*
            // exact match is ×0.3. We first ported that split with a word-shape test — is the
            // name an identifier (`handle_login`) or an English word (`edge`)? — and it was
            // measurably wrong on the very query it was meant to help.
            //
            // Asked *"how does an unresolved reference become a graph **edge**"*, the shape
            // test read `Edge` as a common word and scaled it 25 → 7.5. But `Edge` is the
            // central domain type of a graph database: it is not a word that happens to
            // collide with the query, it is *the thing the query is about*. The penalty
            // dropped it out of the candidate pool entirely, and with it went the entire
            // `edge` half of the question.
            //
            // The stopword list is already the defense against common words — anything
            // meaningless was filtered out before it ever became a term. A word that survives
            // stopwords AND exactly names a symbol is the strongest signal the query has, and
            // it must not be second-guessed by a heuristic about its spelling.
        } else {
            // One concept out of several. Not wrong — just not corroborated. This is the
            // branch the port dropped.
            s.score *= weights::SINGLE_CONCEPT;
        }
    }
}

/// Terms that are substrings of one another are one concept. Longest first, so the longest
/// term names the group.
///
/// # ⚠ Two ways to make this smarter were implemented, MEASURED, and reverted
///
/// Both target the same real defect: *"unresolved reference"* is one noun phrase, but this
/// function's rule is **spelling**, so it yields two concepts. `resolve_rust_path_reference`
/// therefore matches "two of the query's ideas" and is scaled ×2.0 by
/// [`rerank_by_term_groups`], while `resolve_and_persist_batched` — the function that *is* the
/// answer — matches one and is scaled **×0.6**. We reward a symbol for having spelled out the
/// two words the agent happened to write side by side. The defect is real. Both fixes were worse.
///
/// 1. **Co-occurrence merging** (TS's `getSegmentCoOccurrence`, `maps/db-graph-search.md:43`):
///    merge two concepts when ≥2 real symbol names contain a word from each — the codebase has a
///    type called `UnresolvedReference`, which is the domain itself saying the two words name one
///    thing. Implemented against the real corpus. It merges correctly, and it **flattens the
///    query**: with *unresolved* and *reference* fused, nothing matches two concepts any more, so
///    every candidate takes the ×0.6 and ranking falls back to raw lexical strength — where short
///    names win on brevity. The gate question went from 2-of-3 to **0-of-3**, its roots replaced
///    wholesale by `synth_edge`, `edge`, `create_edges`. Correcting the *penalty* is not the same
///    as promoting the *answer*.
///
/// 2. **Letting pass 12's graph verdict count as a concept match** — see the note in
///    [`rerank_by_term_groups`]. It promotes the utility layer, because the utility layer touches
///    every concept.
///
/// The lesson both share, and it is worth stating once: **this multiplier is the wrong instrument
/// for the last mile.** ×2.0-vs-×0.6 is a 3.3× swing applied to a score that already spans 10×,
/// so anything fed into it either does nothing or overturns the ranking entirely. The remaining
/// gap is not a weighting problem. See the module docs.
pub fn term_groups(terms: &[String]) -> Vec<Vec<String>> {
    let mut sorted: Vec<&String> = terms.iter().collect();
    sorted.sort_by_key(|t| std::cmp::Reverse(t.len()));

    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut assigned: Vec<&String> = Vec::new();

    for t in &sorted {
        if assigned.contains(t) {
            continue;
        }
        let mut group = vec![(*t).clone()];
        assigned.push(t);
        for other in &sorted {
            if assigned.contains(other) {
                continue;
            }
            let (a, b) = (t.to_lowercase(), other.to_lowercase());
            if a.contains(&b) || b.contains(&a) {
                group.push((*other).clone());
                assigned.push(other);
            }
        }
        groups.push(group);
    }
    groups
}

/// **Passes 6 & 7.** camelCase-boundary and compound (≥2-term) LIKE matches — the names FTS
/// cannot see, because FTS tokenizes and `handleLoginRequest` is one token.
pub async fn like_passes_pub<S: GraphStore>(
    qm: &QueryManager<S>,
    terms: &[String],
    opts: &FindOptions,
) -> Result<Vec<ScoredNode>> {
    let mut out: Vec<ScoredNode> = Vec::new();

    // Pass 6: each term as a substring — catches the camelCase boundary (`login` inside
    // `handleLoginRequest`).
    //
    // ⚠ The store's `search_name_like` `raw_score` is a **tier**, not a score: 1.0 exact / 0.9
    // prefix / 0.8 contains / 0.7 qualified / 0.5 otherwise. The port used it *as the whole
    // score*, so a camelCase-boundary match was worth **0.8 points** against an FTS hit worth
    // 150 — the channel was, numerically, switched off. TS's pass 6 is `8 + brevity + pathScore`
    // (`maps/mcp-context.md`); the tier belongs as a MULTIPLIER on that base, which is what
    // keeps exact > prefix > contains while putting the channel back on the shared scale.
    for term in terms {
        let hits = qm
            .store()
            .search_name_like(term, &opts.node_kinds, weights::LIKE_LIMIT)
            .await
            .map_err(selene_graph::GraphError::from)?;
        for c in hits {
            let distinctive = is_distinctive(&c.node.name, terms);
            let base = (weights::LIKE_BASE + brevity(&c.node.name, term)) * c.raw_score;
            out.push(ScoredNode {
                score: base,
                node: c.node,
                term_hits: 1,
                distinctive,
            });
        }
    }

    // Passes 6 & 7's scaling. **Ported** (`maps/mcp-context.md`):
    //   pass 6 (camelCase LIKE): `base ×(1+termCount) + 30(termCount−1)`
    //   pass 7 (compound ≥2):    `10 + 20(terms−1) + brevity`
    //
    // Both were ported as a flat `+5 per extra term`, which is what let a name matching ONE
    // query word rank alongside a name matching THREE. The whole point of a compound hit is
    // that it is *categorically* stronger — `20` per extra term, not `5`.
    // **Count CONCEPTS, not terms.** This counted raw terms, which was harmless while every term
    // was a distinct word the agent typed — and became a double-count the moment pass 0 started
    // deriving terms *from* other terms. `UnresolvedReference` contains `unresolved` AND its
    // derivation `resolve`, so it scored as a two-concept compound hit for what is manifestly one
    // idea, and climbed 141 → 157 for free. Grouping first is what pass 5 and pass 12 already do;
    // this pass was the odd one out.
    let groups = term_groups(terms);
    for s in out.iter_mut() {
        let name = s.node.name.to_lowercase();
        let hits = groups
            .iter()
            .filter(|g| g.iter().any(|t| name.contains(&t.to_lowercase())))
            .count();
        if hits == 0 {
            continue;
        }

        // Pass 6.
        s.score = s.score * (1.0 + hits as f64) + weights::LIKE_MULTI_TERM * (hits - 1) as f64;

        // Pass 7: a name containing TWO OR MORE terms is a compound hit —
        // `ShardSearchRequest` for "shard search request", `UnresolvedReference` for
        // "unresolved reference" — and it must outrank any one-term match.
        if hits >= 2 {
            let compound = weights::COMPOUND_BASE
                + weights::COMPOUND_PER_EXTRA_TERM * (hits - 1) as f64
                + brevity(&s.node.name, "");
            s.score = s.score.max(compound);
            s.term_hits = s.term_hits.max(hits);
        }
    }

    Ok(out)
}

/// **Pass 9.** `Low` **iff** the query named ≥2 terms **and** nothing matched more than one of
/// them **and** nothing has a name the query did not already contain.
///
/// The second half is what makes it useful: if the graph hands back a symbol the agent did
/// not name, the graph knew something. If every hit is just the words the agent typed, it
/// did not.
pub fn confidence_of(scored: &[ScoredNode], terms: &[String]) -> Confidence {
    if terms.len() < 2 {
        return Confidence::High;
    }
    let strong = scored.iter().any(|s| s.term_hits >= 2 || s.distinctive);
    if strong {
        Confidence::High
    } else {
        Confidence::Low
    }
}

/// Pass 11's four trims, in order. **Roots are never trimmed** — they are the answer.
fn trim_nodes(nodes: &mut IndexMap<String, Node>, roots: &[String], opts: &FindOptions) {
    // 1. per-file cap: max(5, ceil(max_nodes * 0.2)). One file must not eat the budget.
    let per_file = std::cmp::max(5, (opts.max_nodes as f64 * 0.2).ceil() as usize);
    // 2. non-production cap: max(3, ceil(max_nodes * 0.15)). Tests/generated code are real,
    //    but they are not the answer.
    let non_prod = std::cmp::max(3, (opts.max_nodes as f64 * 0.15).ceil() as usize);

    let mut per_file_count: IndexMap<String, usize> = IndexMap::new();
    let mut non_prod_count = 0usize;
    let mut keep: IndexMap<String, Node> = IndexMap::new();

    for (id, node) in nodes.iter() {
        let is_root = roots.contains(id);

        if !is_root {
            let count = per_file_count.entry(node.file_path.clone()).or_insert(0);
            if *count >= per_file {
                continue;
            }
            if is_test_file(&node.file_path) {
                if non_prod_count >= non_prod {
                    continue;
                }
                non_prod_count += 1;
            }
            *count += 1;
        }

        keep.insert(id.clone(), node.clone());
        // 3. the hard cap — but never at the cost of a root.
        if keep.len() >= opts.max_nodes
            && keep.keys().filter(|k| roots.contains(k)).count() == roots.len()
        {
            break;
        }
    }

    // 4. every root survives, whatever the caps said.
    for id in roots {
        if let Some(n) = nodes.get(id) {
            keep.entry(id.clone()).or_insert_with(|| n.clone());
        }
    }

    *nodes = keep;
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_core::Language;

    /// The agent writes English; the code declares types. `"edge".contains("edges")` is false, so
    /// asked *"how are **edges** created"* the `Edge` type carried none of the query's concepts —
    /// and the flow could not arrive at the one symbol the question is named after.
    #[test]
    fn a_plural_query_word_carries_onto_the_singular_the_code_declares() {
        assert!(
            name_carries("edge", "edges"),
            "`Edge` is what `edges` means"
        );
        assert!(
            name_carries("insert_edges", "edges"),
            "the exact form still matches"
        );
        assert!(name_carries("unresolvedref", "refs"));

        // …and the `-ss` guard, or `class` reads as a plural of `clas`.
        assert!(!name_carries("clas", "class"));
        assert!(!name_carries("proces", "process"));
        // A 2-char stem is a spelling accident, not a word.
        assert!(!name_carries("i", "is"));
        // Still no false friends.
        assert!(!name_carries("resolve_one", "edges"));
    }

    /// Pass 14's discriminator. A utility is *called by* everything; an orchestrator *calls*
    /// everything. Only the second spans the query's concepts through its own out-edges.
    #[test]
    fn concepts_are_read_from_the_name_never_the_path() {
        let groups = vec![vec!["resolve".to_string()], vec!["edge".to_string()]];
        // `create_edges` lives under `crates/selene-resolve/` — pass 5 hands it a free `resolve`
        // concept for that accident of filing. Pass 14 must not repeat the mistake.
        assert_eq!(
            concepts_in_name("create_edges", &groups),
            [1].into_iter().collect()
        );
        assert_eq!(
            concepts_in_name("resolve_and_persist_batched", &groups),
            [0].into_iter().collect()
        );
        // A name that genuinely spells both spans both.
        assert_eq!(
            concepts_in_name("resolve_edge_refs", &groups),
            [0, 1].into_iter().collect()
        );
    }

    #[test]
    fn brevity_favors_the_concise_name() {
        // `AllocationService` (17) over `AllocationBalancingRoundMetrics` (31), prefix
        // `Allocation` (10).
        let short = brevity("AllocationService", "Allocation");
        let long = brevity("AllocationBalancingRoundMetrics", "Allocation");
        assert!(short > long);
        assert_eq!(short, 10.0 - 7.0 / 3.0);
        assert_eq!(long, f64::max(10.0 - 21.0 / 3.0, 0.0));
    }

    #[test]
    fn brevity_never_goes_negative() {
        assert_eq!(brevity(&"x".repeat(200), "x"), 0.0);
    }

    #[test]
    fn test_files_are_recognized_across_ecosystems() {
        for p in [
            "src/__tests__/a.ts",
            "src/a.test.ts",
            "spec/models/user_spec.rb",
            "internal/api_test.go",
            "tests/test_thing.py",
        ] {
            assert!(is_test_file(p), "{p}");
        }
        assert!(!is_test_file("src/services/auth.ts"));
    }

    /// The dominance rule is a **ratio**, not a rank: being the busiest file is not enough.
    #[test]
    fn the_core_dir_boost_needs_a_three_x_dominant_file() {
        let mut candidates = IndexMap::new();
        candidates.insert(
            "n1".to_string(),
            ScoredNode {
                node: node_in("lib/sinatra/helpers.rb"),
                score: 10.0,
                term_hits: 1,
                distinctive: true,
            },
        );

        // Merely the biggest — 2× the runner-up. No boost.
        apply_core_dir_boost(
            &mut candidates,
            Some(&DominantFile {
                file_path: "lib/sinatra/base.rb".into(),
                edge_count: 200,
                next_edge_count: 100,
            }),
        );
        assert_eq!(candidates["n1"].score, 10.0, "2× is not dominance");

        // 3× — the boost fires, and it reaches the dominant file's SIBLINGS.
        apply_core_dir_boost(
            &mut candidates,
            Some(&DominantFile {
                file_path: "lib/sinatra/base.rb".into(),
                edge_count: 300,
                next_edge_count: 100,
            }),
        );
        assert_eq!(candidates["n1"].score, 10.0 + weights::CORE_DIR);
    }

    #[test]
    fn a_missing_dominant_file_is_simply_no_boost() {
        let mut candidates = IndexMap::new();
        candidates.insert(
            "n1".to_string(),
            ScoredNode {
                node: node_in("src/a.ts"),
                score: 7.0,
                term_hits: 1,
                distinctive: true,
            },
        );
        apply_core_dir_boost(&mut candidates, None);
        assert_eq!(
            candidates["n1"].score, 7.0,
            "TS guards this call with `?.()` and a try/catch — scoring WORKS without it"
        );
    }

    fn node_in(path: &str) -> Node {
        Node {
            id: format!("n:{path}"),
            kind: NodeKind::Function,
            name: "x".into(),
            qualified_name: "x".into(),
            file_path: path.into(),
            language: Language::Ruby,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: None,
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: vec![],
            type_parameters: vec![],
            return_type: None,
            route_method: None,
            route_path: None,
            framework: None,
            updated_at: 0,
        }
    }
}
