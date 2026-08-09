# Semantic Viz + Report Implementation Plan ("beat Graphify")

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Beat Graphify on the semantic front without betraying the invariants: deterministic community detection with a "color by cluster" mode in the viz, a measured token-economy figure in every `explore` answer, god-nodes + rare cross-module edges in the viz HUD, a `selene report` command that writes `GRAPH_REPORT.md`, and a zero-dependency 2.5D depth effect (no Three.js, no `--3d`).

**Architecture:** All new analysis (Louvain communities, Tarjan SCC) lives in `selene-cli` (`analysis.rs`) — pure functions over in-memory `(nodes, edges)`, exactly like `viz.rs`'s transform. `selene-graph` stays thin (SurrealQL-max: no graph algorithms in that layer). The token-economy line is computed inside `selene-context`'s `ContextBuilder` from `FileRecord.size` (indexed data — deterministic), so CLI *and* MCP get it for free. The viz JSON gains `c` (community) per node, a `communities` array, and `meta.hubs` / `meta.surprises`; the template gains a Clusters button, HUD insight lines, and a parallax starfield.

**Tech Stack:** Rust (workspace as-is, no new dependencies), vanilla JS/canvas in `viz-template.html` (zero external resources — a hard test already asserts this).

## Global Constraints

- **Deterministic everything.** The viz transform must stay a pure function of the graph (`--watch` compares serialized output). Louvain must process nodes in fixed order with fixed tie-breaks. No `HashMap` iteration order may leak into output.
- **Anti-Read invariant.** The token-economy line must never suggest reading files. Budgets/ceilings in `budgets.rs` are a contract — the footer is appended *after* `truncate_to_ceiling` and must stay ≤ ~200 chars.
- **`isError` reserved** — `selene report` failures follow the viz pattern (stderr + exit 1 for real failures only).
- **Zero external resources in the HTML page** (`page_is_self_contained_and_carries_the_data` test asserts no `http://`, no `src=`).
- **Sentrux layering**: new code in `selene-cli` may import graph/context/db/core; nothing new imports `selene-cli`.
- **Purge stays safe**: `GRAPH_REPORT.md` is selene-generated at the project root → purge removes it, and only it (same audit discipline as `selene-graph.html`).
- **Verify against the real binary** (project lesson: green tests prove nothing).

---

### Task 1: `analysis.rs` — deterministic Louvain + Tarjan SCC

**Files:**
- Create: `crates/selene-cli/src/analysis.rs`
- Modify: `crates/selene-cli/src/lib.rs` (add `pub mod analysis;` next to `pub mod viz;`)

**Interfaces:**
- Produces: `pub fn detect_communities(n: usize, edges: &[(usize, usize)]) -> Vec<usize>` — input: node count + undirected edges as dense indices (parallel edges add weight, self-loops ignored at level 0); output: `out[i]` = community id of node `i`, communities renumbered **by size desc, then smallest member index** so id 0 is always the biggest cluster.
- Produces: `pub fn strongly_connected_components(n: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>>` — directed edges; returns only SCCs of size ≥ 2 (the cycles), each sorted ascending, list sorted by first member.

- [ ] **Step 1: Write the failing tests** (inline `#[cfg(test)]` in `analysis.rs`, following `viz.rs` style)

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail** — `cargo test -p selene-cli analysis` → FAIL (module missing)

- [ ] **Step 3: Implement.** Louvain, deterministic by construction:

