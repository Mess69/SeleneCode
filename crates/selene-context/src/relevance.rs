//! Relevance scoring — **the ordered pass list. Order IS behavior.**
//!
//! ```text
//!  1. symbol extraction from the query                        (Task 5) ✅
//!  2. exact-name lookup + co-location boost   (+20 per extra) (Task 5) ✅
//!  3. TitleCase prefix search over definitions (+15 + brevity)(Task 5) ✅
//!  4. per-term FTS + multi-term boost (+5), test dampen (×0.3),
//!     dominant-file core-dir boost (+25)                      (Task 5) ✅
//!  5. term-group co-occurrence rerank                         (Task 6) — stub
//!  6. camelCase-boundary LIKE matches                         (Task 6) — stub
//!  7. compound ≥2-term LIKE matches                           (Task 6) — stub
//!  8. sort → slice(search_limit*3) → min-score → imports→defs → cap roots (Task 6) — stub
//!  9. confidence: LOW iff ≥2 terms AND no result with 2 hits or a distinctive name (T6)
//! 10. type-hierarchy expansion (budget max_nodes/4, 2 passes)  (Task 6) — stub
//! 11. BFS both directions → trims → edge recovery              (Task 6) — stub
//! ```
//!
//! Every weight below is **ported, not chosen**. The numbers are the product: `+20` for
//! co-location is what makes `scrapeLoop` and `run` in the same file outrank two unrelated
//! symbols of the same name; `×0.3` on test files is what keeps a test double from
//! outranking the thing it doubles. A test that asserts *ordering* passes even when a weight
//! is 10× wrong, so the tests assert the **numbers**.

use indexmap::IndexMap;
use selene_core::{Node, NodeKind};
use selene_db::GraphStore;
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
// Task 6 fills these — the pass list above is the contract; it is never re-ordered.
// =============================================================================
//
//  5. term-group co-occurrence rerank
//  6. camelCase-boundary LIKE matches
//  7. compound ≥2-term LIKE matches
//  8. sort → slice → min-score → imports→definitions → cap roots
//  9. confidence (LOW iff ≥2 terms AND nothing with 2 hits or a distinctive name)
// 10. type-hierarchy expansion
// 11. BFS both directions → trims → edge recovery

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
