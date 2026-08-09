//! `selene viz` — turn the whole code graph into ONE self-contained, offline
//! HTML page: a dark "galaxy" of nodes + links rendered by a dependency-free 2D
//! force-directed graph on `<canvas>`.
//!
//! This module is the pure transform + template half (no I/O): given the graph's
//! [`Node`]s and [`Edge`]s it (1) drops low-signal kinds, (2) ranks the rest by
//! graph degree and caps to `max_nodes` so a browser can actually draw it, (3)
//! serializes a compact `{nodes, links, meta}` JSON, and (4) inlines it into a
//! template that carries all of its CSS/JS inline — no CDN, no `src=`, no fetch.
//! `cmd::viz` owns opening the store and writing the file.
//!
//! **Why a cap.** A large repo is ~350k nodes; no browser force-sim survives that.
//! The default keeps the most-connected ~2000 nodes (the structural backbone) and
//! the page states "showing N of M". `--all-kinds`/`--max-nodes` widen it.

use std::collections::{HashMap, HashSet};

use selene_core::{Edge, EdgeKind, Node, NodeKind};

/// Options controlling the export (mirrors the `viz` subcommand flags).
pub struct VizOptions {
    /// Hard cap on rendered nodes (most-connected first). Always ≥ 1.
    pub max_nodes: usize,
    /// Keep the low-signal kinds ([`is_low_signal`]) that are dropped by default.
    pub all_kinds: bool,
    /// Human label for the page header (the project root path).
    pub root_label: String,
    /// Live mode: the page polls `/data` and animates graph changes in place.
    /// Only `selene viz --watch` sets this — a static export stays inert.
    pub watch: bool,
}

/// The rendered page plus the counts `cmd::viz` echoes to the user.
pub struct VizDoc {
    pub html: String,
    pub shown_nodes: usize,
    pub total_nodes: usize,
    pub shown_edges: usize,
    pub total_edges: usize,
}

/// Kinds dropped from the default view: high-count, low-signal structural noise.
/// A file, an import, a local variable, or a parameter rarely carries the *flow*
/// a galaxy is meant to show, and there are a lot of them. `--all-kinds` keeps them.
fn is_low_signal(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Variable | NodeKind::Parameter
    )
}

/// Build the self-contained HTML page from the full graph.
///
/// Selection strategy: degree is counted over **all** edges (so a hub stays a hub
/// even if some of its neighbors get dropped), kinds are filtered, the survivors
/// are sorted by degree (desc; name then id break ties, so the output is
/// deterministic) and truncated to `max_nodes`. Links are kept only when *both*
/// endpoints survived, self-loops dropped, and each `(source, target, kind)`
/// de-duplicated.
/// Is this path test/vendored/generated noise — code a first map should not
/// show? (The consensus default across dependency-cruiser/madge/NDepend/
/// typescript-graph: third-party and test scaffolding are excluded up front,
/// with the hidden count surfaced so the map is trusted.) Segment and
/// filename checks, no regex needed.
fn is_noise_path(path: &str) -> bool {
    const NOISE_DIRS: [&str; 22] = [
        "node_modules",
        "vendor",
        "third_party",
        "dist",
        "build",
        "target",
        "generated",
        "__generated__",
        "__tests__",
        "tests",
        "test",
        "spec",
        "specs",
        "__mocks__",
        "mocks",
        "fixtures",
        "fixture",
        "e2e",
        "examples",
        "example",
        "docs",
        "doc",
    ];
    let file = path.rsplit('/').next().unwrap_or(path);
    if path
        .split('/')
        .take(path.split('/').count().saturating_sub(1))
        .any(|seg| NOISE_DIRS.contains(&seg))
    {
        return true;
    }
    for pat in [
        ".test.",
        ".spec.",
        ".mock.",
        ".stories.",
        "_test.",
        "_spec.",
    ] {
        if file.contains(pat) {
            return true;
        }
    }
    file.ends_with(".d.ts") || file.ends_with(".min.js")
}

