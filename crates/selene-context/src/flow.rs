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
use selene_core::{Edge, EdgeKind, Node, Provenance};
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
                    if is_named && steps.len() >= limits::MIN_FLOW_NODES && is_better(&steps, &best)
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
fn is_better(candidate: &[FlowStep], best: &Option<Vec<FlowStep>>) -> bool {
    let Some(best) = best else { return true };
    match candidate.len().cmp(&best.len()) {
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