```rust
//! Graph analysis for the viz + report surfaces: community detection (Louvain)
//! and cycle detection (Tarjan SCC). Pure functions over dense-index graphs —
//! no store access, no I/O — and DETERMINISTIC: nodes are processed in
//! ascending index order and every tie breaks toward the smaller id, because
//! the viz transform must stay a pure function of the graph (`--watch` diffs
//! serialized output byte-for-byte).

/// Louvain community detection over an undirected graph.
/// `edges` are `(a, b)` pairs with `a, b < n`; parallel pairs add weight 1
/// each; self-loops are ignored. Returns `out[i]` = community of node `i`,
/// renumbered so communities sort by (size desc, smallest member asc).
pub fn detect_communities(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    // Weighted adjacency at the current level; self_w = intra-community weight
    // folded into a node when levels aggregate.
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
    // membership[i] = community of ORIGINAL node i (community = level-node id)
    let mut membership: Vec<usize> = (0..n).collect();

    loop {
        let ln = adj.len();
        let mut comm: Vec<usize> = (0..ln).collect();
        let two_m: f64 = adj.iter().flatten().map(|(_, w)| w).sum::<f64>()
            + self_w.iter().sum::<f64>() * 2.0;
        if two_m == 0.0 {
            break;
        }
        let deg: Vec<f64> = (0..ln)
            .map(|i| adj[i].iter().map(|(_, w)| w).sum::<f64>() + 2.0 * self_w[i])
            .collect();
        let mut tot: Vec<f64> = deg.clone(); // sum of degrees per community
        let mut moved = true;
        let mut rounds = 0;
        while moved && rounds < 64 {
            moved = false;
            rounds += 1;
            for i in 0..ln {
                let my = comm[i];
                // weight from i to each neighboring community
                let mut to_comm: Vec<(usize, f64)> = Vec::new();
                for &(j, w) in &adj[i] {
                    let c = comm[j];
                    match to_comm.iter_mut().find(|(cc, _)| *cc == c) {
                        Some((_, acc)) => *acc += w,
                        None => to_comm.push((c, w)),
                    }
                }
                to_comm.sort_by_key(|&(c, _)| c); // determinism
                tot[my] -= deg[i];
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
        for i in 0..ln {
            let c = comm[i];
            if remap[c].is_none() {
                remap[c] = Some(next);
                next += 1;
            }
        }
        let comm: Vec<usize> = comm.iter().map(|&c| remap[c].unwrap_or(0)).collect();
        if next == ln {
            break; // nothing merged: converged
        }
        // aggregate to the next level
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

    renumber_by_size(n, membership)
}

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

/// Renumber so communities sort by (size desc, smallest member asc).
fn renumber_by_size(n: usize, membership: Vec<usize>) -> Vec<usize> {
    let mut size: std::collections::BTreeMap<usize, (usize, usize)> = Default::default();
    for (i, &c) in membership.iter().enumerate() {
        let e = size.entry(c).or_insert((0, i));
        e.0 += 1;
    }
    let mut order: Vec<(usize, (usize, usize))> = size.into_iter().collect();
    order.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    let remap: std::collections::BTreeMap<usize, usize> =
        order.iter().enumerate().map(|(new, (old, _))| (*old, new)).collect();
    (0..n).map(|i| remap[&membership[i]]).collect()
}
```

Tarjan, iterative (no recursion — VS Code-scale graphs would blow the stack), neighbor lists sorted for determinism:

```rust
/// Strongly connected components of a directed graph; only SCCs of size ≥ 2
/// (the cycles) are returned, each sorted, the list sorted by first member.
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
    let (mut index, mut low, mut on_stack) = (vec![usize::MAX; n], vec![0usize; n], vec![false; n]);
    let (mut stack, mut next_index) = (Vec::new(), 0usize);
    let mut out: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if index[start] != usize::MAX {
            continue;
        }
        // explicit DFS: (node, next-neighbor-position)
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&mut (v, ref mut pos)) = work.last_mut() {
            if *pos == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = adj[v].get(*pos) {
                *pos += 1;
                if index[w] == usize::MAX {
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
```

- [ ] **Step 4: Run tests** — `cargo test -p selene-cli analysis` → all PASS. Also `cargo clippy -p selene-cli --all-targets`.

- [ ] **Step 5: Commit** — `git add crates/selene-cli/src/analysis.rs crates/selene-cli/src/lib.rs && git commit -m "feat(cli): deterministic Louvain + Tarjan SCC in analysis.rs"`

---

### Task 2: viz data — communities, hubs, surprises in the JSON

**Files:**
- Modify: `crates/selene-cli/src/viz.rs`

**Interfaces:**
- Consumes: `crate::analysis::detect_communities`
- Produces (JSON contract for Task 3): each node row gains `"c": <community idx | -1>`; top-level `"communities": [{"id": u, "n": members, "l": "<dominant module label>"}]` (top 12 by size, computed over the FULL app graph, not just the capped nodes); `meta.hubs`: `[{"i": <dense idx>, "n": name, "f": file, "d": degree, "in": in_deg, "out": out_deg}]` (top 5 by degree among kept); `meta.surprises`: `[{"s": label, "t": label, "c": count}]` (up to 5 cross-module pairs with the SMALLEST counts, ties by label — the rare bridges).

- [ ] **Step 1: Write the failing tests** (extend `viz.rs` tests; the `node()` helper gains a file-path variant)

