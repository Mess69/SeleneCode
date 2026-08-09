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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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
