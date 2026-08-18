//! Graph analysis for the viz + report surfaces: community detection (Louvain)
//! and cycle detection (Tarjan SCC). Pure functions over dense-index graphs —
//! no store access, no I/O — and DETERMINISTIC: nodes are processed in
//! ascending index order and every tie breaks toward the smaller id, because
//! the viz transform must stay a pure function of the graph (`--watch` diffs
//! serialized output byte-for-byte, and a re-run must never repaint clusters).
//!
//! These run in RAM by design. The SurrealQL-max decision covers *traversal at
//! query time*; whole-graph analysis is the same species as the resolver's
//! in-memory symbol table — the store persists the graph, the analysis reads it
//! once and computes.
//!
//! [`compute_insights`] is the ONE recipe every surface shares (MCP `insights`,
//! `selene insights`, and — same ingredients — the viz Clusters mode and the
//! report): app-quality nodes sorted by id, non-`contains` edges, Louvain,
//! hub naming that skips std plumbing, betweenness for the true bottlenecks.

use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance};

// ---------------------------------------------------------------------------
// The shared graph-shaping helpers (moved from selene-cli's viz on 2026-08-18
// so the MCP surface computes the same map).
// ---------------------------------------------------------------------------

/// Kinds dropped from analysis/first-map views: high-count, low-signal noise.
pub fn is_low_signal(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Variable | NodeKind::Parameter
    )
}