```rust
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
        edge("function:a", "function:b"), edge("function:b", "function:c"),
        edge("function:c", "function:a"), edge("function:d", "function:e"),
        edge("function:e", "function:f"), edge("function:f", "function:d"),
        edge("function:c", "function:d"), // the bridge
    ];
    let data = build_data(&nodes, &edges, &opts(2000, false));
    let comms = data.json["communities"].as_array().unwrap();
    assert_eq!(comms.len(), 2, "two clusters, though the dirs interleave");
    let c_of = |name: &str| data.json["nodes"].as_array().unwrap().iter()
        .find(|n| n["n"] == name).unwrap()["c"].as_i64().unwrap();
    assert_eq!(c_of("a"), c_of("b"));
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
    // x<->y heavily linked (3 edges), x<->z linked ONCE — z is the surprise.
    let nodes = vec![
        node_in("function:a", "a", NodeKind::Function, "x/a.rs"),
        node_in("function:b", "b", NodeKind::Function, "y/b.rs"),
        node_in("function:c", "c", NodeKind::Function, "z/c.rs"),
    ];
    let edges = vec![
        edge("function:a", "function:b"), edge("function:b", "function:a"),
        edge("function:a", "function:b"), edge("function:a", "function:c"),
    ];
    let data = build_data(&nodes, &edges, &opts(2000, false));
    let sur = data.json["meta"]["surprises"].as_array().unwrap();
    assert_eq!(sur[0]["c"], 1, "the singleton bridge ranks first");
    assert_eq!(sur[0]["t"], "z");
}
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test -p selene-cli viz`

- [ ] **Step 3: Implement in `build_data`** (after the module map block, before `nodes_json`):

```rust
// --- communities: the REAL clusters of the call graph -------------------
// Louvain over the full app graph (same node set as the module map), on
// every edge kind EXCEPT `contains` — contains mirrors the file hierarchy,
// which is exactly the signal communities exist to complement.
let app_index: HashMap<&str, usize> = app_nodes
    .iter()
    .enumerate()
    .map(|(i, n)| (n.id.as_str(), i))
    .collect();
let comm_edges: Vec<(usize, usize)> = edges
    .iter()
    .filter(|e| e.kind != selene_core::EdgeKind::Contains)
    .filter_map(|e| {
        Some((*app_index.get(e.source.as_str())?, *app_index.get(e.target.as_str())?))
    })
    .collect();
let communities = crate::analysis::detect_communities(app_nodes.len(), &comm_edges);
// label each community by its dominant module (ties -> lexicographic)
let n_comms = communities.iter().copied().max().map_or(0, |m| m + 1);
let mut comm_size = vec![0u32; n_comms];
let mut comm_mods: Vec<HashMap<&str, u32>> = vec![HashMap::new(); n_comms];
for (i, n) in app_nodes.iter().enumerate() {
    let c = communities[i];
    comm_size[c] += 1;
    let m = module_of(&n.file_path, mod_depth);
    let lbl = mod_labels.iter().find(|l| **l == m).map(|l| l.as_str()).unwrap_or("(root)");
    *comm_mods[c].entry(lbl).or_default() += 1;
}
let communities_json: Vec<serde_json::Value> = (0..n_comms)
    .filter(|&c| comm_size[c] >= 2)
    .take(12)
    .map(|c| {
        let mut mods: Vec<(&str, u32)> = comm_mods[c].iter().map(|(k, v)| (*k, *v)).collect();
        mods.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        serde_json::json!({
            "id": c, "n": comm_size[c],
            "l": mods.first().map(|(m, _)| *m).unwrap_or("(root)"),
        })
    })
    .collect();
```

Node rows gain `"c"`:

```rust
"c": app_index.get(n.id.as_str()).map(|&i| communities[i] as i64).unwrap_or(-1),
```

Hubs (after `index` is built — needs the dense idx) and surprises (from the already-built `pair_counts` — **move its consumption up or clone before it is drained**):

