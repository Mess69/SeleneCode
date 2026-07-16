//! Synthesizer 1/5 — callback / field-observer channels.
//!
//! # The hole
//!
//! ```text
//! class Scene {
//!   private callbacks = new Set<Callback>();
//!   onUpdate(cb)     { this.callbacks.add(cb); }        // REGISTRAR
//!   triggerUpdate()  { for (const cb of this.callbacks) cb(); }  // DISPATCHER
//! }
//! this.scene.onUpdate(this.triggerRender);              // REGISTRATION SITE
//! ```
//!
//! `triggerUpdate → triggerRender` exists at runtime and **not in the AST**:
//! `triggerUpdate`'s only literal call is `cb()`, which is anonymous.
//!
//! # Why this is a pass and not a `resolve()`
//!
//! `resolve(ref)` answers "what does this **named** ref point to", one ref at a
//! time. Here there is **no ref to resolve** — and the answer needs three places
//! correlated across files: the registrar (`onUpdate`), the registration site
//! (`this.scene.onUpdate(this.triggerRender)`) and the dispatcher
//! (`triggerUpdate`). That is a whole-graph pass, by construction.
//!
//! # Divergences from the original design — contract, not bugs
//!
//! - **Pair by same FILE + same FIELD**, not by class. The design said class; the
//!   TS build used the file as a class proxy because getting the containing class
//!   reliably was harder. Multi-class files over-pair; accepted, and kept here so
//!   the behavior the fixtures were validated against is preserved.
//! - **Regex arg recovery**, not a tree-sitter re-parse: the registered callback
//!   is recovered by reading the caller's source line. **Named args only** —
//!   `onUpdate(() => …)` is missed by design (the anonymous frontier; attributing
//!   the edge to the enclosing function would be a *wrong* edge, not a partial
//!   one).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Edge, EdgeKind, Node, NodeKind};
use selene_db::GraphStore;

use super::{node_body, stream_nodes_by_kind, synth_edge};
use crate::Result;
use crate::context::ResolutionContext;

/// Per registrar. Beyond this the channel is a bus, not an observer, and the
/// edges stop being informative.
pub const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

/// `onUpdate`, `subscribe`, `addListener`, …
static REGISTRAR_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(
        r"^(on[A-Z]\w*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$",
    )
    .unwrap()
});

/// The registrar's body must actually *store* the callback in a field.
static REGISTRAR_BODY: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"this\.(\w+)\.(?:add|push|set)\(").unwrap()
});

/// `emit`, `trigger`, `notify`, `dispatch`, `fire`, `publish`, `flush` — as a
/// case-insensitive *substring* of the name.
static DISPATCHER_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"(?i)(emit|trigger|notify|dispatch|fire|publish|flush)").unwrap()
});

/// The dispatcher's body must iterate the field: `for (… of this.F)` …
static DISPATCHER_FOR: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"\bof\s+(?:Array\.from\(\s*)?this\.(\w+)").unwrap()
});
/// …or `this.F.forEach(`.
static DISPATCHER_FOREACH: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"this\.(\w+)\.forEach\(").unwrap()
});

/// A channel: one registrar + one dispatcher over the same field, in one file.
struct Channel {
    registrar: Node,
    dispatcher: Node,
    field: String,
}

