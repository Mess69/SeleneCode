#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Metadata KV + aggregate stats + full clear (`src/meta.rs`) and search
//! candidate fetch — FTS, LIKE fallback, exact-name batch, distinct names
//! (`src/search.rs`) — Task 7.

#[cfg(feature = "kv-mem")]
use selene_core::{Edge, EdgeKind, Language, Node, NodeKind, Visibility};
#[cfg(feature = "kv-mem")]
use selene_db::{FileRecord, SurrealStore};

/// A minimal but valid `Node`. Mirrors `tests/store_test.rs`'s `node` helper
/// (kept independent — integration test crates don't share code across test
/// binaries without a `tests/common/mod.rs`, which isn't otherwise needed
/// here).
#[cfg(feature = "kv-mem")]
#[allow(clippy::too_many_arguments)]
fn node_full(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: &str,
    docstring: Option<&str>,
    signature: Option<&str>,
) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        file_path: file_path.to_string(),
        language: Language::from_wire(language).expect("test language"),
        start_line: 1,
        end_line: 2,
        start_column: 0,
        end_column: 1,
        docstring: docstring.map(str::to_string),
        signature: signature.map(str::to_string),
        visibility: Some(Visibility::Public),
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

/// A bare node: no docstring/signature, `qualifiedName == name`.
#[cfg(feature = "kv-mem")]
fn node(id: &str, kind: NodeKind, name: &str, file_path: &str, language: &str) -> Node {
    node_full(id, kind, name, name, file_path, language, None, None)
}

#[cfg(feature = "kv-mem")]
fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: None,
        line: None,
        column: None,
        provenance: None,
    }
}

#[cfg(feature = "kv-mem")]
fn file_record(path: &str, language: &str) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: "h".to_string(),
        language: language.to_string(),
        size: 10,
        modified_at: 1,
        indexed_at: 1,
        node_count: 1,
        errors: vec![],
    }
}

#[cfg(feature = "kv-mem")]
async fn fresh_store() -> SurrealStore {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store
}

// =============================================================================
// Metadata KV
// =============================================================================

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn meta_set_get_round_trip_overwrite_and_missing() {
    let store = fresh_store().await;

    assert_eq!(
        store.get_meta("foo").await.unwrap(),
        None,
        "unset key is None, not an error"
    );

    store.set_meta("foo", "bar").await.unwrap();
    assert_eq!(
        store.get_meta("foo").await.unwrap(),
        Some("bar".to_string())
    );

    store.set_meta("foo", "baz").await.unwrap();
    assert_eq!(
        store.get_meta("foo").await.unwrap(),
        Some("baz".to_string()),
        "set_meta overwrites the same key"
    );

    // schema_version, seeded by apply_schema, is untouched by an unrelated key's set_meta.
    assert_eq!(store.schema_version().await.unwrap(), Some(1));
    assert_eq!(
        store.get_meta("schema_version").await.unwrap(),
        Some("1".to_string()),
        "get_meta reads the same meta table apply_schema seeded"
    );
}

// =============================================================================
// Stats
// =============================================================================