```rust
// god-nodes: the 5 most-connected kept symbols, with direction split
let mut in_deg: HashMap<&str, u32> = HashMap::new();
let mut out_deg: HashMap<&str, u32> = HashMap::new();
for e in edges {
    *out_deg.entry(e.source.as_str()).or_default() += 1;
    *in_deg.entry(e.target.as_str()).or_default() += 1;
}
let hubs_json: Vec<serde_json::Value> = kept
    .iter()
    .take(5) // kept is already sorted by degree desc, name, id
    .filter(|n| degree.get(n.id.as_str()).copied().unwrap_or(0) > 0)
    .map(|n| serde_json::json!({
        "i": index[n.id.as_str()], "n": n.name, "f": n.file_path,
        "d": degree.get(n.id.as_str()).copied().unwrap_or(0),
        "in": in_deg.get(n.id.as_str()).copied().unwrap_or(0),
        "out": out_deg.get(n.id.as_str()).copied().unwrap_or(0),
    }))
    .collect();
// surprises: cross-module pairs with the SMALLEST counts — rare bridges
let mut rare: Vec<((usize, usize), u32)> = mod_links_sorted.clone();
rare.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
let surprises_json: Vec<serde_json::Value> = rare
    .iter()
    .filter(|(_, c)| *c <= 2)
    .take(5)
    .map(|((s, t), c)| serde_json::json!({
        "s": mod_labels[*s], "t": mod_labels[*t], "c": c,
    }))
    .collect();
```

Wire into the output JSON: `"communities": communities_json` at top level; `"hubs": hubs_json, "surprises": surprises_json` inside `meta`.

- [ ] **Step 4: Run tests** — `cargo test -p selene-cli` → PASS (including the 5 existing viz tests, untouched).

- [ ] **Step 5: Commit** — `git commit -am "feat(viz): Louvain communities, god-node hubs, rare-bridge surprises in the data"`

---

### Task 3: template — Clusters mode, HUD insights

**Files:**
- Modify: `crates/selene-cli/src/viz-template.html`

**Interfaces:**
- Consumes: `DATA.communities`, `DATA.meta.hubs`, `DATA.meta.surprises`, node `.c` (Task 2).

- [ ] **Step 1: Add the Clusters button** in `#controls`, after `#mode-sym`:

```html
<button id="mode-clu" title="Call-graph clusters (Louvain) — colors that ignore directories">Clusters</button>
```

- [ ] **Step 2: Add the color mode.** Near the view state:

```js
let colorBy = "kind"; // "kind" | "community"
const CLUSTER_PALETTE = MODULE_PALETTE; // same hues, different meaning
function nodeFill(nd) {
  if (nd.kind === "mod") return nd.color;
  if (colorBy === "community" && nd.c >= 0)
    return CLUSTER_PALETTE[nd.c % CLUSTER_PALETTE.length];
  return colorFor(nd.k);
}
```

Replace the two `ctx.fillStyle = nd.kind === "mod" ? nd.color : colorFor(nd.k)` (draw) and `kEl.style.background = …` (details) call sites with `nodeFill(nd)`. Carry `c` through `buildModel` (`old.c = r.c;` and `c: r.c` in the fresh-node literal).

Buttons: `mode-clu` → `colorBy = "community"; setView("symbols")`; `mode-sym` → `colorBy = "kind"; setView("symbols")`; `mode-map` → `colorBy = "kind"; setView("map")`. In `syncChrome`, `mode-clu` gets `.on` when `view === "symbols" && colorBy === "community"`, and `mode-sym`'s `.on` also requires `colorBy === "kind"`.

- [ ] **Step 3: Cluster legend.** In `syncChrome`'s symbols branch, when `colorBy === "community"`, list `DATA.communities` instead of kinds:

```js
title.textContent = "Clusters (call-graph communities)";
for (const co of (DATA.communities || [])) {
  const row = document.createElement("div");
  row.className = "lrow";
  row.innerHTML =
    '<span class="sw" style="background:' + CLUSTER_PALETTE[co.id % CLUSTER_PALETTE.length] + '"></span>' +
    '<span class="ln">mostly ' + shortLabel(co.l) + '</span><span class="lc">' + co.n + '</span>';
  row.addEventListener("click", () => {
    for (const nd of curNodes) nd.match = nd.c === co.id;   // spotlight via the search dim
    searchQ = "​"; // zero-width: non-empty so draw() dims non-matches
  });
  rows.appendChild(row);
}
```

- [ ] **Step 4: HUD insights.** Below `#counts` in the HUD:

```html
<div class="counts" id="insights" style="display:none"></div>
```

Filled once at startup (and on live updates via `syncChrome`):

