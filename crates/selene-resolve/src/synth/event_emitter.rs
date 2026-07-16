//! Synthesizer 2/5 — EventEmitter (string-keyed) channels.
//!
//! # The hole
//!
//! ```text
//! // app.ts
//! bus.on('mount', function onmount() { initApp(); });   // REGISTRATION
//! class Application { use() { bus.emit('mount'); } }    // DISPATCH
//! ```
//!
//! `use → onmount` exists at runtime. The correlation key is a **string
//! literal**, which the AST has no concept of as a link. Nothing connects the two
//! statically.
//!
//! # Named handlers only — a deliberate frontier, not an omission
//!
//! `bus.on('tick', () => refresh())` synthesizes **nothing**. The arrow is not a
//! node, so there is nothing to point at. The tempting "fix" — attributing the
//! edge to the *enclosing* function — would be a **wrong edge**, not a partial
//! one: it would claim the emitter calls the enclosing function, which it does
//! not. Silent beats wrong. (`callback-edge-synthesis.md`, "Remaining work #1".)
//!
//! # Fan-out cap replaces type inference
//!
//! Without receiver types, `emit('error')` in one module and `on('error', …)` in
//! forty others would over-link catastrophically. [`EVENT_FANOUT_CAP`] skips any
//! event with more than 6 dispatchers **or** more than 6 handlers: a generic
//! event name is exactly the case where the correlation carries no information.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Edge, Node, NodeKind};
use selene_db::GraphStore;

use super::{LineIndex, enclosing_fn, stream_nodes_by_kind, synth_edge};
use crate::Result;
use crate::context::ResolutionContext;

/// Skip an event entirely above this many dispatchers OR handlers. The precision
/// guard that stands in for receiver-type inference.
pub const EVENT_FANOUT_CAP: usize = 6;

/// `.emit('e'` / `.fire('e'` / `.dispatchEvent('e'`
static EMIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r#"\.(?:emit|fire|dispatchEvent)\(\s*['"]([^'"]+)['"]"#).unwrap()
});

/// `.on('e', function named` / `.on('e', handler` / `.on('e', this.handler`
///
/// **Named handlers only.** An arrow (`() =>`) or an anonymous `function (`
/// matches nothing here, by design — see the module docs.
static ON_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(
        r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:function\s+(\w+)|(?:this\.)?(\w+)\s*[,)])"#,
    )
    .unwrap()
});