/// The module (directory-prefix group) of a path at `depth` segments.
fn module_of(path: &str, depth: usize) -> String {
    let dir_end = path.rfind('/').unwrap_or(0);
    let dir = &path[..dir_end];
    if dir.is_empty() {
        return "(root)".to_string();
    }
    let segs: Vec<&str> = dir.split('/').collect();
    segs[..depth.min(segs.len())].join("/")
}

/// The transformed graph data (the page's `DATA` object) plus the counts —
/// what `--watch` re-serves on every index change without re-rendering HTML.
pub struct VizData {
    pub json: serde_json::Value,
    pub shown_nodes: usize,
    pub total_nodes: usize,
    pub shown_edges: usize,
    pub total_edges: usize,
}

pub fn build_data(nodes: &[Node], edges: &[Edge], opts: &VizOptions) -> VizData {
    let total_nodes = nodes.len();
    let total_edges = edges.len();
    let max_nodes = opts.max_nodes.max(1);

    // Degree over the full edge set — the importance signal for the cap.
    let mut degree: HashMap<&str, u32> = HashMap::new();
    for e in edges {
        *degree.entry(e.source.as_str()).or_default() += 1;
        *degree.entry(e.target.as_str()).or_default() += 1;
    }

    // --- the noise pass -----------------------------------------------------
    // Test/vendored/generated code never makes the first map; the count is
    // shipped so the page can say "N hidden" instead of silently lying.
    let noise_hidden = nodes
        .iter()
        .filter(|n| !is_low_signal(n.kind) && is_noise_path(&n.file_path))
        .count();

    let mut kept: Vec<&Node> = nodes
        .iter()
        .filter(|n| opts.all_kinds || !is_low_signal(n.kind))
        .filter(|n| !is_noise_path(&n.file_path))
        .collect();
    kept.sort_by(|a, b| {
        let da = degree.get(a.id.as_str()).copied().unwrap_or(0);
        let db = degree.get(b.id.as_str()).copied().unwrap_or(0);
        db.cmp(&da)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    kept.truncate(max_nodes);

    // --- the module map (the DEFAULT view) ----------------------------------
    // Aggregate the FULL app graph (all kept-quality nodes, not just the
    // capped symbol set) into directory-prefix modules — the unit every
    // surviving code-map tool defaults to. Depth auto-tunes: the deepest
    // prefix that still lands at a readable module count.
    let app_nodes: Vec<&Node> = nodes
        .iter()
        .filter(|n| !is_low_signal(n.kind) && !is_noise_path(&n.file_path))
        .collect();
    let mut mod_depth = 1usize;
    for d in (1..=4).rev() {
        let count = app_nodes
            .iter()
            .map(|n| module_of(&n.file_path, d))
            .collect::<HashSet<_>>()
            .len();
        if count <= 36 {
            mod_depth = d;
            break;
        }
    }
    // Module indices are assigned over the SORTED label set — the store returns
    // nodes in nondeterministic order, and `--watch` compares serialized output
    // to detect real change, so the transform must be a pure function of the
    // graph, not of iteration order.
    let mut mod_labels: Vec<String> = app_nodes
        .iter()
        .map(|n| module_of(&n.file_path, mod_depth))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    mod_labels.sort();
    let mod_index: HashMap<&str, usize> = mod_labels
        .iter()
        .enumerate()
        .map(|(i, l)| (l.as_str(), i))
        .collect();
    let mut mod_members: Vec<u32> = vec![0; mod_labels.len()];
    let mut node_mod: HashMap<&str, usize> = HashMap::new();
    for n in &app_nodes {
        let m = module_of(&n.file_path, mod_depth);
        let idx = mod_index[m.as_str()];
        mod_members[idx] += 1;
        node_mod.insert(n.id.as_str(), idx);
    }
    // Cross-module edge counts (directed) + intra counts, over the FULL edges.
    let mut pair_counts: HashMap<(usize, usize), u32> = HashMap::new();
    let mut intra_counts: Vec<u32> = vec![0; mod_members.len()];
    for e in edges {
        if let (Some(&sm), Some(&tm)) = (
            node_mod.get(e.source.as_str()),
            node_mod.get(e.target.as_str()),
        ) {
            if sm == tm {
                intra_counts[sm] += 1;
            } else {
                *pair_counts.entry((sm, tm)).or_default() += 1;
            }
        }
    }
    let modules_json: Vec<serde_json::Value> = mod_labels
        .iter()
        .enumerate()
        .map(|(i, p)| serde_json::json!({ "p": p, "n": mod_members[i], "e": intra_counts[i] }))
        .collect();
    let mut mod_links_sorted: Vec<((usize, usize), u32)> = pair_counts.into_iter().collect();
    mod_links_sorted.sort_by_key(|((s, t), _)| (*s, *t)); // deterministic output
    let mod_links_json: Vec<serde_json::Value> = mod_links_sorted
        .iter()
        .map(|((s, t), c)| serde_json::json!({ "s": s, "t": t, "c": c }))
        .collect();

    // --- communities: the REAL clusters of the call graph -------------------
    // Louvain over the full app graph (the same node set as the module map),
    // on every edge kind EXCEPT `contains` — contains mirrors the file
    // hierarchy, which is exactly the signal communities exist to complement.
    let app_index: HashMap<&str, usize> = app_nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let comm_edges: Vec<(usize, usize)> = edges
        .iter()
        .filter(|e| e.kind != EdgeKind::Contains)
        .filter_map(|e| {
            Some((
                *app_index.get(e.source.as_str())?,
                *app_index.get(e.target.as_str())?,
            ))
        })
        .collect();
    let communities = crate::analysis::detect_communities(app_nodes.len(), &comm_edges);
    // label each community by its dominant module (ties -> lexicographic)
    let n_comms = communities.iter().copied().max().map_or(0, |m| m + 1);
    let mut comm_size = vec![0u32; n_comms];
    let mut comm_mods: Vec<HashMap<String, u32>> = vec![HashMap::new(); n_comms];
    for (i, n) in app_nodes.iter().enumerate() {
        let c = communities[i];
        comm_size[c] += 1;
        *comm_mods[c]
            .entry(module_of(&n.file_path, mod_depth))
            .or_default() += 1;
    }
    let communities_json: Vec<serde_json::Value> = (0..n_comms)
        .filter(|&c| comm_size[c] >= 2)
        .take(12)
        .map(|c| {
            let mut mods: Vec<(&str, u32)> =
                comm_mods[c].iter().map(|(k, v)| (k.as_str(), *v)).collect();
            mods.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            serde_json::json!({
                "id": c, "n": comm_size[c],
                "l": mods.first().map(|(m, _)| *m).unwrap_or("(root)"),
            })
        })
        .collect();

    // id -> dense index into the emitted node array.
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(kept.len());
    for (i, n) in kept.iter().enumerate() {
        index.insert(n.id.as_str(), i);
    }

    let nodes_json: Vec<serde_json::Value> = kept
        .iter()
        .map(|n| {
            serde_json::json!({
                "i": n.id,
                "n": n.name,
                "k": n.kind.as_str(),
                "f": n.file_path,
                "l": n.start_line,
                "d": degree.get(n.id.as_str()).copied().unwrap_or(0),
                "m": node_mod.get(n.id.as_str()).map(|i| *i as i64).unwrap_or(-1),
                "c": app_index
                    .get(n.id.as_str())
                    .map(|&i| communities[i] as i64)
                    .unwrap_or(-1),
            })
        })
        .collect();

    // Dedup + SORT — edge iteration order is store-dependent, output must not be.
    let mut link_rows: Vec<(usize, usize, &str)> = Vec::new();
    let mut seen: HashSet<(usize, usize, &str)> = HashSet::new();
    for e in edges {
        if let (Some(&s), Some(&t)) = (index.get(e.source.as_str()), index.get(e.target.as_str())) {
            if s == t {
                continue; // self-loops add clutter, not signal
            }
            if seen.insert((s, t, e.kind.as_str())) {
                link_rows.push((s, t, e.kind.as_str()));
            }
        }
    }
    link_rows.sort_unstable();
    let links_json: Vec<serde_json::Value> = link_rows
        .iter()
        .map(|(s, t, k)| serde_json::json!({ "s": s, "t": t, "k": k }))
        .collect();

    let shown_nodes = nodes_json.len();
    let shown_edges = links_json.len();

    // --- god-nodes: the 5 most-connected kept symbols, with direction split.
    // `kept` is already sorted by degree desc (name, id tie-broken).
    let mut in_deg: HashMap<&str, u32> = HashMap::new();
    let mut out_deg: HashMap<&str, u32> = HashMap::new();
    for e in edges {
        *out_deg.entry(e.source.as_str()).or_default() += 1;
        *in_deg.entry(e.target.as_str()).or_default() += 1;
    }
    let hubs_json: Vec<serde_json::Value> = kept
        .iter()
        .filter(|n| degree.get(n.id.as_str()).copied().unwrap_or(0) > 0)
        .take(5)
        .map(|n| {
            serde_json::json!({
                "i": index[n.id.as_str()], "n": n.name, "f": n.file_path,
                "d": degree.get(n.id.as_str()).copied().unwrap_or(0),
                "in": in_deg.get(n.id.as_str()).copied().unwrap_or(0),
                "out": out_deg.get(n.id.as_str()).copied().unwrap_or(0),
            })
        })
        .collect();
    // --- surprises: cross-module pairs carried by ≤2 edges — rare bridges a
    // reader would never guess from the directory layout.
    let mut rare: Vec<((usize, usize), u32)> = mod_links_sorted.clone();
    rare.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let surprises_json: Vec<serde_json::Value> = rare
        .iter()
        .filter(|(_, c)| *c <= 2)
        .take(5)
        .map(|((s, t), c)| serde_json::json!({ "s": mod_labels[*s], "t": mod_labels[*t], "c": c }))
        .collect();

    let json = serde_json::json!({
        "gen": 0,
        "nodes": nodes_json,
        "links": links_json,
        "modules": modules_json,
        "modLinks": mod_links_json,
        "communities": communities_json,
        "meta": {
            "shown": shown_nodes,
            "total": total_nodes,
            "edges": shown_edges,
            "totalEdges": total_edges,
            "root": opts.root_label,
            "maxNodes": max_nodes,
            "allKinds": opts.all_kinds,
            "noiseHidden": noise_hidden,
            "watch": opts.watch,
            "hubs": hubs_json,
            "surprises": surprises_json,
        }
    });

    VizData {
        json,
        shown_nodes,
        total_nodes,
        shown_edges,
        total_edges,
    }
}

/// Render the page around an already-built [`VizData`] JSON.
pub fn render(data: &serde_json::Value, title: &str) -> String {
    // `to_string` on an owned `Value` only fails on a non-string map key, which
    // this shape never has — fall back to an empty graph rather than unwrap.
    let data_str = serde_json::to_string(data)
        .unwrap_or_else(|_| r#"{"nodes":[],"links":[],"meta":{}}"#.to_string());
    // The JSON is embedded as a JS object literal inside <script>. Escaping every
    // '<' to its < form (valid JS-in-string) makes a "</script>" breakout
    // impossible regardless of what a symbol name or file path contains.
    let data_str = data_str.replace('<', "\\u003c");

    TEMPLATE
        .replace("__DATA__", &data_str)
        .replace("__TITLE__", &html_escape(title))
}

pub fn build_html(nodes: &[Node], edges: &[Edge], opts: &VizOptions) -> VizDoc {
    let data = build_data(nodes, edges, opts);
    VizDoc {
        html: render(&data.json, &opts.root_label),
        shown_nodes: data.shown_nodes,
        total_nodes: data.total_nodes,
        shown_edges: data.shown_edges,
        total_edges: data.total_edges,
    }
}

/// Minimal HTML-text escape for the one interpolated value (the title).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The whole page: one file, everything inline. `__DATA__` (a JS object literal)
/// and `__TITLE__` (HTML-escaped text) are the only substitution points.
const TEMPLATE: &str = include_str!("viz-template.html");

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use selene_core::{EdgeKind, Language};

    fn node(id: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/lib.rs".to_string(),
            language: Language::Rust,
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

    fn edge(s: &str, t: &str) -> Edge {
        Edge {
            source: s.to_string(),
            target: t.to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: None,
            column: None,
            provenance: None,
        }
    }

    fn opts(max: usize, all: bool) -> VizOptions {
        VizOptions {
            max_nodes: max,
            all_kinds: all,
            root_label: "/tmp/demo".to_string(),
            watch: false,
        }
    }

    #[test]
    fn low_signal_kinds_dropped_by_default() {
        let nodes = vec![
            node("function:a", "a", NodeKind::Function),
            node("variable:v", "v", NodeKind::Variable),
            node("import:i", "i", NodeKind::Import),
        ];
        let doc = build_html(&nodes, &[], &opts(2000, false));
        assert_eq!(
            doc.shown_nodes, 1,
            "only the function survives the default filter"
        );
        assert_eq!(doc.total_nodes, 3);
        // ...and are kept when --all-kinds is set
        let doc_all = build_html(&nodes, &[], &opts(2000, true));
        assert_eq!(doc_all.shown_nodes, 3);
    }

    #[test]
    fn cap_keeps_the_highest_degree_nodes() {
        // b is the hub (degree 2); a and c have degree 1 each.
        let nodes = vec![
            node("function:a", "a", NodeKind::Function),
            node("function:b", "b", NodeKind::Function),
            node("function:c", "c", NodeKind::Function),
        ];
        let edges = vec![
            edge("function:b", "function:a"),
            edge("function:b", "function:c"),
        ];
        let doc = build_html(&nodes, &edges, &opts(1, false));
        assert_eq!(doc.shown_nodes, 1);
        // the surviving node must be the hub — its data is embedded in the page
        assert!(
            doc.html.contains("\"n\":\"b\""),
            "the hub 'b' should be the kept node"
        );
    }

    #[test]
    fn links_kept_only_when_both_endpoints_survive_and_are_deduped() {
        let nodes = vec![
            node("function:a", "a", NodeKind::Function),
            node("function:b", "b", NodeKind::Function),
        ];
        // one real edge, plus a duplicate, plus a dangling edge to a dropped node
        let edges = vec![
            edge("function:a", "function:b"),
            edge("function:a", "function:b"),
            edge("function:a", "variable:gone"),
        ];
        let doc = build_html(&nodes, &edges, &opts(2000, false));
        assert_eq!(doc.shown_nodes, 2);
        assert_eq!(doc.shown_edges, 1, "dupe collapsed, dangling edge dropped");
    }

    #[test]
    fn page_is_self_contained_and_carries_the_data() {
        let nodes = vec![node("function:main", "main", NodeKind::Function)];
        let doc = build_html(&nodes, &[], &opts(2000, false));
        let h = &doc.html;
        assert!(h.starts_with("<!doctype html>"));
        // no external resources of any kind
        assert!(
            !h.contains("http://") && !h.contains("https://"),
            "no network URLs"
        );
        assert!(!h.contains("src="), "no external script/style src");
        assert!(
            !h.contains("__DATA__") && !h.contains("__TITLE__"),
            "placeholders substituted"
        );
        assert!(h.contains("\"n\":\"main\""), "node data embedded inline");
    }

    fn node_in(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        let mut n = node(id, name, kind);
        n.file_path = file.to_string();
        n
    }

    #[test]
    fn communities_color_the_call_graph_not_the_directories() {
        // Two call-triangles that CROSS directories — Louvain must find them.
        let nodes = vec![
            node_in("function:a", "a", NodeKind::Function, "src/x/a.rs"),
            node_in("function:b", "b", NodeKind::Function, "src/y/b.rs"),
            node_in("function:c", "c", NodeKind::Function, "src/x/c.rs"),
            node_in("function:d", "d", NodeKind::Function, "src/y/d.rs"),
            node_in("function:e", "e", NodeKind::Function, "src/x/e.rs"),
            node_in("function:f", "f", NodeKind::Function, "src/y/f.rs"),
        ];
        let edges = vec![
            edge("function:a", "function:b"),
            edge("function:b", "function:c"),
            edge("function:c", "function:a"),
            edge("function:d", "function:e"),
            edge("function:e", "function:f"),
            edge("function:f", "function:d"),
            edge("function:c", "function:d"), // the bridge
        ];
        let data = build_data(&nodes, &edges, &opts(2000, false));
        let comms = data.json["communities"].as_array().unwrap();
        assert_eq!(comms.len(), 2, "two clusters, though the dirs interleave");
        let nodes_json = data.json["nodes"].as_array().unwrap();
        let c_of = |name: &str| {
            nodes_json.iter().find(|n| n["n"] == name).unwrap()["c"]
                .as_i64()
                .unwrap()
        };
        assert_eq!(c_of("a"), c_of("b"));
        assert_eq!(c_of("b"), c_of("c"));
        assert_ne!(c_of("a"), c_of("d"));
    }

    #[test]
    fn hubs_name_the_most_connected_symbols_with_direction() {
        let nodes = vec![
            node("function:hub", "hub", NodeKind::Function),
            node("function:x", "x", NodeKind::Function),
            node("function:y", "y", NodeKind::Function),
        ];
        let edges = vec![
            edge("function:hub", "function:x"),
            edge("function:hub", "function:y"),
            edge("function:x", "function:hub"),
        ];
        let data = build_data(&nodes, &edges, &opts(2000, false));
        let hubs = data.json["meta"]["hubs"].as_array().unwrap();
        assert_eq!(hubs[0]["n"], "hub");
        assert_eq!(hubs[0]["out"], 2);
        assert_eq!(hubs[0]["in"], 1);
    }

    #[test]
    fn surprises_are_the_rarest_cross_module_pairs() {
        // x<->y heavily linked (3 edges), x->z linked ONCE — z is the surprise.
        let nodes = vec![
            node_in("function:a", "a", NodeKind::Function, "x/a.rs"),
            node_in("function:b", "b", NodeKind::Function, "y/b.rs"),
            node_in("function:c", "c", NodeKind::Function, "z/c.rs"),
        ];
        let edges = vec![
            edge("function:a", "function:b"),
            edge("function:b", "function:a"),
            edge("function:a", "function:b"),
            edge("function:a", "function:c"),
        ];
        let data = build_data(&nodes, &edges, &opts(2000, false));
        let sur = data.json["meta"]["surprises"].as_array().unwrap();
        assert_eq!(sur[0]["c"], 1, "the singleton bridge ranks first");
        assert_eq!(sur[0]["t"], "z");
    }

    #[test]
    fn script_close_tag_in_a_name_cannot_break_out() {
        let nodes = vec![node("function:x", "</script><b>x", NodeKind::Function)];
        let doc = build_html(&nodes, &[], &opts(2000, false));
        // the raw sequence must not appear literally inside the embedded data
        assert!(!doc.html.contains("</script><b>x"));
        assert!(
            doc.html.contains("\\u003c/script>"),
            "the '<' was escaped to \\u003c"
        );
    }
}