```js
function renderInsights() {
  const meta = DATA.meta || {};
  const el = document.getElementById("insights");
  const hubs = (meta.hubs || []).slice(0, 5);
  const sur = (meta.surprises || []).slice(0, 3);
  if (!hubs.length && !sur.length) { el.style.display = "none"; return; }
  el.style.display = "";
  let html = "";
  if (hubs.length) {
    html += "⭐ " + hubs.map(h =>
      '<a href="#" class="hub" data-i="' + h.i + '" title="' + h.f + ' · ' +
      h.out + ' out / ' + h.in + ' in">' + h.n + " (" + h.d + ")</a>").join(" · ");
  }
  if (sur.length) {
    html += (html ? "<br>" : "") + "⚡ " + sur.map(s =>
      shortLabel(s.s) + " → " + shortLabel(s.t) + " (" + s.c + ")").join(" · ");
  }
  el.innerHTML = html;
  el.querySelectorAll("a.hub").forEach(a => a.addEventListener("click", ev => {
    ev.preventDefault();
    const nd = symbols[+a.dataset.i];
    if (!nd) return;
    if (view === "map") setView("symbols");
    select(nd);
    scale = Math.max(scale, 1.2); tx = cw / 2 - nd.x * scale; ty = ch / 2 - nd.y * scale;
  }));
}
renderInsights();
```

Style the links quietly: `#insights a { color:#9ecbff; text-decoration:none } #insights a:hover { text-decoration:underline }`. Call `renderInsights()` from `applyUpdate` too (hubs move as the graph grows).

- [ ] **Step 5: Verify.** `cargo test -p selene-cli` (the self-containment test must still pass — the new markup adds no `src=`/URLs; note `href="#"` is fine, the test greps `src=` and `http`). Then the real binary:

```bash
cargo build --release -p selene
./target/release/selene index /tmp/dogfood-selene   # if missing, recreate per RESUME §7
./target/release/selene viz --path /tmp/dogfood-selene --out /tmp/galaxy.html
grep -c '"communities"' /tmp/galaxy.html   # 1
grep -o '"hubs":\[[^]]*' /tmp/galaxy.html | head -c 300   # real hub names
open /tmp/galaxy.html   # eyeball: Clusters button colors, HUD lists hubs
```

- [ ] **Step 6: Commit** — `git commit -am "feat(viz): Clusters color mode + god-nodes and rare bridges in the HUD"`

---

### Task 4: the token-economy line in `explore`

**Files:**
- Modify: `crates/selene-context/src/builder.rs`

**Interfaces:**
- Consumes: `ctx.subgraph.nodes` (distinct `file_path`s), `self.qm.store().all_files()` (`FileRecord.size` — indexed, deterministic), the final `out` length.
- Produces: a one-line footer appended AFTER `truncate_to_ceiling` (so ceilings/truncation tests are untouched), via `pub(crate) fn token_economy_line(answer_bytes: usize, files_bytes: u64, files: usize) -> Option<String>`.

