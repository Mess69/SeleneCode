//! Dynamic + polymorphic **boundary notes** — the "where does it go from here" answers.
//!
//! # The Flow section shows the chain. These show why it *ends*.
//!
//! A flow that simply stops is indistinguishable, to an agent, from a flow we failed to
//! trace. So when the chain reaches a place where control *leaves* the static call graph, we
//! say so, and we say **where it goes**:
//!
//! - **A dynamic boundary** — a callback registration, an event emission, a route
//!   registration. The next hop exists, and Phase 3's synthesizers found it. It is named.
//! - **A polymorphic boundary** — an interface/trait method with N implementations. The next
//!   hop is *one of* N, and which one is a runtime fact. We list them, because "it could be
//!   any of these three" is an answer, and "the trail goes cold" is not.
//!
//! # Why this is a section and not a footnote
//!
//! Without it the agent sees `handleRequest → dispatch` and nothing after, concludes the
//! index is incomplete, and opens the file — where it will find an interface method and
//! still not know the implementations. **We can answer that question and the file cannot.**
//! Not saying so is the worst of both.

use indexmap::IndexMap;
use selene_core::{EdgeKind, Node, Provenance};
use selene_db::GraphStore;
use selene_graph::QueryManager;

use crate::error::Result;

/// Where control leaves the static call graph.
// The variants differ in size (a `Vec<Node>` vs two `Node`s); boxing would buy a pointer
// chase on a type we build a handful of per response and immediately render.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Boundary {
    /// A dynamic hop: the connection is real but invisible in the source. Phase 3 bridged it.
    Dynamic {
        /// The symbol control leaves from.
        from: Node,
        /// Where it lands.
        to: Node,
        /// The channel that bridged it (`callback`, `event-emitter`, `jsx-render`, …).
        channel: String,
    },
    /// A polymorphic hop: an abstract member with N implementations. Which one runs is a
    /// runtime fact — so all N are named.
    Polymorphic {
        /// The abstract member (an interface/trait method).
        abstract_member: Node,
        /// Every implementation we know of.
        implementations: Vec<Node>,
    },
}

/// Find the boundaries around a set of gathered nodes.
///
/// Never `Err` for "there are none" — an empty list is an answer.
pub async fn find_boundaries<S: GraphStore>(
    qm: &QueryManager<S>,
    nodes: &IndexMap<String, Node>,
) -> Result<Vec<Boundary>> {
    let mut out: Vec<Boundary> = Vec::new();

    for node in nodes.values() {
        // --- dynamic: an outgoing edge Phase 3 synthesized -----------------------
        for entry in qm
            .outgoing(&node.id, &[EdgeKind::Calls, EdgeKind::References])
            .await
            .unwrap_or_default()
        {
            if entry.edge.provenance != Some(Provenance::Heuristic) {
                continue;
            }
            let channel = entry
                .edge
                .metadata
                .as_ref()
                .and_then(|m| m.get("synthesizedBy"))
                .and_then(|v| v.as_str())
                .unwrap_or("dispatch")
                .to_string();

            out.push(Boundary::Dynamic {
                from: node.clone(),
                to: entry.node,
                channel,
            });
        }

        // --- polymorphic: who implements/overrides this? -------------------------
        let impls: Vec<Node> = qm
            .incoming(&node.id, &[EdgeKind::Overrides, EdgeKind::Implements])
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.node)
            .collect();

        if impls.len() >= 2 {
            // ≥2, because ONE implementation is not a boundary — it is just the answer, and
            // the flow already walks to it. A "boundary" with a single exit is noise that
            // makes a certain hop look uncertain.
            out.push(Boundary::Polymorphic {
                abstract_member: node.clone(),
                implementations: impls,
            });
        }
    }

    // Deterministic: the output is rendered.
    out.sort_by_key(boundary_sort_key);
    out.dedup_by_key(|b| boundary_sort_key(b));
    Ok(out)
}

fn boundary_sort_key(b: &Boundary) -> String {
    match b {
        Boundary::Dynamic { from, to, channel } => {
            format!("0:{}:{}:{}", from.id, to.id, channel)
        }
        Boundary::Polymorphic {
            abstract_member, ..
        } => format!("1:{}", abstract_member.id),
    }
}

/// Render the boundary notes. Empty input ⇒ empty string (no section, no apology).
pub fn render_boundaries(boundaries: &[Boundary]) -> String {
    if boundaries.is_empty() {
        return String::new();
    }

    let mut out = String::from("### Where control goes next\n\n");
    for b in boundaries {
        match b {
            Boundary::Dynamic { from, to, channel } => {
                // Named, with the channel — so the agent knows the hop is REAL and knows it
                // could not have found it by reading.
                out.push_str(&format!(
                    "- `{}` → **`{}`** *(dynamic: {channel} — this connection is not written \
                     in the source; it is registered at runtime)*\n",
                    from.name, to.name
                ));
            }
            Boundary::Polymorphic {
                abstract_member,
                implementations,
            } => {
                let names: Vec<String> = implementations
                    .iter()
                    .map(|n| format!("`{}` ({})", n.name, n.file_path))
                    .collect();
                // "It could be any of these three" is an ANSWER. "The trail goes cold" is not.
                out.push_str(&format!(
                    "- `{}` is polymorphic — at runtime it dispatches to one of: {}\n",
                    abstract_member.name,
                    names.join(", ")
                ));
            }
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_core::{Language, NodeKind};

    fn node(id: &str, name: &str, file: &str) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Method,
            name: name.into(),
            qualified_name: name.into(),
            file_path: file.into(),
            language: Language::Typescript,
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

    /// The dynamic note must say the connection is NOT in the source — that sentence is the
    /// one that stops the agent from going to look for it.
    #[test]
    fn a_dynamic_boundary_says_the_hop_is_invisible_in_the_source() {
        let rendered = render_boundaries(&[Boundary::Dynamic {
            from: node("a", "emit", "src/bus.ts"),
            to: node("b", "onMount", "src/app.ts"),
            channel: "event-emitter".into(),
        }]);

        assert!(rendered.contains("dynamic: event-emitter"));
        assert!(
            rendered.contains("not written in the source"),
            "without this sentence the agent opens the file to find the connection — and it \
             is not there, so it has spent a Read to learn nothing:\n{rendered}"
        );
    }

    /// "One of these three" is an answer; "the trail goes cold" is not.
    #[test]
    fn a_polymorphic_boundary_names_every_implementation() {
        let rendered = render_boundaries(&[Boundary::Polymorphic {
            abstract_member: node("i", "handle", "src/handler.ts"),
            implementations: vec![
                node("a", "handle", "src/auth.ts"),
                node("b", "handle", "src/blob.ts"),
            ],
        }]);

        assert!(rendered.contains("polymorphic"));
        assert!(rendered.contains("src/auth.ts") && rendered.contains("src/blob.ts"));
    }

    /// No boundaries ⇒ no section. An empty "Where control goes next" heading is worse than
    /// none: it implies we looked and could not say.
    #[test]
    fn no_boundaries_renders_nothing_at_all() {
        assert_eq!(render_boundaries(&[]), "");
    }
}