/// Is this path test/vendored/generated noise a first map should not show?
pub fn is_noise_path(path: &str) -> bool {
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
pub fn module_of(path: &str, depth: usize) -> String {
    let dir_end = path.rfind('/').unwrap_or(0);
    let dir = &path[..dir_end];
    if dir.is_empty() {
        return "(root)".to_string();
    }
    let segs: Vec<&str> = dir.split('/').collect();
    segs[..depth.min(segs.len())].join("/")
}

/// The deepest directory-prefix depth (1–4) that still lands at a readable
/// module count (≤ 36).
pub fn auto_mod_depth(app_nodes: &[&Node]) -> usize {
    let mut mod_depth = 1usize;
    for d in (1..=4).rev() {
        let count = app_nodes
            .iter()
            .map(|n| module_of(&n.file_path, d))
            .collect::<std::collections::HashSet<_>>()
            .len();
        if count <= 36 {
            mod_depth = d;
            break;
        }
    }
    mod_depth
}

/// Names that make a useless hub label: language plumbing every codebase has
/// (std prelude, ubiquitous methods, dunder/constructor forms) plus anything
/// under 3 chars. Deliberately narrow — domain-plausible words never listed.
pub fn is_trivial_name(name: &str) -> bool {
    if name.chars().count() < 3 {
        return true;
    }
    const TRIVIAL: &[&str] = &[
        "Err",
        "Some",
        "None",
        "Self",
        "Result",
        "Option",
        "Vec",
        "String",
        "Box",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "usize",
        "bool",
        "new",
        "clone",
        "from",
        "into",
        "as_str",
        "as_ref",
        "to_string",
        "collect",
        "iter",
        "next",
        "unwrap",
        "expect",
        "default",
        "Default",
        "fmt",
        "Debug",
        "Display",
        "Drop",
        "len",
        "is_empty",
        "get",
        "set",
        "push",
        "insert",
        "main",
        "init",
        "drop",
        "with",
        "try_from",
        "try_into",
        "__init__",
        "__str__",
        "__repr__",
        "__new__",
        "constructor",
        "toString",
        "valueOf",
        "undefined",
        "null",
        "this",
        "super",
        "self",
        "cls",
        "prototype",
        "hashCode",
        "equals",
        "println",
        "print",
    ];
    TRIVIAL.iter().any(|t| name.eq_ignore_ascii_case(t))
}

// ---------------------------------------------------------------------------
// The one insights recipe.
// ---------------------------------------------------------------------------

/// One call-graph community, named by its most-connected informative member.
#[derive(Debug, Clone)]
pub struct CommunityInsight {
    pub id: usize,
    pub size: u32,
    pub hub: String,
    pub dominant_module: String,
    pub module_span: usize,
}

/// One structural bottleneck: a symbol ranked by betweenness centrality.
#[derive(Debug, Clone)]
pub struct HubInsight {
    pub name: String,
    pub file: String,
    pub betweenness: f64,
    pub in_deg: u32,
    pub out_deg: u32,
}

/// The full structural summary of a graph.
#[derive(Debug, Clone)]
pub struct Insights {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub app_nodes: usize,
    pub communities: Vec<CommunityInsight>,
    /// Top symbols by betweenness (the bridges degree cannot see).
    pub hubs: Vec<HubInsight>,
    /// `true` when betweenness was pivot-sampled (large graph).
    pub approx_betweenness: bool,
    /// Module-level import cycles (tree-sitter provenance only).
    pub import_cycles: Vec<Vec<String>>,
    /// Cross-module dependencies carried by ≤ 2 edges: (from, to, count).
    pub rare_bridges: Vec<(String, String, u32)>,
    /// Modules with zero cross-module edges: (label, members).
    pub orphan_modules: Vec<(String, u32)>,
}

/// Above this many app nodes, betweenness switches to deterministic pivot
/// sampling ([`betweenness_approx`], 256 pivots).
pub const EXACT_BETWEENNESS_MAX_NODES: usize = 20_000;

/// Compute [`Insights`] — deterministic, pure, the shared recipe.
pub fn compute_insights(nodes: &[Node], edges: &[Edge]) -> Insights {
    let mut app_nodes: Vec<&Node> = nodes
        .iter()
        .filter(|n| !is_low_signal(n.kind) && !is_noise_path(&n.file_path))
        .collect();
    app_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let app_index: std::collections::HashMap<&str, usize> = app_nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();

    let mut in_deg: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    let mut out_deg: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for e in edges {
        *out_deg.entry(e.source.as_str()).or_default() += 1;
        *in_deg.entry(e.target.as_str()).or_default() += 1;
    }
    let deg_of =
        |m: &std::collections::HashMap<&str, u32>, id: &str| m.get(id).copied().unwrap_or(0);

    // Dense non-`contains` edge list — the community + betweenness substrate.
    let dense: Vec<(usize, usize)> = edges
        .iter()
        .filter(|e| e.kind != EdgeKind::Contains)
        .filter_map(|e| {
            Some((
                *app_index.get(e.source.as_str())?,
                *app_index.get(e.target.as_str())?,
            ))
        })
        .collect();

    // --- communities, hub-named --------------------------------------------
    let communities = detect_communities(app_nodes.len(), &dense);
    let n_comms = communities.iter().copied().max().map_or(0, |m| m + 1);
    let mod_depth = auto_mod_depth(&app_nodes);
    let mut size = vec![0u32; n_comms];
    let mut mods: Vec<std::collections::BTreeMap<String, u32>> =
        vec![std::collections::BTreeMap::new(); n_comms];
    let mut best: Vec<Option<&Node>> = vec![None; n_comms];
    let mut best_named: Vec<Option<&Node>> = vec![None; n_comms];
    let better = |cand: &Node, cur: Option<&&Node>| match cur {
        None => true,
        Some(cur) => {
            let d = deg_of(&in_deg, &cand.id) + deg_of(&out_deg, &cand.id);
            let cd = deg_of(&in_deg, &cur.id) + deg_of(&out_deg, &cur.id);
            d > cd
                || (d == cd
                    && (cand.name.as_str(), cand.id.as_str())
                        < (cur.name.as_str(), cur.id.as_str()))
        }
    };
    for (i, n) in app_nodes.iter().enumerate() {
        let c = communities[i];
        size[c] += 1;
        *mods[c]
            .entry(module_of(&n.file_path, mod_depth))
            .or_default() += 1;
        if better(n, best[c].as_ref()) {
            best[c] = Some(n);
        }
        if !is_trivial_name(&n.name) && better(n, best_named[c].as_ref()) {
            best_named[c] = Some(n);
        }
    }
    let community_insights: Vec<CommunityInsight> = (0..n_comms)
        .filter(|&c| size[c] >= 2)
        .take(12)
        .map(|c| {
            let mut m: Vec<(&str, u32)> = mods[c].iter().map(|(k, v)| (k.as_str(), *v)).collect();
            m.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            CommunityInsight {
                id: c,
                size: size[c],
                hub: best_named[c]
                    .or(best[c])
                    .map(|n| n.name.clone())
                    .unwrap_or_default(),
                dominant_module: m.first().map(|(k, _)| k.to_string()).unwrap_or_default(),
                module_span: m.len(),
            }
        })
        .collect();

    // --- betweenness hubs ---------------------------------------------------
    let approx = app_nodes.len() > EXACT_BETWEENNESS_MAX_NODES;
    let bc = if approx {
        betweenness_approx(app_nodes.len(), &dense, 256)
    } else {
        betweenness_centrality(app_nodes.len(), &dense)
    };
    let mut ranked: Vec<usize> = (0..app_nodes.len()).filter(|&i| bc[i] > 0.0).collect();
    ranked.sort_by(|&a, &b| {
        bc[b]
            .partial_cmp(&bc[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| app_nodes[a].id.cmp(&app_nodes[b].id))
    });
    let hubs: Vec<HubInsight> = ranked
        .into_iter()
        .take(10)
        .map(|i| HubInsight {
            name: app_nodes[i].name.clone(),
            file: app_nodes[i].file_path.clone(),
            betweenness: bc[i],
            in_deg: deg_of(&in_deg, &app_nodes[i].id),
            out_deg: deg_of(&out_deg, &app_nodes[i].id),
        })
        .collect();

    // --- module map: cycles (imports, tree-sitter), rare bridges, orphans ---
    let mut mod_labels: Vec<String> = app_nodes
        .iter()
        .map(|n| module_of(&n.file_path, mod_depth))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    mod_labels.sort();
    let mod_index: std::collections::HashMap<&str, usize> = mod_labels
        .iter()
        .enumerate()
        .map(|(i, l)| (l.as_str(), i))
        .collect();
    let mut members = vec![0u32; mod_labels.len()];
    let mut node_mod: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for n in &app_nodes {
        let idx = mod_index[module_of(&n.file_path, mod_depth).as_str()];
        members[idx] += 1;
        node_mod.insert(n.id.as_str(), idx);
    }
    let mut cross: std::collections::BTreeMap<(usize, usize), u32> =
        std::collections::BTreeMap::new();
    let mut cross_imports: std::collections::BTreeMap<(usize, usize), u32> =
        std::collections::BTreeMap::new();
    for e in edges {
        if let (Some(&sm), Some(&tm)) = (
            node_mod.get(e.source.as_str()),
            node_mod.get(e.target.as_str()),
        ) && sm != tm
        {
            *cross.entry((sm, tm)).or_default() += 1;
            if e.kind == EdgeKind::Imports && e.provenance != Some(Provenance::Heuristic) {
                *cross_imports.entry((sm, tm)).or_default() += 1;
            }
        }
    }
    let cycle_edges: Vec<(usize, usize)> = cross_imports.keys().copied().collect();
    let import_cycles: Vec<Vec<String>> =
        strongly_connected_components(mod_labels.len(), &cycle_edges)
            .into_iter()
            .map(|scc| scc.into_iter().map(|i| mod_labels[i].clone()).collect())
            .collect();
    let mut rare: Vec<((usize, usize), u32)> = cross.iter().map(|(k, v)| (*k, *v)).collect();
    rare.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let rare_bridges: Vec<(String, String, u32)> = rare
        .into_iter()
        .filter(|(_, c)| *c <= 2)
        .take(8)
        .map(|((s, t), c)| (mod_labels[s].clone(), mod_labels[t].clone(), c))
        .collect();
    let mut connected: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &(s, t) in cross.keys() {
        connected.insert(s);
        connected.insert(t);
    }
    let orphan_modules: Vec<(String, u32)> = if mod_labels.len() > 1 {
        (0..mod_labels.len())
            .filter(|i| !connected.contains(i))
            .map(|i| (mod_labels[i].clone(), members[i]))
            .collect()
    } else {
        Vec::new()
    };

    Insights {
        total_nodes: nodes.len(),
        total_edges: edges.len(),
        app_nodes: app_nodes.len(),
        communities: community_insights,
        hubs,
        approx_betweenness: approx,
        import_cycles,
        rare_bridges,
        orphan_modules,
    }
}

/// Louvain community detection over an undirected graph.
///
/// `edges` are `(a, b)` pairs with `a, b < n`; parallel pairs add weight 1
/// each; self-loops and out-of-range pairs are ignored. Returns `out[i]` =
/// community of node `i`, renumbered so communities sort by
/// (size desc, smallest member asc) — community 0 is always the biggest.
pub fn detect_communities(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    // Weighted adjacency at the current level; `self_w` carries the intra-
    // community weight folded into a node when levels aggregate.
    let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a == b || a >= n || b >= n {
            continue;
        }
        adj[a].push((b, 1.0));
        adj[b].push((a, 1.0));
    }
    merge_parallel(&mut adj);
    let mut self_w = vec![0.0f64; n];
    // membership[i] = community of ORIGINAL node i, expressed as a level-node id.
    let mut membership: Vec<usize> = (0..n).collect();

    loop {
        let ln = adj.len();
        let two_m: f64 =
            adj.iter().flatten().map(|(_, w)| w).sum::<f64>() + self_w.iter().sum::<f64>() * 2.0;
        if two_m == 0.0 {
            break;
        }
        let deg: Vec<f64> = (0..ln)
            .map(|i| adj[i].iter().map(|(_, w)| w).sum::<f64>() + 2.0 * self_w[i])
            .collect();
        let mut comm: Vec<usize> = (0..ln).collect();
        let mut tot: Vec<f64> = deg.clone(); // sum of degrees per community
        let mut moved = true;
        let mut rounds = 0;
        while moved && rounds < 64 {
            moved = false;
            rounds += 1;
            for i in 0..ln {
                let my = comm[i];
                // weight from i to each neighboring community, in ascending
                // community order (ties must break toward the smaller id)
                let mut to_comm: Vec<(usize, f64)> = Vec::new();
                for &(j, w) in &adj[i] {
                    let c = comm[j];
                    match to_comm.iter_mut().find(|(cc, _)| *cc == c) {
                        Some((_, acc)) => *acc += w,
                        None => to_comm.push((c, w)),
                    }
                }
                to_comm.sort_by_key(|&(c, _)| c);
                tot[my] -= deg[i]; // take i out of its community for the gain math
                let w_my = to_comm
                    .iter()
                    .find(|(c, _)| *c == my)
                    .map(|(_, w)| *w)
                    .unwrap_or(0.0);
                let base = w_my - tot[my] * deg[i] / two_m;
                let mut best = (my, 0.0f64);
                for &(c, w_c) in &to_comm {
                    if c == my {
                        continue;
                    }
                    let gain = (w_c - tot[c] * deg[i] / two_m) - base;
                    if gain > best.1 + 1e-12 {
                        best = (c, gain);
                    }
                }
                tot[best.0] += deg[i];
                if best.0 != my {
                    comm[i] = best.0;
                    moved = true;
                }
            }
        }
        // compact community ids in first-seen (ascending node) order
        let mut remap: Vec<Option<usize>> = vec![None; ln];
        let mut next = 0usize;
        for &c in &comm {
            if remap[c].is_none() {
                remap[c] = Some(next);
                next += 1;
            }
        }
        let comm: Vec<usize> = comm.iter().map(|&c| remap[c].unwrap_or(0)).collect();
        if next == ln {
            break; // nothing merged: converged
        }
        // aggregate communities into the next level's nodes
        let mut new_adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); next];
        let mut new_self = vec![0.0f64; next];
        for i in 0..ln {
            new_self[comm[i]] += self_w[i];
            for &(j, w) in &adj[i] {
                if i < j {
                    let (a, b) = (comm[i], comm[j]);
                    if a == b {
                        new_self[a] += w;
                    } else {
                        new_adj[a].push((b, w));
                        new_adj[b].push((a, w));
                    }
                }
            }
        }
        merge_parallel(&mut new_adj);
        for m in membership.iter_mut() {
            *m = comm[*m];
        }
        adj = new_adj;
        self_w = new_self;
    }

    renumber_by_size(n, &membership)
}