- [ ] **Step 1: Write the failing tests** (in `builder.rs`'s test module):

```rust
#[test]
fn token_economy_line_states_the_savings_without_suggesting_read() {
    let line = token_economy_line(4_000, 400_000, 9).unwrap();
    assert!(line.contains("≈ 1k tokens"), "answer ≈ 4000/4 = 1000 tokens: {line}");
    assert!(line.contains("100k tokens"), "files ≈ 400000/4: {line}");
    assert!(line.contains("100× less"), "{line}");
    assert!(!line.to_lowercase().contains("read the"), "must never suggest reading");
}

#[test]
fn token_economy_line_stays_silent_when_there_is_nothing_to_brag_about() {
    assert!(token_economy_line(4_000, 6_000, 1).is_none(), "ratio < 2 is noise");
    assert!(token_economy_line(0, 6_000, 1).is_none());
    assert!(token_economy_line(4_000, 400_000, 0).is_none());
}
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test -p selene-context token_economy`

- [ ] **Step 3: Implement.**

```rust
/// The measured token economy of an answer: what reading the same files raw
/// would have cost. ≈ 4 bytes/token (the cross-tokenizer rule of thumb —
/// this is a magnitude claim, not an invoice). Silent below 2× (a brag that
/// small reads as an apology) and silent when nothing was rendered.
pub(crate) fn token_economy_line(
    answer_bytes: usize,
    files_bytes: u64,
    files: usize,
) -> Option<String> {
    const BYTES_PER_TOKEN: f64 = 4.0;
    if answer_bytes == 0 || files == 0 {
        return None;
    }
    let ratio = files_bytes as f64 / answer_bytes as f64;
    if ratio < 2.0 {
        return None;
    }
    let fmt = |bytes: f64| {
        let t = bytes / BYTES_PER_TOKEN;
        if t >= 950.0 {
            format!("{}k", (t / 1000.0).round().max(1.0) as u64)
        } else {
            format!("{}", t.round() as u64)
        }
    };
    Some(format!(
        "\n\n---\n*Token economy: this answer ≈ {} tokens; the {} file{} it distills total ≈ {} tokens — **{}× less** than reading them.*\n",
        fmt(answer_bytes as f64),
        files,
        if files == 1 { "" } else { "s" },
        fmt(files_bytes as f64),
        ratio.round() as u64,
    ))
}
```

Wire into `build_context`, replacing the final `Ok(truncate_to_ceiling(&out, &budget))`:

```rust
let mut answer = truncate_to_ceiling(&out, &budget);
// The token-economy receipt — measured from the index (FileRecord.size),
// appended after truncation so the ceiling contract is untouched.
let touched: std::collections::HashSet<&str> = ctx
    .subgraph
    .nodes
    .values()
    .map(|n| n.file_path.as_str())
    .collect();
let files_bytes: u64 = self
    .qm
    .store()
    .all_files()
    .await
    .map(|fs| {
        fs.iter()
            .filter(|f| touched.contains(f.path.as_str()))
            .map(|f| f.size)
            .sum()
    })
    .unwrap_or(0);
if let Some(line) = token_economy_line(answer.len(), files_bytes, touched.len()) {
    answer.push_str(&line);
}
Ok(answer)
```

- [ ] **Step 4: Run the tests that guard this file** — `cargo test -p selene-context` **and count failures the honest way**: `cargo test -p selene-context 2>&1 | grep -c 'FAILED'` → 0. If `phase4_gate` asserts on output *endings*, fix the assertion to be section-based, not suffix-based (the gate corpus is 2 TS projects; the footer appears when ratio ≥ 2, which real corpora will trigger).

- [ ] **Step 5: Verify against the real binary** (the probe, RESUME §7):

```bash
cargo build --release -p selene
/tmp/ask.sh "how does an unresolved reference become a graph edge"  # still 3/3
printf '…explore call…' | ./target/release/selene explore --path /tmp/dogfood-selene "how does a file get indexed" | tail -3
# expect: the Token economy line with a real multiplier
```

- [ ] **Step 6: Commit** — `git commit -am "feat(explore): measured token-economy receipt on every answer"`

---

### Task 5: `selene report` → `GRAPH_REPORT.md`

**Files:**
- Create: `crates/selene-cli/src/cmd/report.rs`
- Modify: `crates/selene-cli/src/cli.rs` (new `Report` arm), `crates/selene-cli/src/cmd/mod.rs` (mod + re-export), `crates/selene-cli/src/lib.rs` (dispatch arm), `crates/selene-cli/src/viz.rs` (make `is_low_signal`, `is_noise_path`, `module_of` `pub(crate)`), `crates/selene-cli/src/cmd/lifecycle.rs` (purge also removes `GRAPH_REPORT.md`)

**Interfaces:**
- Consumes: `SurrealStore::open` + `all_nodes`/`all_edges` (the viz pattern), `crate::analysis::{detect_communities, strongly_connected_components}`, `crate::viz::{is_low_signal, is_noise_path, module_of}`.
- Produces: `pub async fn report(path: Option<PathBuf>, out: Option<PathBuf>) -> Outcome`; a pure `fn render_report(nodes: &[Node], edges: &[Edge], root_label: &str) -> String` (testable without a store). Default output `<root>/GRAPH_REPORT.md`, path printed to stdout.

- [ ] **Step 1: clap arm** in `cli.rs`, after `Viz`:

```rust
/// Write GRAPH_REPORT.md — hubs, clusters, cycles, orphan modules, and
/// the questions worth asking `selene explore` first.
Report {
    #[arg(short, long)]
    path: Option<PathBuf>,
    /// Output file. Default: `./GRAPH_REPORT.md`.
    #[arg(short, long)]
    out: Option<PathBuf>,
},
```

Dispatch in `lib.rs` next to the `Viz` arm: `Command::Report { path, out } => cmd::report(path, out).await`.

- [ ] **Step 2: Write the failing test** (in `report.rs`; reuse the `node_in`/`edge` helper shapes from `viz.rs` tests):

```rust
#[test]
fn the_report_names_hubs_cycles_orphans_and_questions() {
    // hub: called by everyone; cycle: a->b->a across modules; orphan: z/ alone
    let nodes = vec![
        node_in("function:hub", "hub", NodeKind::Function, "x/hub.rs"),
        node_in("function:a", "a", NodeKind::Function, "x/a.rs"),
        node_in("function:b", "b", NodeKind::Function, "y/b.rs"),
        node_in("function:lone", "lone", NodeKind::Function, "z/lone.rs"),
    ];
    let edges = vec![
        edge("function:a", "function:hub"), edge("function:b", "function:hub"),
        edge("function:a", "function:b"), edge("function:b", "function:a"),
    ];
    let md = render_report(&nodes, &edges, "/tmp/demo");
    assert!(md.contains("# Graph report"), "{md}");
    assert!(md.contains("`hub`"), "hubs section names the hub");
    assert!(md.contains("x ⇄ y") || md.contains("x → y → x"), "the module cycle is spelled out: {md}");
    assert!(md.contains("`z`"), "orphan module listed");
    assert!(md.contains("selene explore"), "suggested questions teach the tool");
}

#[test]
fn the_report_is_deterministic() {
    let (nodes, edges) = /* same fixture */;
    let mut rev_e = edges.clone(); rev_e.reverse();
    let mut rev_n = nodes.clone(); rev_n.reverse();
    assert_eq!(render_report(&nodes, &edges, "r"), render_report(&rev_n, &rev_e, "r"));
}
```

- [ ] **Step 3: Run to verify FAIL**, then implement `report.rs`. Shape:

```rust
//! `selene report` — the graph's executive summary as a markdown file: the
//! god-nodes, the call-graph clusters, the module cycles, the orphan modules,
//! the rare bridges, and the questions worth asking `selene explore` first.
//! Pure transform + the same store-opening choreography as `cmd::viz`.
```

`report(path, out)`: `query_root_direct(path)` → open store → `all_nodes`/`all_edges` → `render_report` → write to `out.unwrap_or(root.join("GRAPH_REPORT.md"))` → println! the path, eprintln! a one-line summary. Errors follow `viz`'s `Outcome::Failure` shape.

`render_report` sections (all computed over app-quality nodes — `!is_low_signal && !is_noise_path` — sorted deterministically; module depth auto-tuned exactly like `build_data`, extract that loop into `pub(crate) fn auto_mod_depth(app_nodes: &[&Node]) -> usize` in `viz.rs` and reuse):

1. **Header** — `# Graph report — {project}`, node/edge/file counts, generation note ("regenerate with `selene report`").
2. **God-nodes** — top 10 by total degree: `| symbol | file | in | out |` table. A one-liner under it: any node with `out == 0` and huge `in` is *plumbing*, any with big `out` is an *orchestrator* (the directional lesson from relevance.rs, made visible).
3. **Call-graph clusters** — Louvain (via `analysis::detect_communities` on non-`Contains` edges): top 8 as `cluster N — M symbols, mostly <label>`; flag any cluster whose members span ≥3 modules ("crosses directories — the map under the map").
4. **Module cycles** — module-level directed graph (from `module_of`), `strongly_connected_components`; each SCC rendered `x → y → x`. None → "No module cycles — the layering holds."
5. **Orphan modules** — modules with zero cross-module edges either direction: listed with member counts. None → omitted.
6. **Rare bridges** — cross-module pairs with count ≤ 2 (the viz `surprises` logic, recomputed here): `x → z (1 reference)`.
7. **Questions worth asking** — 3–5 lines derived from the data, each a runnable command:
   `- \`selene explore "how does {top hub} work"\` — it sits under {in} callers.`
   `- \`selene explore "what happens between {bridge.s} and {bridge.t}"\` — a single edge carries the whole dependency.`

- [ ] **Step 4: purge.** In `lifecycle.rs`'s purge target list, add `GRAPH_REPORT.md` beside `selene-graph.html` (same guard style: remove only if present; report the removal). Extend the existing purge audit test enumerating removal targets.

- [ ] **Step 5: Run** — `cargo test -p selene-cli` → PASS; `cargo clippy -p selene-cli --all-targets` clean. Real binary:

```bash
cargo build --release -p selene
./target/release/selene report --path /tmp/dogfood-selene --out /tmp/GRAPH_REPORT.md
head -50 /tmp/GRAPH_REPORT.md   # eyeball: real hubs, real clusters, real questions
```

- [ ] **Step 6: Commit** — `git commit -am "feat(cli): selene report — GRAPH_REPORT.md with hubs, clusters, cycles, orphans"`

---

### Task 6: ~~the 2.5D depth effect (zero-dependency)~~ — DROPPED

> **User decision (2026-08-10): no 3D/2.5D at all — the 4 semantic features are the
> deliverable.** Implemented, then reverted (the commit was dropped before push).
> The steps below are kept for the record only.

**Files:**
- Modify: `crates/selene-cli/src/viz-template.html`

- [ ] **Step 1: Parallax starfield.** Two star layers behind the graph, moving at 0.35×/0.65× of the camera — depth without a z-axis, no libraries. Seeded PRNG so reloads look identical:

```js
// ---- 2.5D: parallax starfield --------------------------------------------
function mulberry32(a) { return function() {
  a |= 0; a = a + 0x6D2B79F5 | 0;
  let t = Math.imul(a ^ a >>> 15, 1 | a);
  t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t;
  return ((t ^ t >>> 14) >>> 0) / 4294967296;
}; }
const STARS = (() => {
  let seed = 0;
  for (const ch of projectName) seed = (seed * 31 + ch.charCodeAt(0)) | 0;
  const rnd = mulberry32(seed || 1);
  const layers = [];
  for (const [depth, count, size] of [[0.35, 90, 1.1], [0.65, 50, 1.7]]) {
    for (let i = 0; i < count; i++)
      layers.push({ x: (rnd() - 0.5) * 4000, y: (rnd() - 0.5) * 4000,
                    d: depth, r: size * (0.5 + rnd()), a: 0.12 + rnd() * 0.25 });
  }
  return layers;
})();
function drawStars() {
  for (const s of STARS) {
    const sx = s.x * scale * s.d + tx * s.d + cw * (1 - s.d) / 2;
    const sy = s.y * scale * s.d + ty * s.d + ch * (1 - s.d) / 2;
    if (sx < -8 || sx > cw + 8 || sy < -8 || sy > ch + 8) continue;
    ctx.globalAlpha = s.a;
    ctx.fillStyle = "#cdd6f4";
    ctx.beginPath(); ctx.arc(sx, sy, s.r, 0, Math.PI * 2); ctx.fill();
  }
  ctx.globalAlpha = 1;
}
```

Call `drawStars()` first thing in `draw()` after `clearRect`. (`projectName` is defined later in the file today — move the HUD `projectName`/`rootPath` block **above** the model section so `STARS` can seed from it.)

- [ ] **Step 2: Verify** — `cargo test -p selene-cli` (self-containment holds), rebuild release, regenerate `/tmp/galaxy.html`, open, eyeball: stars drift slower than nodes when panning; zero regressions in map/symbols/clusters modes.

- [ ] **Step 3: Commit** — `git commit -am "feat(viz): 2.5D parallax starfield — depth with zero dependencies"`

---

### Task 7: full-suite verification + docs

- [ ] **Step 1:** `cargo test --workspace 2>&1 | grep -c 'test result: FAILED'` → must print `0` (never `| head` — it has lied before). `cargo clippy --all-targets` clean. `cargo fmt --check`.
- [ ] **Step 2:** Real-binary sweep: `selene index` on the dogfood corpus, `selene viz` (grep communities/hubs), `selene report` (eyeball), `/tmp/ask.sh` gate question still 3/3, explore answer carries the token-economy line.
- [ ] **Step 3:** README: add `selene report` to the command list; one line under viz about Clusters/insights. RESUME.md gets a dated addendum line pointing here.
- [ ] **Step 4: Commit** — `git commit -am "docs: report + clusters in README, plan addendum"`

---

## Self-review notes

- **Spec coverage:** goal item 1 → Tasks 1–3 (detection + button); item 2 → Task 4; item 3 → Tasks 2–3 (HUD); item 4 → Task 5; item 5 → Task 6 (2.5D chosen per the goal's own lean, no `--3d` mode, zero-dep held).
- **Type consistency:** `detect_communities(n, &[(usize,usize)]) -> Vec<usize>` is consumed with exactly that shape in Tasks 2 and 5; `token_economy_line(usize, u64, usize) -> Option<String>` matches its wiring; JSON keys (`c`, `communities`, `hubs`, `surprises`) match between Task 2 (producer) and Task 3 (consumer).
- **Known risk:** `pair_counts` is consumed by `into_iter` in current code — Task 2 explicitly notes cloning/ordering (`mod_links_sorted` is reused instead, which still exists). The `phase4_gate` may pin output endings — Task 4 step 4 addresses it head-on.
