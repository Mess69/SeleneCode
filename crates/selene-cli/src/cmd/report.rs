//! `selene report` — the graph's executive summary as a markdown file: the
//! god-nodes, the call-graph clusters, the module cycles, the orphan modules,
//! the rare bridges, and the questions worth asking `selene explore` first.
//!
//! Pure transform ([`render_report`]) + the same store-opening choreography as
//! `cmd::viz`. Deterministic: every section sorts on stable keys, so the same
//! graph always writes the same bytes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use selene_core::{Edge, EdgeKind, Node};
use selene_db::SurrealStore;

use crate::analysis::{detect_communities, strongly_connected_components};
use crate::exit::Outcome;
use crate::viz::{auto_mod_depth, is_low_signal, is_noise_path, module_of};

use super::query_root_direct;

/// `selene report` — read the graph, render the summary, write the file.
/// The written path goes to **stdout** (scripts capture it); the summary line
/// goes to stderr, like `viz`.
pub async fn report(path: Option<PathBuf>, out: Option<PathBuf>) -> Outcome {
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match report_inner(&root, out).await {
        Ok(dest) => {
            eprintln!("selene report: written");
            println!("{}", dest.display());
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene report: {e:#}");
            Outcome::Failure
        }
    }
}

async fn report_inner(root: &Path, out: Option<PathBuf>) -> Result<PathBuf> {
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("could not open the index")?;
    let nodes = store.all_nodes().await.context("read nodes")?;
    let edges = store.all_edges().await.context("read edges")?;
    let md = render_report(&nodes, &edges, &root.display().to_string());
    let dest = out.unwrap_or_else(|| root.join("GRAPH_REPORT.md"));
    std::fs::write(&dest, &md).with_context(|| format!("could not write {}", dest.display()))?;
    Ok(dest)
}

/// The whole report, as one pure function — testable without a store.
pub(crate) fn render_report(nodes: &[Node], edges: &[Edge], root_label: &str) -> String {
    let project = root_label
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(root_label);

    // The app-quality node set, in canonical (id) order — the same filter as
    // the viz, so the report and the galaxy describe the same graph.
    let mut app_nodes: Vec<&Node> = nodes
        .iter()
        .filter(|n| !is_low_signal(n.kind) && !is_noise_path(&n.file_path))
        .collect();
    app_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let files: HashSet<&str> = nodes.iter().map(|n| n.file_path.as_str()).collect();

    let mut in_deg: HashMap<&str, u32> = HashMap::new();
    let mut out_deg: HashMap<&str, u32> = HashMap::new();
    for e in edges {
        *out_deg.entry(e.source.as_str()).or_default() += 1;
        *in_deg.entry(e.target.as_str()).or_default() += 1;
    }
    let deg = |m: &HashMap<&str, u32>, id: &str| m.get(id).copied().unwrap_or(0);

    let mut out = String::new();
    out.push_str(&format!("# Graph report — {project}\n\n"));
    out.push_str(&format!(
        "> **{}** symbols · **{}** edges · **{}** files — generated from the \
         SeleneCode graph; regenerate with `selene report`.\n\n",
        nodes.len(),
        edges.len(),
        files.len()
    ));

    // ── god-nodes ────────────────────────────────────────────────────────────
    let mut ranked: Vec<&&Node> = app_nodes
        .iter()
        .filter(|n| deg(&in_deg, &n.id) + deg(&out_deg, &n.id) > 0)
        .collect();
    ranked.sort_by(|a, b| {
        let da = deg(&in_deg, &a.id) + deg(&out_deg, &a.id);
        let db = deg(&in_deg, &b.id) + deg(&out_deg, &b.id);
        db.cmp(&da)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    if !ranked.is_empty() {
        out.push_str("## God-nodes — the most-connected symbols\n\n");
        out.push_str("| symbol | file | in | out |\n|---|---|---:|---:|\n");
        for n in ranked.iter().take(10) {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                n.name,
                n.file_path,
                deg(&in_deg, &n.id),
                deg(&out_deg, &n.id)
            ));
        }
        out.push_str(
            "\nAn `out` of 0 with a huge `in` is plumbing — everything uses it. A large \
             `out` is an orchestrator — it drives a flow. Change the former carefully; \
             ask about the latter first.\n\n",
        );
    }

    // ── the module map (shared with the viz) ─────────────────────────────────
    let mod_depth = auto_mod_depth(&app_nodes);
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
        let idx = mod_index[module_of(&n.file_path, mod_depth).as_str()];
        mod_members[idx] += 1;
        node_mod.insert(n.id.as_str(), idx);
    }
    // `cross` (all kinds) feeds orphans and rare bridges. `cross_structural`
    // feeds the cycle detector, and is deliberately strict: **imports only,
    // tree-sitter provenance** — the sentrux/dependency-cruiser definition of
    // an architecture cycle. `references` fire on any name mention, and even
    // `calls` cross layers freely once the name-matcher and dispatch synthesis
    // have bound them — either one welds the whole workspace into a single
    // giant SCC, and a "cycle" that names every module says nothing.
    let mut cross: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    let mut cross_structural: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    for e in edges {
        if let (Some(&sm), Some(&tm)) = (
            node_mod.get(e.source.as_str()),
            node_mod.get(e.target.as_str()),
        ) && sm != tm
        {
            *cross.entry((sm, tm)).or_default() += 1;
            if e.kind == EdgeKind::Imports
                && e.provenance != Some(selene_core::Provenance::Heuristic)
            {
                *cross_structural.entry((sm, tm)).or_default() += 1;
            }
        }
    }

    // ── call-graph clusters ──────────────────────────────────────────────────
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
    let communities = detect_communities(app_nodes.len(), &comm_edges);
    let n_comms = communities.iter().copied().max().map_or(0, |m| m + 1);
    let mut comm_size = vec![0u32; n_comms];
    let mut comm_mods: Vec<BTreeMap<String, u32>> = vec![BTreeMap::new(); n_comms];
    for (i, n) in app_nodes.iter().enumerate() {
        let c = communities[i];
        comm_size[c] += 1;
        *comm_mods[c]
            .entry(module_of(&n.file_path, mod_depth))
            .or_default() += 1;
    }
    let clusters: Vec<usize> = (0..n_comms).filter(|&c| comm_size[c] >= 2).collect();
    if !clusters.is_empty() {
        out.push_str("## Call-graph clusters (Louvain)\n\n");
        for &c in clusters.iter().take(8) {
            let mut mods: Vec<(&str, u32)> =
                comm_mods[c].iter().map(|(k, v)| (k.as_str(), *v)).collect();
            mods.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let dominant = mods.first().map(|(m, _)| *m).unwrap_or("(root)");
            let span = mods.len();
            let span_note = if span >= 3 {
                format!(" — spans {span} directories: structure the tree cannot show")
            } else {
                String::new()
            };
            out.push_str(&format!(
                "- **cluster {c}** — {} symbols, mostly `{dominant}`{span_note}\n",
                comm_size[c]
            ));
        }
        out.push_str(
            "\nClusters are computed from who *calls* whom (Louvain over the call \
             graph), not from where files sit. `selene viz` colors them under the \
             **Clusters** button.\n\n",
        );
    }

    // ── module cycles ────────────────────────────────────────────────────────
    let cycle_edges: Vec<(usize, usize)> = cross_structural.keys().copied().collect();
    let sccs = strongly_connected_components(mod_labels.len(), &cycle_edges);
    out.push_str("## Module cycles (imports)\n\n");
    if sccs.is_empty() {
        out.push_str("No import cycles between modules — the layering holds.\n\n");
    } else {
        for scc in &sccs {
            let names: Vec<&str> = scc.iter().map(|&i| mod_labels[i].as_str()).collect();
            out.push_str(&format!("- {}\n", names.join(" ⇄ ")));
        }
        out.push_str(
            "\nA cycle means these modules can only be understood (and safely changed) \
             together.\n\n",
        );
    }

    // ── orphan modules ───────────────────────────────────────────────────────
    let mut connected: HashSet<usize> = HashSet::new();
    for &(s, t) in cross.keys() {
        connected.insert(s);
        connected.insert(t);
    }
    let orphans: Vec<usize> = (0..mod_labels.len())
        .filter(|i| !connected.contains(i))
        .collect();
    if !orphans.is_empty() && mod_labels.len() > 1 {
        out.push_str("## Orphan modules\n\n");
        for i in orphans {
            out.push_str(&format!(
                "- `{}` — {} symbol{}, no edges in or out of the module\n",
                mod_labels[i],
                mod_members[i],
                if mod_members[i] == 1 { "" } else { "s" }
            ));
        }
        out.push_str("\nDead code, an entry point, or an island the resolver cannot see into.\n\n");
    }

    // ── rare bridges ─────────────────────────────────────────────────────────
    let mut rare: Vec<((usize, usize), u32)> = cross.iter().map(|(k, v)| (*k, *v)).collect();
    rare.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let rare: Vec<_> = rare.into_iter().filter(|(_, c)| *c <= 2).take(8).collect();
    if !rare.is_empty() {
        out.push_str("## Rare bridges\n\n");
        for ((s, t), c) in &rare {
            out.push_str(&format!(
                "- `{}` → `{}` — {} edge{}\n",
                mod_labels[*s],
                mod_labels[*t],
                c,
                if *c == 1 { "" } else { "s" }
            ));
        }
        out.push_str(
            "\nA dependency carried by one or two edges is either an accident or a \
             load-bearing surprise — worth knowing which.\n\n",
        );
    }

    // ── questions worth asking ───────────────────────────────────────────────
    out.push_str("## Questions worth asking first\n\n");
    let mut by_in: Vec<&&Node> = app_nodes
        .iter()
        .filter(|n| deg(&in_deg, &n.id) > 0)
        .collect();
    by_in.sort_by(|a, b| {
        deg(&in_deg, &b.id)
            .cmp(&deg(&in_deg, &a.id))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(n) = by_in.first() {
        out.push_str(&format!(
            "- `selene explore \"how does {} work\"` — it sits under {} callers.\n",
            n.name,
            deg(&in_deg, &n.id)
        ));
    }
    if let Some(((s, t), c)) = rare.first() {
        out.push_str(&format!(
            "- `selene explore \"what happens between {} and {}\"` — {} edge{} \
             carr{} the whole dependency.\n",
            mod_labels[*s],
            mod_labels[*t],
            c,
            if *c == 1 { "" } else { "s" },
            if *c == 1 { "ies" } else { "y" }
        ));
    }
    if let Some(&c) = clusters.first() {
        let dominant = comm_mods[c]
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(k, _)| k.as_str())
            .unwrap_or("(root)");
        out.push_str(&format!(
            "- `selene explore \"how does {dominant} work\"` — the biggest \
             call-graph cluster ({} symbols) centers there.\n",
            comm_size[c]
        ));
    }
    out.push_str("\nEach answer arrives with source, call flow, and blast radius — no file reading needed.\n");

    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use selene_core::{Language, NodeKind};

    fn node_in(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
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

    fn import_edge(s: &str, t: &str) -> Edge {
        Edge {
            kind: EdgeKind::Imports,
            ..edge(s, t)
        }
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        // hub: called from both sides; cycle: a->b->a across modules x and y;
        // orphan: z/ has no cross-module edge at all.
        let nodes = vec![
            node_in("function:hub", "hub", NodeKind::Function, "x/hub.rs"),
            node_in("function:a", "a", NodeKind::Function, "x/a.rs"),
            node_in("function:b", "b", NodeKind::Function, "y/b.rs"),
            node_in("function:lone", "lone", NodeKind::Function, "z/lone.rs"),
        ];
        let edges = vec![
            edge("function:a", "function:hub"),
            edge("function:b", "function:hub"),
            import_edge("function:a", "function:b"),
            import_edge("function:b", "function:a"),
        ];
        (nodes, edges)
    }

    #[test]
    fn the_report_names_hubs_cycles_orphans_and_questions() {
        let (nodes, edges) = fixture();
        let md = render_report(&nodes, &edges, "/tmp/demo");
        assert!(md.contains("# Graph report"), "{md}");
        assert!(md.contains("`hub`"), "hubs section names the hub: {md}");
        assert!(
            md.contains("x ⇄ y"),
            "the module cycle is spelled out: {md}"
        );
        assert!(md.contains("`z`"), "orphan module listed: {md}");
        assert!(
            md.contains("selene explore"),
            "suggested questions teach the tool: {md}"
        );
        assert!(
            md.contains("selene report"),
            "the regenerate marker (purge keys on it): {md}"
        );
    }

    #[test]
    fn the_report_is_deterministic() {
        let (nodes, edges) = fixture();
        let mut rev_n = nodes.clone();
        rev_n.reverse();
        let mut rev_e = edges.clone();
        rev_e.reverse();
        assert_eq!(
            render_report(&nodes, &edges, "r"),
            render_report(&rev_n, &rev_e, "r")
        );
    }
}