pub async fn run<S: GraphStore>(store: &S, ctx: &dyn ResolutionContext) -> Result<Vec<Edge>> {
    // --- (1) async: stream the candidates ------------------------------------
    // Stream — never materialize an unbounded kind (#610).
    let mut candidates: Vec<Node> = stream_nodes_by_kind(store, NodeKind::Method).await?;
    candidates.extend(stream_nodes_by_kind(store, NodeKind::Function).await?);

    // --- (2) sync: classify, over the CONTEXT --------------------------------
    // `ResolutionContext` is a SYNC seam that drives the async store through
    // `block_on` (see context.rs). Calling it straight from this async fn panics
    // — "cannot start a runtime from within a runtime" — because the thread is
    // currently driving tasks. `block_in_place` hands the worker's other tasks
    // off, which is exactly what makes the context's internal `block_on` legal.
    // Every ctx-touching section of every pass is wrapped this way.
    let channels: Vec<Channel> = tokio::task::block_in_place(|| classify(ctx, &candidates));

    // --- (3) async: who called each registrar? -------------------------------
    let mut incoming_per_channel = Vec::with_capacity(channels.len());
    for ch in &channels {
        incoming_per_channel.push(store.incoming(&ch.registrar.id, &[EdgeKind::Calls]).await?);
    }

    // --- (4) sync: recover the registered callback at each site ---------------
    let mut edges = tokio::task::block_in_place(|| {
        let mut edges = Vec::new();
        for (ch, incoming) in channels.iter().zip(&incoming_per_channel) {
            // The arg-recovery regex is built per registrar name.
            let Ok(arg_re) = Regex::new(&format!(
                r"{}\s*\(\s*(?:this\.)?(\w+)",
                regex::escape(&ch.registrar.name)
            )) else {
                continue; // a name that will not escape into a regex: skip, never panic
            };

            // Deterministic: sort the registration sites BEFORE the cap, so the
            // 40 that survive are always the same 40. (The TS build's order was
            // incidental; making it explicit is a deliberate deviation.)
            let mut sites: Vec<(String, u32, String)> = Vec::new();
            for entry in incoming {
                let caller = &entry.node;
                let Some(line) = entry.edge.line else {
                    continue;
                };
                let Some(lines) = ctx.file_lines(&caller.file_path) else {
                    continue;
                };
                let Some(text) = lines.get(line.saturating_sub(1) as usize) else {
                    continue;
                };
                let Some(c) = arg_re.captures(text) else {
                    continue;
                };
                sites.push((caller.file_path.clone(), line, c[1].to_string()));
            }
            sites.sort();
            sites.dedup();

            for (caller_file, line, arg) in sites.into_iter().take(MAX_CALLBACKS_PER_CHANNEL) {
                // Resolve the arg by name. AMBIGUOUS ⇒ SKIP — never guess which
                // `triggerRender` was meant.
                let group = ctx.nodes_by_name(&arg);
                let hits: Vec<_> = group
                    .iter()
                    .filter(|n| matches!(n.kind, NodeKind::Method | NodeKind::Function))
                    .collect();
                if hits.len() != 1 {
                    continue;
                }

                edges.push(synth_edge(
                    &ch.dispatcher.id,
                    &hits[0].id,
                    "callback",
                    Some(ch.dispatcher.start_line),
                    &[
                        ("via", ch.registrar.name.clone()),
                        ("field", ch.field.clone()),
                        ("registeredAt", format!("{caller_file}:{line}")),
                    ],
                ));
            }
        }
        edges
    });

    // Deterministic output order.
    edges.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
    Ok(edges)
}

/// Registrar × dispatcher, paired by **same file + same field**.
fn classify(ctx: &dyn ResolutionContext, candidates: &[Node]) -> Vec<Channel> {
    let mut registrars: BTreeMap<(String, String), Node> = BTreeMap::new();
    let mut dispatchers: BTreeMap<(String, String), Node> = BTreeMap::new();

    for n in candidates {
        let Some(body) = node_body(ctx, n) else {
            continue;
        };
        // Cheap pre-gate before any regex (#1235).
        if !body.contains("this.") {
            continue;
        }

        if REGISTRAR_NAME.is_match(&n.name)
            && let Some(c) = REGISTRAR_BODY.captures(&body)
        {
            registrars
                .entry((n.file_path.clone(), c[1].to_string()))
                .or_insert_with(|| n.clone());
        }

        if DISPATCHER_NAME.is_match(&n.name) {
            let field = DISPATCHER_FOR
                .captures(&body)
                .or_else(|| DISPATCHER_FOREACH.captures(&body))
                .map(|c| c[1].to_string());
            if let Some(field) = field {
                dispatchers
                    .entry((n.file_path.clone(), field))
                    .or_insert_with(|| n.clone());
            }
        }
    }

    registrars
        .iter()
        .filter_map(|((file, field), registrar)| {
            dispatchers
                .get(&(file.clone(), field.clone()))
                .map(|dispatcher| Channel {
                    registrar: registrar.clone(),
                    dispatcher: dispatcher.clone(),
                    field: field.clone(),
                })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn regexes_compile() {
        assert!(REGISTRAR_NAME.is_match("onUpdate"));
        assert!(REGISTRAR_NAME.is_match("subscribe"));
        assert!(
            !REGISTRAR_NAME.is_match("only"),
            "`on` + lowercase is not a registrar"
        );
        assert!(REGISTRAR_BODY.is_match("this.callbacks.add(cb)"));
        assert!(DISPATCHER_NAME.is_match("triggerUpdate"));
        assert!(DISPATCHER_NAME.is_match("emitAll"));
        assert!(DISPATCHER_FOR.is_match("for (const cb of this.callbacks)"));
        assert!(DISPATCHER_FOR.is_match("of Array.from(this.callbacks)"));
        assert!(DISPATCHER_FOREACH.is_match("this.callbacks.forEach("));
    }
}