/// 3 nodes (kinds: function×2, class×1; languages: rust×2, python×1), 2 edges
/// (calls×1, references×1), 2 files (rust×1, python×1).
#[cfg(feature = "kv-mem")]
async fn stats_fixture(store: &SurrealStore) {
    let n1 = node("function:n1", NodeKind::Function, "n1", "src/a.rs", "rust");
    let n2 = node("function:n2", NodeKind::Function, "n2", "src/a.rs", "rust");
    let n3 = node("class:n3", NodeKind::Class, "n3", "src/b.py", "python");
    store
        .insert_nodes(&[n1.clone(), n2.clone(), n3.clone()])
        .await
        .unwrap();
    store
        .insert_edges(&[
            edge(&n1.id, &n2.id, EdgeKind::Calls),
            edge(&n1.id, &n3.id, EdgeKind::References),
        ])
        .await
        .unwrap();
    store
        .upsert_file(&file_record("src/a.rs", "rust"))
        .await
        .unwrap();
    store
        .upsert_file(&file_record("src/b.py", "python"))
        .await
        .unwrap();
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn stats_and_node_edge_count_match_the_exact_fixture() {
    let store = fresh_store().await;
    stats_fixture(&store).await;

    let stats = store.stats().await.unwrap();
    assert_eq!(stats.nodes, 3);
    assert_eq!(stats.edges, 2);
    assert_eq!(stats.files, 2);

    let mut expected_nodes_by_kind = std::collections::BTreeMap::new();
    expected_nodes_by_kind.insert("function".to_string(), 2);
    expected_nodes_by_kind.insert("class".to_string(), 1);
    assert_eq!(stats.nodes_by_kind, expected_nodes_by_kind);

    let mut expected_edges_by_kind = std::collections::BTreeMap::new();
    expected_edges_by_kind.insert("calls".to_string(), 1);
    expected_edges_by_kind.insert("references".to_string(), 1);
    assert_eq!(stats.edges_by_kind, expected_edges_by_kind);

    // `languages` is a FILE count per language (2 nodes are rust but only ONE
    // rust FILE is tracked) — see src/meta.rs's module docs for the TS-parity
    // rationale (`filesByLanguage`, grouped over `files`, never `nodes`).
    let mut expected_languages = std::collections::BTreeMap::new();
    expected_languages.insert("rust".to_string(), 1);
    expected_languages.insert("python".to_string(), 1);
    assert_eq!(
        stats.languages, expected_languages,
        "languages counts FILES per language, not nodes (would be rust:2,python:1 if node-counted)"
    );

    assert_eq!(store.node_edge_count().await.unwrap(), (3, 2));
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn clear_empties_everything_but_meta_survives() {
    let store = fresh_store().await;
    stats_fixture(&store).await;
    store
        .insert_unresolved(&[selene_db::UnresolvedRef {
            from_node_id: "function:n1".to_string(),
            reference_name: "target".to_string(),
            reference_kind: "call".to_string(),
            line: None,
            column: None,
            candidates: vec![],
            file_path: "src/a.rs".to_string(),
            language: Language::Rust,
            status: selene_db::RefStatus::Pending,
            name_tail: "target".to_string(),
        }])
        .await
        .unwrap();
    store.set_meta("customKey", "customValue").await.unwrap();

    store.clear().await.unwrap();

    assert_eq!(store.node_edge_count().await.unwrap(), (0, 0));
    assert_eq!(store.stats().await.unwrap().files, 0);
    assert!(store.all_files().await.unwrap().is_empty());
    assert_eq!(store.unresolved_pending_count().await.unwrap(), 0);
    assert!(store.get_node("function:n1").await.unwrap().is_none());

    // Meta is untouched by clear() — schema_version AND any other key survive.
    assert_eq!(
        store.schema_version().await.unwrap(),
        Some(1),
        "schema_version must survive clear()"
    );
    assert_eq!(
        store.get_meta("customKey").await.unwrap(),
        Some("customValue".to_string()),
        "clear() must not touch the meta table at all, not even non-schema_version keys"
    );
}

// =============================================================================
// FTS candidate fetch (search_fts)
// =============================================================================

/// Filler nodes so FTS corpora aren't degenerately tiny (Task 1 finding: BM25
/// can go negative on a 1-2 document corpus). Distinct, unrelated content.
#[cfg(feature = "kv-mem")]
fn filler_nodes() -> Vec<Node> {
    vec![
        node_full(
            "function:filler1",
            NodeKind::Function,
            "unrelatedOne",
            "unrelatedOne",
            "src/filler1.rs",
            "rust",
            Some("does something else entirely"),
            Some("fn unrelatedOne()"),
        ),
        node_full(
            "function:filler2",
            NodeKind::Function,
            "unrelatedTwo",
            "unrelatedTwo",
            "src/filler2.rs",
            "rust",
            Some("also unrelated"),
            Some("fn unrelatedTwo()"),
        ),
        node_full(
            "function:filler3",
            NodeKind::Function,
            "unrelatedThree",
            "unrelatedThree",
            "src/filler3.rs",
            "rust",
            Some("filler content here"),
            Some("fn unrelatedThree()"),
        ),
    ]
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_camel_case_term_matches_humped_identifier() {
    let store = fresh_store().await;
    store.insert_nodes(&filler_nodes()).await.unwrap();
    let n = node_full(
        "method:calcTotal",
        NodeKind::Method,
        "calculateTotal",
        "Billing.calculateTotal",
        "src/billing.rs",
        "rust",
        Some("adds up the invoice"),
        Some("fn calculateTotal(&self) -> f64"),
    );
    store.insert_nodes(std::slice::from_ref(&n)).await.unwrap();

    let hits = store
        .search_fts(&["total".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|c| c.node.id == n.id),
        "the camel analyzer must split calculateTotal so 'total' matches it; got {:?}",
        hits.iter().map(|c| &c.node.id).collect::<Vec<_>>()
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_matches_via_qualified_name_only() {
    let store = fresh_store().await;
    store.insert_nodes(&filler_nodes()).await.unwrap();
    let n = node_full(
        "function:run3",
        NodeKind::Function,
        "run",
        "UniqueModule.veryDistinctQualifiedNameXyz",
        "src/u.rs",
        "rust",
        None,
        None,
    );
    store.insert_nodes(std::slice::from_ref(&n)).await.unwrap();

    let hits = store
        .search_fts(&["distinct".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|c| c.node.id == n.id),
        "must match via the qualifiedName field even though name ('run') does not contain the term"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_kinds_and_languages_filters() {
    let store = fresh_store().await;
    let rust_fn = node_full(
        "function:sharedWord",
        NodeKind::Function,
        "sharedWordRust",
        "sharedWordRust",
        "src/a.rs",
        "rust",
        None,
        None,
    );
    let py_class = node_full(
        "class:sharedWord",
        NodeKind::Class,
        "sharedWordPython",
        "sharedWordPython",
        "src/b.py",
        "python",
        None,
        None,
    );
    store
        .insert_nodes(&[rust_fn.clone(), py_class.clone()])
        .await
        .unwrap();

    let both = store
        .search_fts(&["shared".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();
    assert_eq!(both.len(), 2, "no filter: both match");

    let kind_filtered = store
        .search_fts(&["shared".to_string()], &[NodeKind::Function], &[], 20, 0)
        .await
        .unwrap();
    assert_eq!(kind_filtered.len(), 1);
    assert_eq!(kind_filtered[0].node.id, rust_fn.id);

    let lang_filtered = store
        .search_fts(&["shared".to_string()], &[], &["python".to_string()], 20, 0)
        .await
        .unwrap();
    assert_eq!(lang_filtered.len(), 1);
    assert_eq!(lang_filtered[0].node.id, py_class.id);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_limit_and_offset_page_through_without_overlap() {
    let store = fresh_store().await;
    let names = [
        "alphaWidget",
        "betaWidget",
        "gammaWidget",
        "deltaWidget",
        "epsilonWidget",
    ];
    let nodes: Vec<Node> = names
        .iter()
        .map(|n| {
            node_full(
                &format!("function:{n}"),
                NodeKind::Function,
                n,
                n,
                &format!("src/{n}.rs"),
                "rust",
                None,
                None,
            )
        })
        .collect();
    store.insert_nodes(&nodes).await.unwrap();

    let page1 = store
        .search_fts(&["widget".to_string()], &[], &[], 2, 0)
        .await
        .unwrap();
    let page2 = store
        .search_fts(&["widget".to_string()], &[], &[], 2, 2)
        .await
        .unwrap();
    let page3 = store
        .search_fts(&["widget".to_string()], &[], &[], 2, 4)
        .await
        .unwrap();

    assert_eq!(page1.len(), 2, "limit respected");
    assert_eq!(page2.len(), 2);
    assert_eq!(page3.len(), 1, "final page has the remainder");

    let mut all_ids: Vec<String> = page1
        .iter()
        .chain(page2.iter())
        .chain(page3.iter())
        .map(|c| c.node.id.clone())
        .collect();
    all_ids.sort_unstable();
    let mut expected_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    expected_ids.sort_unstable();
    assert_eq!(
        all_ids, expected_ids,
        "offset must page through every match exactly once, no dupes/gaps"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_empty_terms_yield_empty_not_error() {
    let store = fresh_store().await;
    store.insert_nodes(&filler_nodes()).await.unwrap();

    assert!(
        store
            .search_fts(&[], &[], &[], 20, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search_fts(&["   ".to_string(), "".to_string()], &[], &[], 20, 0)
            .await
            .unwrap()
            .is_empty(),
        "blank-only terms must also short-circuit to empty"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_nonsense_term_yields_empty_not_error() {
    let store = fresh_store().await;
    store.insert_nodes(&filler_nodes()).await.unwrap();

    let hits = store
        .search_fts(&["zzznonexistentxyz123".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();
    assert!(hits.is_empty());
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_fts_name_hit_outranks_docstring_only_hit() {
    let store = fresh_store().await;
    store.insert_nodes(&filler_nodes()).await.unwrap();

    let name_hit = node_full(
        "function:zzzuniqueword",
        NodeKind::Function,
        "zzzuniqueword",
        "zzzuniqueword",
        "src/name_hit.rs",
        "rust",
        None,
        None,
    );
    let docstring_hit = node_full(
        "function:mentionsIt",
        NodeKind::Function,
        "somethingElse",
        "somethingElse",
        "src/doc_hit.rs",
        "rust",
        Some("this docstring mentions zzzuniqueword in passing"),
        None,
    );
    store
        .insert_nodes(&[name_hit.clone(), docstring_hit.clone()])
        .await
        .unwrap();

    let hits = store
        .search_fts(&["zzzuniqueword".to_string()], &[], &[], 20, 0)
        .await
        .unwrap();

    let name_rank = hits.iter().position(|c| c.node.id == name_hit.id);
    let doc_rank = hits.iter().position(|c| c.node.id == docstring_hit.id);
    assert!(name_rank.is_some() && doc_rank.is_some(), "both must match");
    assert!(
        name_rank < doc_rank,
        "the name-weighted (20x) hit must rank above the docstring-weighted (1x) hit: {hits:?}"
    );
}

// =============================================================================
// LIKE fallback candidate fetch (search_name_like)
// =============================================================================

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_name_like_orders_exact_prefix_contains_and_qualified_tiers() {
    let store = fresh_store().await;

    let exact = node(
        "function:foo1",
        NodeKind::Function,
        "Foo",
        "src/a.rs",
        "rust",
    );
    let starts = node(
        "function:foo2",
        NodeKind::Function,
        "FooBar",
        "src/a.rs",
        "rust",
    );
    let contains = node(
        "function:foo3",
        NodeKind::Function,
        "xFooy",
        "src/a.rs",
        "rust",
    );
    let qualified_only = node_full(
        "function:foo4",
        NodeKind::Function,
        "Other",
        "Namespace.foo.Other",
        "src/a.rs",
        "rust",
        None,
        None,
    );
    store
        .insert_nodes(&[
            exact.clone(),
            starts.clone(),
            contains.clone(),
            qualified_only.clone(),
        ])
        .await
        .unwrap();

    let hits = store.search_name_like("foo", &[], 20).await.unwrap();
    let ids: Vec<&str> = hits.iter().map(|c| c.node.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "function:foo1",
            "function:foo2",
            "function:foo3",
            "function:foo4"
        ],
        "exact(1.0) > starts-with(0.9) > contains(0.8) > qualified-contains(0.7)"
    );
    assert_eq!(hits[0].raw_score, 1.0);
    assert_eq!(hits[1].raw_score, 0.9);
    assert_eq!(hits[2].raw_score, 0.8);
    assert_eq!(hits[3].raw_score, 0.7);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_name_like_ties_break_by_shorter_name_first() {
    let store = fresh_store().await;
    let short = node(
        "function:short",
        NodeKind::Function,
        "fooA",
        "src/a.rs",
        "rust",
    );
    let long = node(
        "function:long",
        NodeKind::Function,
        "fooAB",
        "src/a.rs",
        "rust",
    );
    // Insert longer name first to prove the order isn't insertion order.
    store
        .insert_nodes(&[long.clone(), short.clone()])
        .await
        .unwrap();

    let hits = store.search_name_like("foo", &[], 20).await.unwrap();
    assert_eq!(
        hits[0].node.id, short.id,
        "both are starts-with ties; shorter name ranks first"
    );
    assert_eq!(hits[1].node.id, long.id);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_name_like_kinds_filter() {
    let store = fresh_store().await;
    let f = node(
        "function:foo1",
        NodeKind::Function,
        "Foo",
        "src/a.rs",
        "rust",
    );
    let c = node(
        "class:foo2",
        NodeKind::Class,
        "FooClass",
        "src/a.rs",
        "rust",
    );
    store.insert_nodes(&[f.clone(), c.clone()]).await.unwrap();

    let hits = store
        .search_name_like("foo", &[NodeKind::Class], 20)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node.id, c.id);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn search_name_like_blank_query_is_empty_not_error() {
    let store = fresh_store().await;
    store
        .insert_nodes(&[node(
            "function:a",
            NodeKind::Function,
            "a",
            "src/a.rs",
            "rust",
        )])
        .await
        .unwrap();
    assert!(
        store
            .search_name_like("   ", &[], 20)
            .await
            .unwrap()
            .is_empty()
    );
}

// =============================================================================
// find_by_exact_names / all_node_names
// =============================================================================

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn find_by_exact_names_is_case_insensitive_and_caps_per_name() {
    let store = fresh_store().await;
    let alpha = node(
        "function:alpha",
        NodeKind::Function,
        "Alpha",
        "src/a.rs",
        "rust",
    );
    let beta = node(
        "function:beta",
        NodeKind::Function,
        "beta",
        "src/a.rs",
        "rust",
    );
    let dups: Vec<Node> = ["d1", "d2", "d3"]
        .iter()
        .map(|n| {
            node(
                &format!("function:{n}"),
                NodeKind::Function,
                "Dup",
                "src/a.rs",
                "rust",
            )
        })
        .collect();
    store
        .insert_nodes(&[alpha.clone(), beta.clone()])
        .await
        .unwrap();
    store.insert_nodes(&dups).await.unwrap();

    let found = store
        .find_by_exact_names(&["alpha".to_string(), "BETA".to_string()], 10)
        .await
        .unwrap();
    let mut ids: Vec<&str> = found.iter().map(|n| n.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec!["function:alpha", "function:beta"],
        "case-insensitive exact match on both queried names"
    );

    let capped = store
        .find_by_exact_names(&["Dup".to_string()], 2)
        .await
        .unwrap();
    assert_eq!(capped.len(), 2, "per-name cap enforced");

    assert!(store.find_by_exact_names(&[], 10).await.unwrap().is_empty());
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn all_node_names_is_distinct_and_complete() {
    let store = fresh_store().await;
    let a1 = node("function:a1", NodeKind::Function, "a", "src/a.rs", "rust");
    let a2 = node("function:a2", NodeKind::Function, "a", "src/b.rs", "rust");
    let b1 = node("function:b1", NodeKind::Function, "b", "src/a.rs", "rust");
    store.insert_nodes(&[a1, a2, b1]).await.unwrap();

    let mut names = store.all_node_names().await.unwrap();
    names.sort_unstable();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
}
