//! The **Flow** section — the spine of the answer.
//!
//! # This is what the agent reads *instead of* the code
//!
//! An agent asking "how does a login request get handled" wants one thing: the chain, in
//! order, with names it can trust. Everything else in an explore response is supporting
//! material. If the Flow section is right, the agent stops. If it has a gap, the agent Reads.
//!
//! # A gap is not a smaller answer — it is a *worse* one
//!
//! ```text
//! 1. handleLogin
//!    ↓ calls
//! 2. login
//!    ↓ ???            ← the agent goes to Read, right here
//! 3. ???
//! ```
//!
//! `A → B → ?` sends the agent straight to the file. **That is the entire reason Phase 3's
//! dynamic-dispatch work exists**: a callback registration, an EventEmitter channel, a React
//! re-render, a route→handler bridge — each is a hop the agent *cannot* find by reading,
//! because the connection is not written anywhere in the source. If we know it and do not
//! render it, we have done the hard work and thrown the answer away.
//!
//! So every synthesized edge (`provenance: heuristic`) is rendered as a **named hop** with a
//! `↓ dynamic:` marker naming the channel that bridged it. Not as a gap. Not as an ordinary
//! call either — the marker is what tells the agent "this connection is real but invisible in
//! the source", which is exactly the thing it would otherwise go looking for.

use indexmap::IndexMap;
use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance};
use selene_db::GraphStore;
use selene_graph::QueryManager;

use crate::error::Result;

/// The edge kinds a flow may traverse. **`Contains` is excluded**: file→class→method
/// containment is structure, not flow, and walking it would let a "flow" run through pure
/// nesting — a path that looks like an answer and explains nothing.
pub const FLOW_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Imports,
    EdgeKind::Instantiates,
];

/// The kinds the flow **BFS** walks. Narrower than [`FLOW_KINDS`] and deliberately so:
/// `Imports` is a module-level fact, not a step in a flow, and letting the spine run through
/// it draws a "chain" that explains nothing. TS walks `calls` only
/// (`maps/mcp-context.md` §`buildFlowFromNamedSymbols`); `References` is kept because Rust's
/// method-value and trait-object hops land there.
const BFS_KINDS: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::References];

/// Kinds a flow may **not end on**.
///
/// A spine ends where the answer is, and a constant is not an answer — it is a value that
/// happens to sit near one. Asked *"how does a file get indexed"*, the walk arrived at
/// `FILE_DDL` (a `&str` of schema DDL) purely because its NAME contains *file*, and it beat the
/// real pipeline (`index → index_all → run_pipeline → …`) for being shorter. Spelling is not
/// arrival.
///
/// `selene-context` has made this exact mistake once before, one layer up: pass 12's admission
/// gate exists because the connectivity walk kept volunteering `RESOLVE_BATCH` and
/// `INSERT_CHUNK` — two `usize` chunk-size constants that "sit next to the answer and explain
/// none of it". Same species, same fix, and TS draws the line in the same place: non-callable
/// `{constant, variable, field, property}` endpoints never take the ordinary spine
/// (`maps/mcp-context.md` §10).
///
/// **Types are NOT excluded**, deliberately: *"…become a graph **edge**"* is answered by
/// arriving at `Edge`, and that is the whole shape of a "how does X become Y" question.
///
/// **This is TS's list, exactly — four kinds, no more.** A first cut also excluded
/// `Import`/`Export`/`Parameter`/`EnumMember`, which is a defensible-sounding guess and was
/// wrong: in a two-file project whose only call edge is `handleLogin → login`, the sole provable
/// 3-node spine runs through the `login` **import** node, and excluding it deleted the Flow
/// section entirely (`selene-mcp/tests/server_test.rs`). Widening a ported list "while we're
/// here" is how a fix becomes a regression.
const SINK_EXCLUDED: &[NodeKind] = &[
    NodeKind::Constant,
    NodeKind::Variable,
    NodeKind::Field,
    NodeKind::Property,
];