/// Sum parallel `(neighbor, weight)` entries and sort each row — the shape
/// every pass above relies on for determinism.
fn merge_parallel(adj: &mut [Vec<(usize, f64)>]) {
    for row in adj.iter_mut() {
        row.sort_by_key(|&(j, _)| j);
        let mut out: Vec<(usize, f64)> = Vec::with_capacity(row.len());
        for &(j, w) in row.iter() {
            match out.last_mut() {
                Some((lj, lw)) if *lj == j => *lw += w,
                _ => out.push((j, w)),
            }
        }
        *row = out;
    }
}

/// Renumber so communities sort by (size desc, smallest member asc) —
/// community 0 is always the biggest cluster, and the labels are stable.
fn renumber_by_size(n: usize, membership: &[usize]) -> Vec<usize> {
    let mut size: std::collections::BTreeMap<usize, (usize, usize)> = Default::default();
    for (i, &c) in membership.iter().enumerate() {
        let e = size.entry(c).or_insert((0, i));
        e.0 += 1;
    }
    let mut order: Vec<(usize, (usize, usize))> = size.into_iter().collect();
    order.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    let remap: std::collections::BTreeMap<usize, usize> = order
        .iter()
        .enumerate()
        .map(|(new, (old, _))| (*old, new))
        .collect();
    (0..n)
        .map(|i| remap.get(&membership[i]).copied().unwrap_or(0))
        .collect()
}

