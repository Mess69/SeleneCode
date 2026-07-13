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

/// One hop of a rendered flow.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowStep {
    /// The symbol at this step.
    pub node: Node,
    /// How we got here from the previous step (`None` on the first step).
    pub via: Option<Edge>,
}

/// Build the Flow section from named symbols, in the order the caller names them.
///
/// Returns `None` when there is no chain to show — **not an error, and not a fabricated
/// one**. A flow we cannot prove is a flow we must not draw.
pub async fn build_flow_from_named_symbols<S: GraphStore>(
    qm: &QueryManager<S>,
    names: &[String],
) -> Result<Option<Vec<FlowStep>>> {
    if names.len() < 2 {
        return Ok(None);
    }

    // Resolve each name to its best node.
    let mut anchors: Vec<Node> = Vec::new();
    for name in names {
        if let Some(n) = qm.find_all_symbols(name).await?.into_iter().next() {
            anchors.push(n);
        }
    }
    if anchors.len() < 2 {
        return Ok(None);
    }

    // Walk each consecutive pair and splice the paths together.
    let mut steps: Vec<FlowStep> = Vec::new();
    for pair in anchors.windows(2) {
        let (from, to) = (&pair[0], &pair[1]);
        let Some(path) = qm.find_path(&from.id, &to.id, FLOW_KINDS).await? else {
            continue; // no provable chain between these two — draw nothing rather than a guess
        };

        for (node, edge) in path {
            // Splice: the previous pair's endpoint is this pair's start.
            if steps.last().is_some_and(|s| s.node.id == node.id) {
                continue;
            }
            steps.push(FlowStep { node, via: edge });
        }
    }

    if steps.len() < 2 {
        return Ok(None);
    }
    Ok(Some(steps))
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