/// TS parity constants (`maps/mcp-context.md` §10). Ported, not chosen.
mod limits {
    /// The furthest a spine may run.
    pub const MAX_HOPS: usize = 7;
    /// **At most one consecutive *unnamed* hop.** The bridge is what lets a chain cross one
    /// intermediate the agent did not name (`resolve_all` between `resolve_and_persist_batched`
    /// and `resolve_one`). Allowing two would let the walk wander a god-function's fan-out and
    /// call the result a flow.
    pub const MAX_BRIDGE: usize = 1;
    /// How many resolved symbols may seed the BFS.
    pub const MAX_SEEDS: usize = 8;
    /// Definitions kept per named symbol (a name can be a trait decl *and* two impls).
    pub const MAX_DEFS_PER_NAME: usize = 6;
    /// Names considered at all.
    pub const MAX_NAMES: usize = 16;
    /// **A 2-node "flow" is just an edge** — it tells the agent nothing it did not ask. Three
    /// is the shortest chain that reveals a hop.
    pub const MIN_FLOW_NODES: usize = 3;
    /// The frontier ceiling — a hub with thousands of callees must not blow the walk up.
    pub const FRONTIER_CAP: usize = 1500;
}

/// One hop of a rendered flow.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowStep {
    /// The symbol at this step.
    pub node: Node,
    /// How we got here from the previous step (`None` on the first step).
    pub via: Option<Edge>,
}

/// Build the Flow section from named symbols — **a BFS over the call graph, seeded by every
/// symbol the caller named, longest chain wins.**
///
/// Returns `None` when there is no chain to show — **not an error, and not a fabricated
/// one**. A flow we cannot prove is a flow we must not draw.
///
/// # The order the names arrive in is NOT the order of the flow
///
/// This used to walk `names.windows(2)` and demand a path from each name to the *next one the
/// caller happened to list*. That is a trap, and it made the Flow section a coin-flip:
///
/// - The **caller cannot know the call order** — discovering it is the entire question being
///   asked. Requiring the answer as input is requiring the agent to already have it.
/// - The ranked roots arrive in **score order**, which has no relationship to call order. The
///   same three roots rendered a flow or rendered nothing depending on how ranking happened to
///   sort them.
/// - Real flows are not chains — they **fan out**. `resolve_and_persist_batched` calls
///   `resolve_all` *and* `create_edges` *and* `insert_edges`; `resolve_one` never calls
///   `create_edges`. Splicing consecutive pairs demands a linear spine the code does not have,
///   finds "no path", and silently draws nothing.
///
/// So: seed the BFS with **all** the named symbols, walk `calls`/`references` forward, and
/// keep the **longest chain that ends on a named symbol**. Order-independent, fan-out-tolerant,
/// and still incapable of drawing a hop it cannot prove.
pub async fn build_flow_from_named_symbols<S: GraphStore>(
    qm: &QueryManager<S>,
    names: &[String],
) -> Result<Option<Vec<FlowStep>>> {
    if names.len() < 2 {
        return Ok(None);
    }

    // 1. Resolve the names. A name may have several definitions (a trait decl and two impls);
    //    all of them are legitimate anchors, so keep up to MAX_DEFS_PER_NAME each.
    let mut named: IndexMap<String, Node> = IndexMap::new();
    for name in names.iter().take(limits::MAX_NAMES) {
        for n in qm
            .find_all_symbols(name)
            .await?
            .into_iter()
            .take(limits::MAX_DEFS_PER_NAME)
        {
            named.entry(n.id.clone()).or_insert(n);
        }
    }
    // The agent NAMED these symbols; the concepts are the names themselves, one group each.
    let concepts: Vec<Vec<String>> = names.iter().map(|n| vec![n.clone()]).collect();
    walk_flow(qm, named, &concepts).await
}

/// The same walk, over nodes the caller **already holds** — no name round-trip.
///
/// [`build_flow_from_named_symbols`] exists to turn the symbols an *agent typed* into nodes, and
/// its caps are TS's caps on exactly that: ≤16 tokens, ≤6 definitions each
/// (`maps/mcp-context.md` §10). Those are the right limits for a query and the wrong ones for a
/// **gathered subgraph**, which is what the fallback path actually has.
///
/// Measured: asked *"how does an unresolved reference become a graph edge"* with the roots
/// finally correct, the fallback still rendered **no Flow**. It was handing 147 subgraph nodes
/// over as *names*, `MAX_NAMES` kept the first 16, and the chain's own sinks — `create_edges`,
/// `resolve_all`, `insert_edges` — fell outside the cut. With `MAX_BRIDGE = 1` the walk then
/// died one hop out of the root.
///
/// Two further reasons never to make that round-trip: `find_all_symbols` matches names
/// **case-sensitively** (so a subgraph node `Edge` does not match itself through a lowercase
/// name) and, on zero exact matches, **falls through to an arbitrary top-FTS hit** — which can
/// seed a chain with a symbol that has nothing to do with the query and render a *confidently
/// wrong* flow. A node we already resolved cannot be mis-resolved.
pub async fn build_flow_from_nodes<S: GraphStore>(
    qm: &QueryManager<S>,
    nodes: &[Node],
    concepts: &[Vec<String>],
) -> Result<Option<Vec<FlowStep>>> {
    let mut named: IndexMap<String, Node> = IndexMap::new();
    for n in nodes {
        named.entry(n.id.clone()).or_insert_with(|| n.clone());
    }
    walk_flow(qm, named, concepts).await
}

