//! Relevance scoring — **the ordered pass list. Order IS behavior.**
//!
//! ```text
//!  1. symbol extraction from the query                        (Task 5) ✅
//!  2. exact-name lookup + co-location boost   (+20 per extra) (Task 5) ✅
//!  3. TitleCase prefix search over definitions (+15 + brevity)(Task 5) ✅
//!  4. per-term FTS + multi-term boost (+5), test dampen (×0.3),
//!     dominant-file core-dir boost (+25)                      (Task 5) ✅
//!  5. term-group co-occurrence rerank                         (Task 6) ✅
//!  6. camelCase-boundary LIKE matches                         (Task 6) ✅
//!  7. compound ≥2-term LIKE matches                           (Task 6) ✅
//!  8. sort → slice(search_limit*3) → min-score → cap roots     (Task 6) ✅
//!  9. confidence: LOW iff ≥2 terms AND nothing with 2 hits or a distinctive name (T6) ✅
//! 10. type-hierarchy expansion (budget max_nodes/4)            (Task 6) ✅
//! 11. BFS both directions → trims → edge recovery              (Task 6) ✅
//! ```
//!
//! Every weight below is **ported, not chosen**. The numbers are the product: `+20` for
//! co-location is what makes `scrapeLoop` and `run` in the same file outrank two unrelated
//! symbols of the same name; `×0.3` on test files is what keeps a test double from
//! outranking the thing it doubles. A test that asserts *ordering* passes even when a weight
//! is 10× wrong, so the tests assert the **numbers**.

use indexmap::IndexMap;
use selene_core::{Edge, EdgeKind, Node, NodeKind};
use selene_db::{GraphStore, Subgraph};
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
    for term in &terms {
        let hits = qm
            .store()
            .search_fts(
                std::slice::from_ref(term),
                &opts.node_kinds,
                &[],
                opts.search_limit * 2,
                0,
            )
            .await
            .map_err(selene_graph::GraphError::from)?;

        for c in hits {
            let distinctive = is_distinctive(&c.node.name, &terms);
            // The FIRST hit for a node contributes its raw score; each EXTRA term it matches
            // adds +5. A node matching "shard" + "search" + "request" beats one matching
            // only "execution", which is the whole reason this boost exists.
            let entry = by_id.get(&c.node.id).map(|e| e.term_hits);
            let bonus = match entry {
                Some(_) => weights::MULTI_TERM, // an extra term hit
                None => c.raw_score,
            };
            upsert(&mut by_id, c.node, bonus, 1, distinctive);
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
    let mut scored = score_candidates(qm, query, opts, dominant).await?;

    // --- pass 5: term-group co-occurrence rerank ------------------------------
    rerank_by_term_groups(&mut scored, &terms);

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

    // --- pass 8: sort → slice → min-score → cap roots --------------------------
    sort_candidates(&mut scored);
    scored.truncate(opts.search_limit * 3);
    scored.retain(|s| s.score >= opts.min_score);

    // --- pass 9: confidence ---------------------------------------------------
    let confidence = confidence_of(&scored, &terms);

    let roots: Vec<ScoredNode> = scored.into_iter().take(opts.search_limit).collect();
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

/// **Pass 5.** Group query terms that are stem variants of one another (`indexed`, `index`)
/// so they count as ONE concept, then boost nodes matching ≥2 distinct concepts.
///
/// Without the grouping, stem variants inflate the match count and hand a false multi-term
/// boost to a symbol that matched one root word three ways.
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
        if concepts >= 2 {
            s.score += (concepts - 1) as f64 * weights::MULTI_TERM;
            s.term_hits = s.term_hits.max(concepts);
        }
    }
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
    for term in terms {
        let hits = qm
            .store()
            .search_name_like(term, &opts.node_kinds, opts.search_limit * 2)
            .await
            .map_err(selene_graph::GraphError::from)?;
        for c in hits {
            let distinctive = is_distinctive(&c.node.name, terms);
            out.push(ScoredNode {
                score: c.raw_score,
                node: c.node,
                term_hits: 1,
                distinctive,
            });
        }
    }

    // Pass 7: a node whose name contains TWO OR MORE of the terms is a compound hit —
    // `ShardSearchRequest` for "shard search request" — and outranks a one-term match.
    if terms.len() >= 2 {
        for s in out.iter_mut() {
            let name = s.node.name.to_lowercase();
            let hits = terms
                .iter()
                .filter(|t| name.contains(&t.to_lowercase()))
                .count();
            if hits >= 2 {
                s.score += (hits - 1) as f64 * weights::MULTI_TERM;
                s.term_hits = s.term_hits.max(hits);
            }
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