pub async fn run<S: GraphStore>(store: &S, ctx: &dyn ResolutionContext) -> Result<Vec<Edge>> {
    // --- async: the node universe the correlation resolves against -----------
    let mut fns: Vec<Node> = stream_nodes_by_kind(store, NodeKind::Method).await?;
    fns.extend(stream_nodes_by_kind(store, NodeKind::Function).await?);
    fns.extend(stream_nodes_by_kind(store, NodeKind::Component).await?);

    // --- sync: file-oriented scan (see the seam note in synth/mod.rs) --------
    let edges = tokio::task::block_in_place(|| {
        // event -> dispatchers / (handler, site)
        let mut dispatchers: BTreeMap<String, Vec<Node>> = BTreeMap::new();
        let mut handlers: BTreeMap<String, Vec<(Node, String, u32)>> = BTreeMap::new();

        // `all_files()` is sorted, so the scan order — and therefore the output
        // order — is deterministic.
        for file in ctx.all_files() {
            let Some(src) = ctx.read_file(file) else {
                continue;
            };

            // Cheap `contains` pre-gates BEFORE any regex. An ungated scan of
            // every file cost 20+ minutes on real corpora (#1235).
            let has_emit =
                src.contains(".emit(") || src.contains(".fire(") || src.contains(".dispatchEvent(");
            let has_on =
                src.contains(".on(") || src.contains(".once(") || src.contains(".addListener(");
            if !has_emit && !has_on {
                continue;
            }

            let idx = LineIndex::new(&src);

            if has_emit {
                for c in EMIT_RE.captures_iter(&src) {
                    let (Some(whole), Some(ev)) = (c.get(0), c.get(1)) else {
                        continue;
                    };
                    let line = idx.line_at(whole.start());
                    // The dispatcher is the function the emit sits INSIDE.
                    if let Some(f) = enclosing_fn(&fns, file, line) {
                        dispatchers
                            .entry(ev.as_str().to_string())
                            .or_default()
                            .push(f);
                    }
                }
            }

            if has_on {
                for c in ON_RE.captures_iter(&src) {
                    let (Some(whole), Some(ev)) = (c.get(0), c.get(1)) else {
                        continue;
                    };
                    // group 2 = `function named`, group 3 = a bare/`this.` name.
                    let Some(name) = c.get(2).or_else(|| c.get(3)) else {
                        continue;
                    };
                    let line = idx.line_at(whole.start());
                    // Resolve the handler NAME to a node. Ambiguous ⇒ skip.
                    let hits: Vec<_> = ctx
                        .nodes_by_name(name.as_str())
                        .into_iter()
                        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
                        .collect();
                    if hits.len() != 1 {
                        continue;
                    }
                    handlers.entry(ev.as_str().to_string()).or_default().push((
                        hits[0].as_ref().clone(),
                        file.clone(),
                        line,
                    ));
                }
            }
        }

        // --- correlate by the event literal ---------------------------------
        let mut edges = Vec::new();
        for (event, ds) in &dispatchers {
            let Some(hs) = handlers.get(event) else {
                continue;
            };
            // The precision guard. A generic name (`error`, `change`, `data`) is
            // exactly where the string correlation stops carrying information.
            if ds.len() > EVENT_FANOUT_CAP || hs.len() > EVENT_FANOUT_CAP {
                continue;
            }

            for d in ds {
                for (h, site_file, site_line) in hs {
                    if d.id == h.id {
                        continue; // a handler that emits its own event
                    }
                    edges.push(synth_edge(
                        &d.id,
                        &h.id,
                        "event-emitter",
                        // NO `line` on this edge — the map is explicit.
                        None,
                        &[
                            ("event", event.clone()),
                            // The WIRING site is the `on(` call, not the emit.
                            ("registeredAt", format!("{site_file}:{site_line}")),
                        ],
                    ));
                }
            }
        }
        edges
    });

    let mut edges = edges;
    edges.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
    edges.dedup_by(|a, b| a.source == b.source && a.target == b.target);
    Ok(edges)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn regexes_compile() {
        assert!(EMIT_RE.is_match("bus.emit('mount')"));
        assert!(EMIT_RE.is_match("el.dispatchEvent(\"x\")"));

        let c = ON_RE
            .captures("bus.on('mount', function onmount() {")
            .unwrap();
        assert_eq!(&c[1], "mount");
        assert_eq!(c.get(2).unwrap().as_str(), "onmount");

        let c = ON_RE.captures("bus.on('tick', handleTick)").unwrap();
        assert_eq!(c.get(3).unwrap().as_str(), "handleTick");

        let c = ON_RE.captures("bus.on('x', this.handler)").unwrap();
        assert_eq!(c.get(3).unwrap().as_str(), "handler");
    }

    /// The frontier, asserted: an arrow or an anonymous `function` matches
    /// NOTHING. Do not "fix" this by linking to the enclosing function — that
    /// would be a wrong edge, not a partial one.
    #[test]
    fn anonymous_handlers_match_nothing_by_design() {
        assert!(
            ON_RE.captures("bus.on('tick', () => refresh())").is_none(),
            "an arrow handler is the deliberate frontier — silent beats wrong"
        );
        assert!(
            ON_RE.captures("bus.on('tick', function () {})").is_none(),
            "an anonymous function is not a node; there is nothing to point at"
        );
    }
}