/// The BFS itself. `named` is insertion-ordered: the first [`limits::MAX_SEEDS`] entries seed the
/// walk, and **every** entry is an acceptable sink. `concepts` are the query's term-groups —
/// they decide, via [`is_better`], which of the many provable chains is the ANSWER.
async fn walk_flow<S: GraphStore>(
    qm: &QueryManager<S>,
    named: IndexMap<String, Node>,
    concepts: &[Vec<String>],
) -> Result<Option<Vec<FlowStep>>> {
    if named.len() < 2 {
        return Ok(None);
    }

    // 2. BFS forward from every named symbol. A path may cross at most MAX_BRIDGE consecutive
    //    hops through symbols the caller did *not* name — enough to reveal the intermediate
    //    (`resolve_all`), not enough to wander.
    let seeds: Vec<&Node> = named.values().take(limits::MAX_SEEDS).collect();
    let mut best: Option<Vec<FlowStep>> = None;

    for seed in seeds {
        let mut frontier: Vec<Walk> = vec![Walk {
            steps: vec![FlowStep {
                node: seed.clone(),
                via: None,
            }],
            bridge: 0,
        }];

        for _hop in 0..limits::MAX_HOPS {
            if frontier.is_empty() {
                break;
            }

            let ids: Vec<String> = frontier
                .iter()
                .filter_map(|w| w.steps.last().map(|s| s.node.id.clone()))
                .collect();
            let out = qm
                .store()
                .outgoing_batch(&ids, BFS_KINDS)
                .await
                .map_err(selene_graph::GraphError::from)?;

            let mut next: Vec<Walk> = Vec::new();
            for walk in &frontier {
                let Some(tail) = walk.steps.last() else {
                    continue;
                };
                let Some(neighbors) = out.get(&tail.node.id) else {
                    continue;
                };

                // Deterministic expansion: the store hands back a HashMap, and an unsorted
                // frontier makes the rendered flow differ run to run.
                let mut neighbors: Vec<&selene_db::NeighborEntry> = neighbors.iter().collect();
                neighbors.sort_by(|a, b| {
                    (&a.node.file_path, a.node.start_line, &a.node.name).cmp(&(
                        &b.node.file_path,
                        b.node.start_line,
                        &b.node.name,
                    ))
                });

                for entry in neighbors {
                    // A path never revisits a node: a cycle rendered as a flow is a lie the
                    // agent cannot check.
                    if walk.steps.iter().any(|s| s.node.id == entry.node.id) {
                        continue;
                    }
                    let is_named = named.contains_key(&entry.node.id);
                    let bridge = if is_named { 0 } else { walk.bridge + 1 };
                    if bridge > limits::MAX_BRIDGE {
                        continue; // two unnamed hops in a row is wandering, not a flow
                    }

                    let mut steps = walk.steps.clone();
                    steps.push(FlowStep {
                        node: entry.node.clone(),
                        via: Some(entry.edge.clone()),
                    });

                    // A chain is only an ANSWER if it ends on something the caller named —
                    // trailing off into an unnamed callee is where the agent starts reading.
                    if is_named
                        && steps.len() >= limits::MIN_FLOW_NODES
                        && !SINK_EXCLUDED.contains(&entry.node.kind)
                        && is_better(&steps, &best, concepts)
                    {
                        best = Some(steps.clone());
                    }
                    next.push(Walk { steps, bridge });
                }
            }

            next.truncate(limits::FRONTIER_CAP);
            frontier = next;
        }
    }

    Ok(best)
}

/// One in-progress walk: the steps so far, and how many *consecutive* unnamed hops it has
/// spent (reset to 0 every time it lands on a named symbol).
struct Walk {
    steps: Vec<FlowStep>,
    bridge: usize,
}

