//! Synthesizers 3/5 and 4/5 — React re-render, and JSX child.
//!
//! # These two are ONE unit. Do not ship `react-render` alone.
//!
//! ```text
//! handleClick → [react-render] → render → [jsx-render] → StaticCanvas → renderStaticScene
//!               ^^^^^^^^^^^^^^           ^^^^^^^^^^^^
//!                  Task 24                  Task 25
//! ```
//!
//! The map records the measurement, and it is the worked example of PRD §8.2:
//! shipping `react-render` **without** `jsx-render` measurably **RAISED** agent
//! reads. The half-bridged flow ends at `render`, which *advertises* that
//! something happens next and gives the agent nowhere to go — so it opens the
//! file. A bridge to nowhere is worse than no bridge.
//!
//! Neither pass is a flow on its own. `render` is not an answer; `renderStaticScene`
//! is.
//!
//! # react-render: the hole
//!
//! `this.setState({…})` hands control to React's reconciler, which calls
//! `render()`. There is no static call anywhere between them.
//!
//! **Over-approximation is accepted.** A `setState` in a rarely-taken branch still
//! links. The model is *reachability*, not instance precision — guards that trade
//! recall for a precision the product does not need are the wrong trade here.
//!
//! # jsx-render: the hole
//!
//! `<StaticCanvas …/>` inside `render()` **is a call** to that component. tree-sitter
//! sees a JSX element, not a call. Lowercase tags (`<div>`) are DOM and correctly
//! ignored — the capital initial is JSX's own component/element discriminator.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Edge, EdgeKind, Node, NodeKind};
use selene_db::GraphStore;

use super::{node_body, stream_nodes_by_kind, synth_edge};
use crate::Result;
use crate::context::ResolutionContext;

/// Per class. Beyond this the "class" is a god-object and the edges stop meaning
/// anything.
pub const MAX_SETSTATE_SIBLINGS: usize = 40;
/// Per parent component.
pub const MAX_JSX_CHILDREN: usize = 30;

static SET_STATE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"this\.setState\s*\(").unwrap()
});

/// `<Foo`, `<Foo/>`, `<Foo ` — a **capital** initial. Lowercase is a DOM tag.
static JSX_TAG: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"<([A-Z][A-Za-z0-9_]*)[\s/>]").unwrap()
});

// =============================================================================
// 3/5 — react-render: setState → render
// =============================================================================

pub async fn run_react_render<S: GraphStore>(
    store: &S,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    // --- async: every class, and its methods ---------------------------------
    let classes = stream_nodes_by_kind(store, NodeKind::Class).await?;
    let mut children_per_class = Vec::with_capacity(classes.len());
    for c in &classes {
        children_per_class.push(store.outgoing(&c.id, &[EdgeKind::Contains], None).await?);
    }

    // --- sync: read the bodies (see the seam note in synth/mod.rs) -----------
    let mut edges = tokio::task::block_in_place(|| {
        let mut edges = Vec::new();

        for (class, children) in classes.iter().zip(&children_per_class) {
            let methods: Vec<&Node> = children
                .iter()
                .map(|e| &e.node)
                .filter(|n| n.kind == NodeKind::Method)
                .collect();

            // No `render` ⇒ not a React class component. Skip it entirely: this
            // is what keeps the pass inert on every ordinary OO class that
            // happens to have a method called `setState`.
            let Some(render) = methods.iter().find(|m| m.name == "render") else {
                continue;
            };

            // Deterministic: sort the siblings BEFORE the cap.
            let mut siblings: Vec<&&Node> = methods
                .iter()
                .filter(|m| m.id != render.id)
                .filter(|m| {
                    node_body(ctx, m)
                        .is_some_and(|b| b.contains("setState") && SET_STATE.is_match(&b))
                })
                .collect();
            siblings.sort_by_key(|m| (m.start_line, m.name.clone()));

            for m in siblings.into_iter().take(MAX_SETSTATE_SIBLINGS) {
                edges.push(synth_edge(
                    &m.id,
                    &render.id,
                    "react-render",
                    Some(m.start_line),
                    &[
                        ("via", "setState".to_string()),
                        (
                            "registeredAt",
                            format!("{}:{}", render.file_path, render.start_line),
                        ),
                    ],
                ));
            }
            let _ = class; // the class itself is not an endpoint, only its methods
        }
        edges
    });

    edges.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
    Ok(edges)
}

// =============================================================================
// 4/5 — jsx-render: <Child/> → the component
// =============================================================================

pub async fn run_jsx_render<S: GraphStore>(
    store: &S,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    // --- async: every renderable body ----------------------------------------
    let mut parents: Vec<Node> = stream_nodes_by_kind(store, NodeKind::Method).await?;
    parents.extend(stream_nodes_by_kind(store, NodeKind::Function).await?);
    parents.extend(stream_nodes_by_kind(store, NodeKind::Component).await?);

    // --- sync: scan the bodies for JSX ---------------------------------------
    let mut edges = tokio::task::block_in_place(|| {
        let mut edges = Vec::new();

        for parent in &parents {
            let Some(body) = node_body(ctx, parent) else {
                continue;
            };
            // Cheap pre-gate before the regex (#1235).
            if !body.contains("</") && !body.contains("/>") && !body.contains('<') {
                continue;
            }

            // Deterministic + deduped: one edge per distinct tag.
            let tags: BTreeSet<String> = JSX_TAG
                .captures_iter(&body)
                .filter_map(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .collect();

            for tag in tags.into_iter().take(MAX_JSX_CHILDREN) {
                // Resolve the tag to a component. AMBIGUOUS ⇒ SKIP.
                let hits: Vec<_> = ctx
                    .nodes_by_name(&tag)
                    .into_iter()
                    .filter(|n| {
                        matches!(
                            n.kind,
                            NodeKind::Component | NodeKind::Function | NodeKind::Class
                        )
                    })
                    .collect();
                if hits.len() != 1 {
                    continue;
                }
                let child = &hits[0];
                if child.id == parent.id {
                    continue; // a component rendering itself is recursion, not a hop
                }

                edges.push(synth_edge(
                    &parent.id,
                    &child.id,
                    "jsx-render",
                    Some(parent.start_line),
                    // NO `registeredAt` — there is no wiring site. The JSX element
                    // IS the call. (The map is explicit about the asymmetry.)
                    &[("via", tag)],
                ));
            }
        }
        edges
    });

    edges.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
    Ok(edges)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn regexes_compile() {
        assert!(SET_STATE.is_match("this.setState({ x: 1 })"));
        assert!(SET_STATE.is_match("this.setState ("));

        let tags: Vec<&str> = JSX_TAG
            .captures_iter("<div><StaticCanvas /><span>x</span><Foo bar={1}>")
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(
            tags,
            vec!["StaticCanvas", "Foo"],
            "capital initial = component; `div`/`span` are DOM and correctly ignored"
        );
    }
}