/// Strongly connected components of a directed graph; only SCCs of size ≥ 2
/// (the cycles) are returned, each sorted ascending, the list sorted by first
/// member. Iterative Tarjan — a recursive DFS would blow the stack at
/// VS Code scale.
pub fn strongly_connected_components(n: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a < n && b < n && a != b {
            adj[a].push(b);
        }
    }
    for row in adj.iter_mut() {
        row.sort_unstable();
        row.dedup();
    }
    const UNSEEN: usize = usize::MAX;
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut out: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if index[start] != UNSEEN {
            continue;
        }
        // explicit DFS frames: (node, next-neighbor-position)
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(v, pos)) = work.last() {
            if pos == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = adj[v].get(pos) {
                if let Some(frame) = work.last_mut() {
                    frame.1 += 1;
                }
                if index[w] == UNSEEN {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut scc = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        scc.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if scc.len() >= 2 {
                        scc.sort_unstable();
                        out.push(scc);
                    }
                }
            }
        }
    }
    out.sort_by_key(|s| s[0]);
    out
}

/// Betweenness centrality (Brandes 2001) over a DIRECTED unweighted graph —
/// exact. `out[i]` = number of shortest s→t paths through `i`, summed over all
/// pairs, fractional when paths tie. Deterministic by construction: sources
/// ascend, neighbor lists are sorted, accumulation is sequential — the
/// property the shelf crate (rustworkx-core) loses the moment its rayon path
/// engages (RwLock accumulation in thread-completion order; PRD 2026-08-18 §4.1).
pub fn betweenness_centrality(n: usize, edges: &[(usize, usize)]) -> Vec<f64> {
    let sources: Vec<usize> = (0..n).collect();
    brandes(n, edges, &sources, 1.0)
}