/// Longest chain wins. **The tie-break is a contract** — two chains of equal length must not
/// swap between runs, because the flow is rendered and a reordering diff is indistinguishable
/// from a ranking change.
/// Does this chain **arrive somewhere the query asked about that it did not start from**?
///
/// This is the whole objective, and getting it wrong renders a confidently wrong answer.
///
/// *"How does an unresolved reference become a graph edge"* names two poles. The answer is a
/// chain that **starts at one and ends at the other**. A chain that starts in `resolve` and ends
/// in `resolve` has explained nothing, however long it is.
///
/// Measured, with the roots finally correct and "longest chain wins" still in force:
///
/// ```text
///   1. resolve_and_persist_batched   <- right
///   2. resolve_all                   <- right
///   3. resolve_one                   <- right
///   4. resolve_via_import   5. resolve_import_path   6. is_external_import
///   7. resolve_workspace_import      8. WorkspacePackages   <- nowhere near "graph edge"
/// ```
///
/// Every hop is a real call edge and the whole thing is off-topic: it walks *deeper into the
/// resolver* because depth was the objective. Note it cannot be fixed by demanding the chain end
/// on a query-relevant symbol either — `resolve_workspace_import` **is** query-relevant. The
/// entire crate is named `resolve_*`. What is missing is that the chain never **arrives** at the
/// other pole.
fn concept_gain(steps: &[FlowStep], concepts: &[Vec<String>]) -> bool {
    if concepts.len() < 2 {
        return false; // a one-concept query has no second pole to reach; length decides
    }
    let (Some(first), Some(last)) = (steps.first(), steps.last()) else {
        return false;
    };
    let start = concepts_of(&first.node.name, concepts);
    let end = concepts_of(&last.node.name, concepts);
    !end.is_empty() && !end.is_subset(&start)
}

/// Which of the query's concepts does this name spell? (Name only — a path says where a symbol
/// was filed, not what it does.)
fn concepts_of(name: &str, concepts: &[Vec<String>]) -> std::collections::BTreeSet<usize> {
    let lower = name.to_lowercase();
    concepts
        .iter()
        .enumerate()
        .filter(|(_, g)| g.iter().any(|t| crate::relevance::name_carries(&lower, t)))
        .map(|(i, _)| i)
        .collect()
}

/// Rank two candidate spines.
///
/// **A chain that reaches the query's other pole beats one that does not, at any length.** Among
/// chains that do, the *shortest* wins: the tightest route from X to Y is the explanation, and
/// every extra hop past the arrival is a hop the agent did not ask for. Only when no chain
/// arrives anywhere new (a single-concept query — the agent named one symbol and wants to see
/// where control goes) does length decide, which is the original behavior.
fn is_better(
    candidate: &[FlowStep],
    best: &Option<Vec<FlowStep>>,
    concepts: &[Vec<String>],
) -> bool {
    let Some(best) = best else { return true };

    let (cg, bg) = (
        concept_gain(candidate, concepts),
        concept_gain(best, concepts),
    );
    if cg != bg {
        return cg;
    }

    // Both arrive, or neither does. Arriving ⇒ tighter is better; otherwise ⇒ longer.
    let ord = if cg {
        best.len().cmp(&candidate.len())
    } else {
        candidate.len().cmp(&best.len())
    };
    match ord {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            let key = |s: &[FlowStep]| -> Vec<(String, u32, String)> {
                s.iter()
                    .map(|st| {
                        (
                            st.node.file_path.clone(),
                            st.node.start_line,
                            st.node.name.clone(),
                        )
                    })
                    .collect()
            };
            key(candidate) < key(best)
        }
    }
}

/// Render the steps as the Flow section an agent reads.
///
/// A synthesized hop is `↓ dynamic: <channel>` — see the module docs for why that marker is
/// the difference between an answer and a trip to `Read`.
pub fn render_flow(steps: &[FlowStep]) -> String {
    let mut out = String::from("### Flow\n\n");

    for (i, step) in steps.iter().enumerate() {
        if let Some(edge) = &step.via {
            out.push_str(&format!("   ↓ {}\n", describe_hop(edge)));
        }
        out.push_str(&format!(
            "{}. `{}` ({}:{})\n",
            i + 1,
            step.node.name,
            step.node.file_path,
            step.node.start_line
        ));
    }
    out.push('\n');
    out
}

