//! Relevance scoring — **the ordered pass list. Order IS behavior.**
//!
//! ```text
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
pub const CALLABLE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Component,
];

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

    // ── Pass 5's rerank. MULTIPLICATIVE — see `rerank_by_term_groups`. ──────────────
    /// Per matched concept, when a node matched ≥2: `×(1 + 0.5n)`.
    pub const GROUP_SCALE: f64 = 0.5;
    /// A node literally *named* a common query word (`edge`, `graph`). Matches the letter of
    /// the query and none of its meaning.
    pub const COMMON_WORD_EXACT: f64 = 0.3;
    /// **The penalty the port dropped.** One concept out of several ⇒ probably noise.
    pub const SINGLE_CONCEPT: f64 = 0.6;

    // ── Passes 6 & 7's LIKE scaling. ───────────────────────────────────────────────
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
    pub const CONNECTIVITY: f64 = 60.0;
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
    /// A *discovered* node (one no lexical pass found) must carry at least this share of the
    /// best bridge's score to be admitted. Below it, the walk is just leaking into the repo at
    /// large.
    pub const ADMISSION: f64 = 0.15;
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
}

impl Default for FindOptions {
    fn default() -> Self {
        Self {
            search_limit: 3,
            traversal_depth: 1,
            max_nodes: 20,
            min_score: 0.3,
            node_kinds: HIGH_VALUE_NODE_KINDS.to_vec(),
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
    // --- pass 1: the terms ---------------------------------------------------
    let terms = extract_search_terms(query);
    if terms.is_empty() {
        // A stopword-only query. **Empty is an ANSWER**, and the caller renders guidance.
        return Ok(Vec::new());
    }

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
) -> Result<RelevantContext> {
    let terms = extract_search_terms(query);

    // Passes 1–4.
    let scored = score_candidates(qm, query, opts, dominant).await?;

    // --- passes 6 & 7: LIKE matches -------------------------------------------
    let like = like_passes(qm, &terms, opts).await?;
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
    // (TS reranks before its LIKE passes because in TS the LIKE channels *add* to a node's
    // score. Ours take the max. With a `max()` merge, the rescale has to come last, or it
    // cannot survive the merge. The deviation is in the ORDER; the weights are TS's.)
    rerank_by_term_groups(&mut scored, &terms);

    // --- pass 12: graph connectivity ------------------------------------------
    // BEFORE the sort and BEFORE the truncate, because it both re-scores what the lexical
    // passes found AND admits what they could not. Running it after the cut would let the cut
    // throw away the answer first.
    sort_candidates(&mut scored); // seed order = lexical rank
    apply_graph_connectivity(qm, &mut scored, &terms, opts).await?;

    // --- pass 8: sort → slice → min-score → cap roots --------------------------
    sort_candidates(&mut scored);
    scored.truncate(opts.search_limit * 3);
    scored.retain(|s| s.score >= opts.min_score);

    // --- pass 9: confidence ---------------------------------------------------
    let confidence = confidence_of(&scored, &terms);

    // --- pass 13: root diversity ----------------------------------------------
    let roots: Vec<ScoredNode> = pick_diverse_roots(&scored, opts.search_limit);
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
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }

    // Nothing to bridge unless the query names at least two distinct concepts.
    let groups = term_groups(terms);
    if groups.len() < 2 {
        return Ok(());
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
        return Ok(());
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
                for entry in out
                    .get(id)
                    .into_iter()
                    .flatten()
                    .chain(inc.get(id).into_iter().flatten())
                    .take(rwr::MAX_NEIGHBORS_PER_NODE)
                {
                    if !opts.node_kinds.contains(&entry.node.kind)
                        || is_test_file(&entry.node.file_path)
                    {
                        continue;
                    }
                    if seen.insert(entry.node.id.clone()) {
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
        return Ok(());
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
        return Ok(());
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

    Ok(())
}


/// **Pass 13 — root diversity.** Pick `limit` roots, **never two with the same name**.
///
/// The graph legitimately holds three nodes named `insert_edges` — a trait declaration, its
/// impl, and a delegating impl. Ranking scored all three, and the roots came back as
/// *`insert_edges`, `insert_edges`, `insert_edges`*: the entire root budget spent on three
/// spellings of one symbol, and with it every downstream section — the flow seeds, the file
/// sections, the blast radius — collapsed onto a single concept.
///
/// One name, one root. The rest of the budget goes to symbols that say something new.
pub fn pick_diverse_roots(scored: &[ScoredNode], limit: usize) -> Vec<ScoredNode> {
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
        let concepts = groups
            .iter()
            .filter(|g| g.iter().any(|t| hay.contains(&t.to_lowercase())))
            .count();

        let lower = s.node.name.to_lowercase();
        let exact = terms.iter().any(|t| t.to_lowercase() == lower);

        if concepts >= 2 {
            s.score *= 1.0 + weights::GROUP_SCALE * concepts as f64;
            s.term_hits = s.term_hits.max(concepts);
        } else if exact && !is_code_shaped(&s.node.name) {
            // A node literally NAMED `edge` or `graph`. It matches the query word exactly and
            // means nothing — the query used the word in English, not as an identifier.
            s.score *= weights::COMMON_WORD_EXACT;
        } else if exact {
            // A distinctive exact match is EXEMPT: `handleLogin` named exactly is the answer.
        } else {
            // One concept out of several. Not wrong — just not corroborated. This is the
            // branch the port dropped.
            s.score *= weights::SINGLE_CONCEPT;
        }
    }
}

/// Is this name an *identifier* rather than an English word? `handle_login`, `UnresolvedRef`
/// and `resolve_and_persist_batched` are code-shaped; `edge`, `graph` and `data` are not.
///
/// This distinguishes TS's *"distinctive exact match"* (exempt) from its *"common-word exact
/// match"* (×0.3). The map names the two branches but not the predicate that separates them,
/// so the shape test below is **our reading**, not a port — recorded as such.
fn is_code_shaped(name: &str) -> bool {
    name.contains('_')
        || name.contains('.')
        || name.chars().skip(1).any(char::is_uppercase) // a camelCase/PascalCase boundary
        || name.len() >= 8
}

/// Terms that are substrings of one another are one concept. Longest first, so the longest
/// term names the group.
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
async fn like_passes<S: GraphStore>(
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
            .search_name_like(term, &opts.node_kinds, opts.search_limit * 2)
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
    for s in out.iter_mut() {
        let name = s.node.name.to_lowercase();
        let hits = terms
            .iter()
            .filter(|t| name.contains(&t.to_lowercase()))
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
            language: "ruby".into(),
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