/// Approximate betweenness via deterministic pivot sampling (Brandes–Pich):
/// every ⌈n/max_pivots⌉-th node in ascending order is a source, scores scaled
/// by n/|pivots|. Same input ⇒ same output, no RNG.
pub fn betweenness_approx(n: usize, edges: &[(usize, usize)], max_pivots: usize) -> Vec<f64> {
    if n == 0 || max_pivots == 0 {
        return vec![0.0; n];
    }
    if max_pivots >= n {
        return betweenness_centrality(n, edges);
    }
    let step = n.div_ceil(max_pivots);
    let sources: Vec<usize> = (0..n).step_by(step).collect();
    let scale = n as f64 / sources.len() as f64;
    brandes(n, edges, &sources, scale)
}

fn brandes(n: usize, edges: &[(usize, usize)], sources: &[usize], scale: f64) -> Vec<f64> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a < n && b < n && a != b {
            adj[a].push(b);
        }
    }
    for row in adj.iter_mut() {
        row.sort_unstable();
        row.dedup();
    }

    let mut bc = vec![0.0f64; n];
    // Reused per-source scratch (allocation-free inner loop).
    let mut sigma = vec![0.0f64; n];
    let mut dist = vec![-1i64; n];
    let mut delta = vec![0.0f64; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];

    for &s in sources {
        sigma.iter_mut().for_each(|x| *x = 0.0);
        dist.iter_mut().for_each(|x| *x = -1);
        delta.iter_mut().for_each(|x| *x = 0.0);
        order.clear();
        preds.iter_mut().for_each(Vec::clear);

        sigma[s] = 1.0;
        dist[s] = 0;
        let mut queue = std::collections::VecDeque::from([s]);
        while let Some(v) = queue.pop_front() {
            order.push(v);
            for &w in &adj[v] {
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }
        // Dependency accumulation in reverse BFS order.
        for &w in order.iter().rev() {
            for &v in &preds[w] {
                delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
            }
            if w != s {
                bc[w] += delta[w] * scale;
            }
        }
    }
    bc
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn betweenness_of_a_directed_path_peaks_in_the_middle() {
        // 0 -> 1 -> 2 -> 3 : node 1 carries (0,2),(0,3); node 2 carries (0,3),(1,3)
        let edges = [(0, 1), (1, 2), (2, 3)];
        let bc = betweenness_centrality(4, &edges);
        assert_eq!(bc[0], 0.0);
        assert_eq!(bc[1], 2.0);
        assert_eq!(bc[2], 2.0);
        assert_eq!(bc[3], 0.0);
    }

    #[test]
    fn betweenness_splits_across_tied_shortest_paths() {
        // diamond: 0->1->3 and 0->2->3 — the pair (0,3) splits 0.5/0.5
        let edges = [(0, 1), (0, 2), (1, 3), (2, 3)];
        let bc = betweenness_centrality(4, &edges);
        assert!((bc[1] - 0.5).abs() < 1e-12, "{bc:?}");
        assert!((bc[2] - 0.5).abs() < 1e-12, "{bc:?}");
    }

    #[test]
    fn betweenness_finds_the_bridge_the_degree_misses() {
        // Two cliques joined by a low-degree bridge node 3: 0,1,2 -> 3 -> 4,5,6.
        // Node 3 has degree 6 like everyone touching it, but ALL cross traffic
        // flows through it — the god-node signature degree cannot see.
        let edges = [
            (0, 1),
            (1, 0),
            (0, 2),
            (2, 0),
            (1, 2),
            (2, 1),
            (4, 5),
            (5, 4),
            (4, 6),
            (6, 4),
            (5, 6),
            (6, 5),
            (0, 3),
            (1, 3),
            (2, 3),
            (3, 4),
            (3, 5),
            (3, 6),
        ];
        let bc = betweenness_centrality(7, &edges);
        let max = bc.iter().cloned().fold(f64::MIN, f64::max);
        assert_eq!(bc[3], max, "the bridge dominates: {bc:?}");
        assert!(
            bc[3] >= 8.0,
            "3 carries all 3x3 cross pairs minus direct: {bc:?}"
        );
    }

    #[test]
    fn betweenness_is_deterministic_under_edge_permutation_and_reruns() {
        let edges = [(0, 1), (1, 2), (2, 3), (0, 2), (3, 4), (1, 4), (4, 0)];
        let mut rev = edges.to_vec();
        rev.reverse();
        let a = betweenness_centrality(5, &edges);
        let b = betweenness_centrality(5, &rev);
        let c = betweenness_centrality(5, &edges);
        assert_eq!(a, b, "edge order must not leak");
        assert_eq!(a, c, "reruns bit-identical");
    }

    #[test]
    fn approx_is_deterministic_and_exact_when_pivots_cover_all() {
        let edges = [(0, 1), (1, 2), (2, 3)];
        assert_eq!(
            betweenness_approx(4, &edges, 100),
            betweenness_centrality(4, &edges),
            "pivots >= n degrades to exact"
        );
        let e2: Vec<(usize, usize)> = (0..50).map(|i| (i, (i + 1) % 50)).collect();
        let a = betweenness_approx(50, &e2, 10);
        let b = betweenness_approx(50, &e2, 10);
        assert_eq!(a, b, "sampling is seeded by structure, not RNG");
    }

    #[test]
    fn two_triangles_bridged_by_one_edge_are_two_communities() {
        // 0-1-2 triangle, 3-4-5 triangle, one bridge 2-3
        let edges = [(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5), (2, 3)];
        let c = detect_communities(6, &edges);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_eq!(c[4], c[5]);
        assert_ne!(c[0], c[3], "the bridge must not merge the triangles");
        // renumbering: equal sizes -> the community holding node 0 gets id 0
        assert_eq!(c[0], 0);
        assert_eq!(c[3], 1);
    }

    #[test]
    fn communities_are_deterministic_under_edge_permutation() {
        let base = [(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5), (2, 3)];
        let mut rev = base.to_vec();
        rev.reverse();
        assert_eq!(detect_communities(6, &base), detect_communities(6, &rev));
    }

    #[test]
    fn isolated_nodes_get_their_own_community() {
        let c = detect_communities(3, &[]);
        assert_eq!(c.len(), 3);
        assert_ne!(c[0], c[1]);
        assert_ne!(c[1], c[2]);
    }

    #[test]
    fn empty_graph_is_an_empty_answer() {
        assert!(detect_communities(0, &[]).is_empty());
        assert!(strongly_connected_components(0, &[]).is_empty());
    }

    #[test]
    fn parallel_edges_weigh_more_than_single_ones() {
        // 0 is pulled both ways; three parallel edges to 1 must win over one to 2.
        let edges = [(0, 1), (0, 1), (0, 1), (0, 2), (1, 3), (2, 3)];
        let c = detect_communities(4, &edges);
        assert_eq!(c[0], c[1], "the heavy pair clusters together");
    }

    #[test]
    fn scc_finds_the_cycle_and_ignores_the_dag() {
        // cycle 0->1->2->0, dag tail 2->3->4
        let edges = [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4)];
        let sccs = strongly_connected_components(5, &edges);
        assert_eq!(sccs, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn scc_on_a_dag_is_empty() {
        assert!(strongly_connected_components(3, &[(0, 1), (1, 2)]).is_empty());
    }

    #[test]
    fn scc_two_disjoint_cycles_sorted_by_first_member() {
        let edges = [(3, 4), (4, 3), (0, 1), (1, 0)];
        let sccs = strongly_connected_components(5, &edges);
        assert_eq!(sccs, vec![vec![0, 1], vec![3, 4]]);
    }
}
