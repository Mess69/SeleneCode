//! THROWAWAY diagnostic — not part of the build product. Deleted before commit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use indexmap::{IndexMap, IndexSet};
use selene_context::{
    FindOptions, RWR_EDGE_KINDS, extract_search_terms, is_test_file, score_candidates,
    sort_candidates, term_groups,
};
use selene_db::{GraphStore, SurrealStore};
use selene_graph::QueryManager;
use std::path::PathBuf;

/// Hypothesis: the answer to "how does an X become a Y" is the code that touches BOTH an
/// X-concept seed and a Y-concept seed. Score by CONCEPTS BRIDGED, not seeds bridged.
#[tokio::main]
async fn main() {
    let root = PathBuf::from("/tmp/dogfood-selene");
    let store = SurrealStore::open(&root.join(".selene")).await.unwrap();
    let qm = QueryManager::new(store, root.clone());
    let opts = FindOptions::default();

    for query in [
        "how does an unresolved reference become a graph edge",
        "how are edges created during resolution",
    ] {
        println!("\n######## {query}");
        let terms = extract_search_terms(query);
        let groups = term_groups(&terms);
        println!("concepts: {:?}", groups.iter().map(|g| &g[0]).collect::<Vec<_>>());

        let mut lex = score_candidates(&qm, query, &opts, None).await.unwrap();
        sort_candidates(&mut lex);
        let seeds: Vec<_> = lex
            .iter()
            .filter(|s| !is_test_file(&s.node.file_path))
            .take(20)
            .collect();

        // Which CONCEPT does each seed represent?
        let concept_of = |name: &str, path: &str| -> IndexSet<usize> {
            let hay = format!("{name} {path}").to_lowercase();
            groups
                .iter()
                .enumerate()
                .filter(|(_, g)| g.iter().any(|t| hay.contains(&t.to_lowercase())))
                .map(|(i, _)| i)
                .collect()
        };

        println!("seeds (concept ids):");
        for s in seeds.iter().take(10) {
            println!(
                "   {:<24} {:?}",
                s.node.name,
                concept_of(&s.node.name, &s.node.file_path)
            );
        }

        // 1-hop undirected: which concepts does each neighbor bridge?
        let seed_ids: Vec<String> = seeds.iter().map(|s| s.node.id.clone()).collect();
        let out = qm.store().outgoing_batch(&seed_ids, RWR_EDGE_KINDS).await.unwrap();
        let inc = qm.store().incoming_batch(&seed_ids, RWR_EDGE_KINDS).await.unwrap();

        let mut bridged: IndexMap<String, (IndexSet<usize>, String, String)> = IndexMap::new();
        for s in &seeds {
            let cs = concept_of(&s.node.name, &s.node.file_path);
            if cs.is_empty() {
                continue;
            }
            for e in out
                .get(&s.node.id)
                .into_iter()
                .flatten()
                .chain(inc.get(&s.node.id).into_iter().flatten())
            {
                if !opts.node_kinds.contains(&e.node.kind) || is_test_file(&e.node.file_path) {
                    continue;
                }
                let slot = bridged.entry(e.node.id.clone()).or_insert_with(|| {
                    (IndexSet::new(), e.node.name.clone(), e.node.file_path.clone())
                });
                slot.0.extend(cs.iter().copied());
            }
        }

        let mut ranked: Vec<_> = bridged.values().filter(|(c, _, _)| c.len() >= 2).collect();
        ranked.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(&b.1)));
        println!("--- neighbors bridging >=2 CONCEPTS ---");
        for (c, name, path) in ranked.iter().take(16) {
            println!("   concepts={} {:<34} {}", c.len(), name, path);
        }
    }
}