/// How this hop happened. A heuristic edge names its **channel**, because "how" is the whole
/// question when the connection is invisible in the source.
pub fn describe_hop(edge: &Edge) -> String {
    if edge.provenance == Some(Provenance::Heuristic) {
        let channel = edge
            .metadata
            .as_ref()
            .and_then(|m| m.get("synthesizedBy"))
            .and_then(|v| v.as_str())
            .unwrap_or("dispatch");
        // ⚠ The `dynamic:` marker is load-bearing. Rendering this as a plain `calls` would
        // tell the agent the connection is in the source — it is not, and it would go
        // looking. Rendering it as a GAP would send it to Read. The marker is the third
        // option, and it is the only correct one.
        return format!("dynamic: {channel}");
    }
    edge.kind.as_str().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edge(kind: EdgeKind, prov: Provenance, synth: Option<&str>) -> Edge {
        Edge {
            source: "a".into(),
            target: "b".into(),
            kind,
            metadata: synth.map(|s| json!({ "synthesizedBy": s })),
            line: Some(1),
            column: Some(0),
            provenance: Some(prov),
        }
    }

    fn step(name: &str) -> FlowStep {
        FlowStep {
            node: Node {
                id: name.into(),
                kind: NodeKind::Function,
                name: name.into(),
                qualified_name: name.into(),
                file_path: "src/lib.rs".into(),
                language: "rust".into(),
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
            },
            via: None,
        }
    }

    /// **The objective, and the bug it exists to prevent.**
    ///
    /// "How does an unresolved reference become a graph edge" names two poles. With "longest
    /// chain wins", the walk answered it by descending eight hops *deeper into the resolver* and
    /// stopping at `WorkspacePackages` — every hop a real call edge, the whole spine off-topic.
    /// Length is not relevance. **Arrival is.**
    #[test]
    fn a_chain_that_reaches_the_other_pole_beats_a_longer_one_that_never_arrives() {
        let concepts = vec![vec!["resolve".to_string()], vec!["edge".to_string()]];

        // The real wander, shortened: starts in `resolve`, ends in `resolve`. Explains nothing.
        let wander = vec![
            step("resolve_and_persist_batched"),
            step("resolve_all"),
            step("resolve_one"),
            step("resolve_via_import"),
            step("resolve_workspace_import"),
        ];
        // The answer: starts in `resolve`, ARRIVES in `edge`. Half the length.
        let arrives = vec![
            step("resolve_and_persist_batched"),
            step("insert_edges"),
            step("Edge"),
        ];

        assert!(
            !concept_gain(&wander, &concepts),
            "resolve → resolve gains nothing"
        );
        assert!(
            concept_gain(&arrives, &concepts),
            "resolve → edge is the question"
        );

        // Arrival beats length, in either evaluation order (the BFS may find either first).
        assert!(is_better(&arrives, &Some(wander.clone()), &concepts));
        assert!(!is_better(&wander, &Some(arrives.clone()), &concepts));

        // Among chains that BOTH arrive, the tighter one wins: every hop past the arrival is a
        // hop the agent did not ask for.
        let long_arrival = vec![
            step("resolve_and_persist_batched"),
            step("resolve_all"),
            step("resolve_one"),
            step("insert_edges"),
        ];
        assert!(concept_gain(&long_arrival, &concepts));
        assert!(is_better(&arrives, &Some(long_arrival), &concepts));
    }

    /// A single-concept query ("show me where control goes from X") has no second pole to reach,
    /// so length decides — the original behavior, deliberately preserved.
    #[test]
    fn with_one_concept_there_is_no_pole_to_reach_and_length_decides() {
        let one = vec![vec!["resolve_and_persist_batched".to_string()]];
        let short = vec![
            step("resolve_and_persist_batched"),
            step("resolve_all"),
            step("x"),
        ];
        let long = vec![
            step("resolve_and_persist_batched"),
            step("resolve_all"),
            step("resolve_one"),
            step("match_reference"),
        ];
        assert!(!concept_gain(&long, &one));
        assert!(
            is_better(&long, &Some(short), &one),
            "longest still wins on a 1-concept query"
        );
    }

    #[test]
    fn a_synthesized_hop_is_named_dynamic_with_its_channel() {
        let e = edge(EdgeKind::Calls, Provenance::Heuristic, Some("callback"));
        assert_eq!(
            describe_hop(&e),
            "dynamic: callback",
            "the channel is the answer to 'how did we get here' — and it is invisible in the \
             source, which is exactly why the agent would otherwise go and Read"
        );
    }

    #[test]
    fn an_ordinary_hop_is_named_by_its_kind() {
        let e = edge(EdgeKind::Calls, Provenance::TreeSitter, None);
        assert_eq!(describe_hop(&e), "calls");
    }

    /// A heuristic edge with no channel recorded still says `dynamic:` — never a bare `calls`,
    /// which would claim the connection is written in the source.
    #[test]
    fn a_synthesized_hop_without_a_channel_still_says_dynamic() {
        let e = edge(EdgeKind::Calls, Provenance::Heuristic, None);
        assert_eq!(describe_hop(&e), "dynamic: dispatch");
    }
}
