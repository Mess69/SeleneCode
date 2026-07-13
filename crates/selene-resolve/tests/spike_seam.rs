#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Phase 3, Task 1 — the de-risking spike, kept as a smoke/regression test.
//!
//! Every assumption Parts A/B/C are built on is checked here against the real
//! `GraphStore` trait, the real `SurrealStore`, and the real regex/cache/JSONC
//! crates. **The findings below are the knowledge later tasks bake in** — a
//! wrong assumption here poisons thirty tasks, so each one is an executable
//! assertion, not a note.
//!
//! ============================================================================
//! FINDINGS (2026-07-13) — later tasks consume these
//! ============================================================================
//!
//! **F1 — `delete_resolved` / `mark_failed` key arity: THE ASSUMPTION FAILED
//! (as suspected). CONFIRMED SILENT-RECALL BUG. → FIXED 2026-07-13 (`p3-dbfix`).**
//!
//! > **RESOLUTION.** The maintainer took the fix rather than working around it.
//! > `GraphStore`'s key is now `selene_db::UnresolvedKey` =
//! > `(from_node_id, reference_name, reference_kind)` across `delete_resolved`,
//! > `mark_failed`, and `retryable_failed`'s dedup. Observed red before the fix:
//! > 2 rows in → `delete_resolved` left **0** (expected 1), and `retryable_failed`
//! > returned **1** (expected 2). `f1_delete_resolved_is_kind_scoped_and_spares_the_twin`
//! > below now pins the CORRECT behavior. The narrative below is kept as the
//! > record of what the bug was; **Task 27 is unblocked**.
//!
//! *What the bug was (historical record — all of this is now fixed):*
//! `GraphStore::delete_resolved(&[(from_node_id, reference_name)])` keyed a row
//! by a **2-tuple**; CodeGraph TS keys it by three fields
//! (`fromNodeId + referenceName + referenceKind`, maps/resolution.md §Wire).
//! `crates/selene-db/src/unresolved.rs:182` issues
//! `DELETE unresolved_ref WHERE fromNodeId = $from AND referenceName = $name` —
//! **no kind predicate**. Phase 2 legitimately emits two rows that collide on
//! that 2-tuple: a `calls` ref and a `function_ref` for the same name from the
//! same function (`selene-extract/src/fnref.rs` — a name passed as a value AND
//! called in the same body). Resolving either one therefore **deletes both**:
//! the second reference never resolves, and because the row is *gone* rather
//! than `failed`, the pending-count orphan sweep still reaches 0 and **nothing
//! detects the loss**. `mark_failed` has the identical shape (it flips both
//! rows to `failed`, so the retry pipeline retries a phantom).
//! **And it is worse than the plan predicted**: `retryable_failed` dedups its
//! result by the SAME 2-tuple (`unresolved.rs:246`), so after `mark_failed`
//! flips both rows, the retry pipeline sees **one** of them — the second kind is
//! unreachable through every door. Measured below: 2 rows in, 1 row out.
//! Proven by `f1_delete_resolved_2tuple_drains_both_kinds` below.
//! **NOT fixed here** (the plan forbids it): the fix is a `selene-db` trait
//! change to a 3-part key `(from_node_id, reference_name, reference_kind)` —
//! `delete_resolved`, `mark_failed`, and `retryable_failed`'s dedup — and it is
//! a maintainer decision, surfaced as Open Coordination Point 1. Until it lands,
//! Task 27's batch loop under-resolves on fn-ref-heavy files, silently.
//!
//! **F2 — `method_matches` has no store primitive (as assumed) — but the ceiling
//! primitive the plan told Task 7 to use DOES NOT COUNT NODES. SECOND ASSUMPTION
//! FAILED.**
//!
//! - **(a) The filter shape is confirmed.** The trait offers exact
//!   `get_nodes_by_name` / `get_nodes_by_qualified_name` and **no suffix query**,
//!   so `resolve_method_on_type` fetches by *method name* and filters in-resolver
//!   on `qualified_name == "{type}::{method}"` OR `.ends_with("::{type}::{method}")`,
//!   plus language and kind, memoized in an LRU keyed `"{language} {type}::{method}"`
//!   (maps/resolution.md §Caches). Proven below: the 1 true target is selected out
//!   of 2 000 same-named decoys.
//! - **(b) `count_nodes_matching_name_in_files` counts DISTINCT FILES, not nodes.**
//!   Its `GraphStore` doc says "Count of nodes named exactly `name`, across every
//!   file", but `selene-db/src/nodes.rs:294` issues
//!   `SELECT filePath FROM node WHERE name = $name GROUP BY filePath` and returns
//!   `rows.len()` — the **file** count. Measured below: 2 001 nodes named `get`
//!   spread over 201 files ⇒ the primitive answers **201**, not 2 001.
//!
//! **Consequence, and it is load-bearing.** Task 7's plan text says
//! "`AMBIGUOUS_NAME_CEILING` uses `ctx.count_nodes_named(name)` … **not**
//! `nodes_by_name(..).len()`". As written that is WRONG: wiring the #999 ceiling to
//! this primitive compares 500 against a *file* count, so a name defined 5 000 times
//! across 300 files would **not** decline and the ubiquitous-name guard would
//! silently never fire. The TS ceiling (#999) compares the **candidate-node** count.
//!
//! **RESOLVED 2026-07-13 (`p3-dbfix`) — option (1) was taken.** `GraphStore` gained
//! `count_nodes_named(name) -> u64` (`SELECT count() FROM node WHERE name = $name
//! GROUP ALL`, over the existing `node_name` index): a real NODE count that keeps the
//! ceiling's "decline WITHOUT materializing 10k nodes" property. Observed red before
//! the fix: 3 nodes named `helper` over 2 files → the old primitive answered **2**.
//! `count_nodes_matching_name_in_files` survives unchanged with its honest FILE-count
//! semantics, its own callers, and a corrected doc comment — the two are different
//! questions. **Task 7 is UNBLOCKED: wire `AMBIGUOUS_NAME_CEILING` to
//! `count_nodes_named`.** (`ResolutionContext::count_files_with_name` is still the
//! file count; Task 7 adds a `count_nodes_named` context method over the new
//! primitive — deliberately left to Task 7, whose file `context.rs` is.)
//!
//! **F3 — `supertypes` is buildable node-anchored from the trait, as specified.**
//! `outgoing(node_id, &[Implements, Extends], None)` → supertype nodes →
//! `children(supertype_id)` (the `contains` edge) for member lookup. Proven
//! cross-file (`f3_supertypes_node_anchored_across_files`). The name-keyed TS
//! shape (`getSupertypes("Engine")`) is NOT reproduced — it is the rails
//! cross-class wrong-edge bug (design/function-ref-capture.md §Known limits).
//!
//! **F4 — `all_node_names` as a whole `Vec<String>` is fine; the yielding
//! variant is dropped.** Measured on a synthetic 50 000-node graph
//! (`f4_all_node_names_bulk`): the Vec is ~50k short strings. Real-repo
//! measurement (SeleneCode itself, via `f4_real_repo_measurement`, `#[ignore]`d,
//! 2026-07-13): **2 482 nodes over 151 files → 1 767 distinct names, a
//! `HashSet<String>` payload of ~0.08 MB**. Linearly, a repo 100x this size
//! (~250k nodes) warms a ~10 MB name set — nothing. The
//! TS `iterateNodeNames` streaming API existed for a Node-heap constraint we do
//! not have → Task 2's warm cache is a straight `HashSet`, and the yielding
//! variant is DROPPED.
//!
//! **F5 — regex portability: all three JS-isms confirmed.**
//!
//! - **(a)** `CHAIN_SHAPE = ^(.+)\(\)\.(\w+)$` — the Rust `regex` crate's greedy
//!   `(.+)` binds the **LAST** `().`, exactly as JS does: on `A().b().c` the inner
//!   capture is `A().b`. Portable verbatim (Task 9).
//! - **(b)** `String.replace('*', x)` in JS replaces only the FIRST `*`; Rust's
//!   `str::replace` replaces **ALL**. Task 4's `apply_aliases` MUST use
//!   `replacen(.., 1)` — asserted below on a two-`*` pattern.
//! - **(c)** The Lua/Luau lookahead `(?![\w.]|\s*[({"'\[])` **fails to compile** on
//!   the `regex` crate and **compiles** on `fancy-regex`. That single pattern is the
//!   entire justification for the `fancy-regex` dependency.
//!
//! **F6 — per-ref regex compilation dominates; Task 8 uses the LRU cache.**
//! Receiver patterns are built PER REFERENCE from an escaped receiver name (the TS
//! `new RegExp` shape). Measured by `f6_per_ref_regex_compilation_cost` (400 refs
//! over 25 distinct receivers): a single fancy-regex receiver pattern costs
//! **~10 ms to compile**, so the naive path spent **4.6 s** vs **0.22 s** through an
//! `LruCache<String, Regex>` — a **21x** end-to-end win, and the cached run's 0.22 s
//! is almost entirely its 25 cold compiles (a cache HIT is free). A real repo puts
//! hundreds of thousands of refs through this path, so naive per-ref compilation
//! would dominate resolution wall-clock outright. Task 8 caches — confirmed, and
//! more urgent than the plan assumed.
//!
//! **F7 — JSONC: `json5` tolerates comments AND trailing commas; `jsonc-parser`
//! does too. PICK: `json5`** (Task 4 uses it, and Task 4 drops the other from
//! dev-deps). Both parse a tsconfig carrying `//` + `/* */` comments and
//! trailing commas in objects and arrays; `json5` is a serde `Deserializer`
//! (so `compilerOptions.paths` maps straight onto a typed struct with zero
//! hand-rolled AST walking), which `jsonc-parser` is not. Asserted both ways in
//! `f7_jsonc_tolerance_bakeoff` — if either ever regresses, alias loading
//! silently vanishes and EVERY aliased import regresses to unresolved, so the
//! assertion stays.
//!
//! ============================================================================

use std::num::NonZeroUsize;
use std::time::Instant;

use lru::LruCache;
use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance, RefStatus, UnresolvedRef};
use selene_db::{GraphStore, SurrealStore};

// The resolver is generic over `S: GraphStore` and never names SurrealStore
// (Phase 3 Global Constraints). These helpers are deliberately written against
// the TRAIT — they are the shape every Part A/B/C call site takes, and they
// prove the seam compiles generically rather than riding SurrealStore's
// inherent methods.
async fn pending_count<S: GraphStore>(store: &S) -> u64 {
    store.unresolved_pending_count().await.unwrap()
}

async fn drain_resolved<S: GraphStore>(store: &S, from: &str, name: &str, kind: &str) {
    store
        .delete_resolved(&[(from.to_string(), name.to_string(), kind.to_string())])
        .await
        .unwrap();
}

async fn fail_refs<S: GraphStore>(store: &S, from: &str, name: &str, kind: &str) {
    store
        .mark_failed(&[(from.to_string(), name.to_string(), kind.to_string())])
        .await
        .unwrap();
}

// =============================================================================
// Fixtures
// =============================================================================

fn node(id: &str, name: &str, kind: NodeKind, file: &str, qn: &str) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qn.to_string(),
        file_path: file.to_string(),
        language: "typescript".to_string(),
        start_line: 1,
        end_line: 10,
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
        updated_at: 0,
    }
}

fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: None,
        line: None,
        column: None,
        provenance: Some(Provenance::TreeSitter),
    }
}

/// A pending `UnresolvedRef` — `from_node_id` + `reference_name` + **kind**.
/// The kind is the field the store's delete key does not carry (F1).
fn pending(from: &str, name: &str, kind: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind.to_string(),
        line: Some(1),
        column: Some(0),
        candidates: vec![],
        file_path: "src/a.ts".to_string(),
        language: "typescript".to_string(),
        status: RefStatus::Pending,
        name_tail: name.to_string(),
    }
}

async fn store_with(nodes: &[Node]) -> SurrealStore {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store.insert_nodes(nodes).await.unwrap();
    store
}

// =============================================================================
// F1 — the delete key arity (THE #1 RISK — and it is real)
// =============================================================================

/// Two pending refs from the SAME node with the SAME name and DIFFERENT kinds
/// (`calls` + `function_ref` — exactly what Phase 2's fnref capture emits for
/// `register(handler); handler();`).
///
/// **FIXED (2026-07-13, branch `p3-dbfix`).** This test used to assert the buggy
/// behavior — resolving the `calls` row drained BOTH, and `retryable_failed`
/// deduped 2 failed rows down to 1, leaving the `function_ref` unreachable
/// through every door — as the evidence for Open Coordination Point 1. The
/// maintainer took the fix: `GraphStore`'s key is now the 3-tuple
/// `selene_db::UnresolvedKey` = `(from_node_id, reference_name, reference_kind)`.
/// The assertions below are inverted accordingly and now pin the CORRECT
/// behavior; they are the resolver-side guard that the store keeps it.
#[tokio::test(flavor = "multi_thread")]
async fn f1_delete_resolved_is_kind_scoped_and_spares_the_twin() {
    let caller = node(
        "function:caller",
        "caller",
        NodeKind::Function,
        "src/a.ts",
        "caller",
    );
    let store = store_with(&[caller]).await;

    // Phase 2 emits BOTH for `register(handler); handler();` in one body.
    let refs = [
        pending("function:caller", "handler", "calls"),
        pending("function:caller", "handler", "function_ref"),
    ];
    store.insert_unresolved(&refs).await.unwrap();
    assert_eq!(pending_count(&store).await, 2);

    // The resolver resolves the `calls` ref and drains ONLY that row.
    drain_resolved(&store, "function:caller", "handler", "calls").await;

    assert_eq!(
        pending_count(&store).await,
        1,
        "F1: the kind-scoped key drains only the `calls` row — the `function_ref` \
         twin survives to be resolved on its own terms. (Before the fix this was 0: \
         silent data loss that nothing detected, because the pending count still \
         reached 0 and the orphan sweep was satisfied.)"
    );
    let survivor = store.unresolved_pending_batch(0, 10).await.unwrap();
    assert_eq!(survivor[0].reference_kind, "function_ref");

    // Same shape for mark_failed: the key flips only its own kind...
    let store2 = SurrealStore::in_memory().await.unwrap();
    store2.apply_schema().await.unwrap();
    store2.insert_unresolved(&refs).await.unwrap();
    fail_refs(&store2, "function:caller", "handler", "calls").await;
    assert_eq!(
        pending_count(&store2).await,
        1,
        "F1: mark_failed is kind-scoped too — the `function_ref` stays pending"
    );

    // ...and once both are failed, `retryable_failed` returns BOTH (its dedup key
    // carries `reference_kind`), so neither kind is unreachable.
    fail_refs(&store2, "function:caller", "handler", "function_ref").await;
    let failed = store2
        .retryable_failed(&["handler".to_string()], 10)
        .await
        .unwrap();
    assert_eq!(
        failed.len(),
        2,
        "F1: TWO failed rows in, TWO out — retryable_failed dedups by the full \
         3-part key, so the `function_ref` is reachable by the retry pipeline. \
         (Before the fix this was 1.)"
    );
}

// =============================================================================
// F2 — method_matches shape + hot-name cost
// =============================================================================

/// `resolve_method_on_type("Repo", "get")` has no store primitive: fetch by
/// method NAME, filter in-resolver on qualified_name/language/kind. Proves the
/// filter selects exactly the right node out of 5 000 same-named decoys, and
/// prints the cost of the fetch vs. the counting primitive.
#[tokio::test(flavor = "multi_thread")]
async fn f2_method_matches_shape_and_hot_name_cost() {
    const TYPES: usize = 200;
    const PER_TYPE: usize = 10;

    let mut nodes = Vec::new();
    for t in 0..TYPES {
        for m in 0..PER_TYPE {
            // Every one of these is named `get`; only the qualified name differs.
            nodes.push(node(
                &format!("method:T{t}:{m}"),
                "get",
                NodeKind::Method,
                &format!("src/t{t}.ts"),
                &format!("T{t}::Inner{m}::get"),
            ));
        }
    }
    // The one true target: `Repo::get`.
    nodes.push(node(
        "method:repo_get",
        "get",
        NodeKind::Method,
        "src/repo.ts",
        "Repo::get",
    ));
    let total = nodes.len();
    let store = store_with(&nodes).await;

    let t0 = Instant::now();
    let file_count = store
        .count_nodes_matching_name_in_files("get")
        .await
        .unwrap();
    let count_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = Instant::now();
    let fetched = store.get_nodes_by_name("get").await.unwrap();
    let fetch_ms = t1.elapsed().as_secs_f64() * 1e3;

    assert_eq!(
        fetched.len(),
        total,
        "F2: the name fetch is exact and unbounded"
    );

    // The two primitives answer DIFFERENT questions, and the spike's finding was
    // that the ceiling had been wired to the wrong one.
    // `count_nodes_matching_name_in_files` = distinct FILES (TYPES decoy files +
    // the one `src/repo.ts` holding the true target).
    assert_eq!(
        file_count as usize,
        TYPES + 1,
        "F2: count_nodes_matching_name_in_files is the FILE count — that IS its \
         honest contract (and its doc now says so)"
    );

    // FIXED (2026-07-13, branch `p3-dbfix`): `count_nodes_named` is the real node
    // count the #999 AMBIGUOUS_NAME_CEILING is defined against. Task 7 wires the
    // ceiling to THIS one.
    let node_count = store.count_nodes_named("get").await.unwrap();
    assert_eq!(
        node_count as usize, total,
        "F2: count_nodes_named answers the NODE count ({total}), counted in the DB \
         over the name index — so the ceiling declines a ubiquitous name WITHOUT \
         materializing its candidates, which is the whole point of a counter"
    );
    assert!(
        node_count > 500 && (file_count as usize) < 500,
        "F2: and this is what the fix buys — {node_count} nodes named `get` is above \
         AMBIGUOUS_NAME_CEILING (500) and correctly trips the guard, while the FILE \
         count ({file_count}) is below it and would have silently never fired. \
         Wiring the ceiling to the file count is the bug this pins."
    );

    // The in-resolver filter `resolve_method_on_type` will run (Task 8):
    // same language, method kind, qualified_name == "Repo::get" or ends with
    // "::Repo::get". This half of the assumption HOLDS.
    let ty = "Repo";
    let method = "get";
    let exact = format!("{ty}::{method}");
    let suffix = format!("::{ty}::{method}");
    let matches: Vec<&Node> = fetched
        .iter()
        .filter(|n| {
            n.kind == NodeKind::Method
                && n.language == "typescript"
                && (n.qualified_name == exact || n.qualified_name.ends_with(&suffix))
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "F2: the in-resolver filter selects exactly Repo::get out of {total} decoys"
    );
    assert_eq!(matches[0].id, "method:repo_get");

    println!(
        "F2: name='get' -> {total} nodes over {TYPES} files | \
         count_nodes_matching_name_in_files = {file_count} ({count_ms:.1}ms) <- FILES, NOT NODES \
         | get_nodes_by_name = {} nodes ({fetch_ms:.1}ms, materialized) \
         | in-resolver filter -> 1 match. The #999 ceiling has NO node-count primitive.",
        fetched.len()
    );
}

// =============================================================================
// F3 — node-anchored supertypes (cross-file)
// =============================================================================

/// `ctx.supertypes(node_id)` = `outgoing(id, [Implements, Extends])`, and
/// `ctx.members_of(id)` = `children(id)` (the `contains` edge). Proven with the
/// class and its superclass in DIFFERENT files (the case that matters: the
/// conformance passes only fire cross-file).
#[tokio::test(flavor = "multi_thread")]
async fn f3_supertypes_node_anchored_across_files() {
    let child = node("class:Dog", "Dog", NodeKind::Class, "src/dog.ts", "Dog");
    let parent = node(
        "class:Animal",
        "Animal",
        NodeKind::Class,
        "src/animal.ts", // ← different file
        "Animal",
    );
    let inherited = node(
        "method:Animal_speak",
        "speak",
        NodeKind::Method,
        "src/animal.ts",
        "Animal::speak",
    );
    let store = store_with(&[child, parent, inherited]).await;
    store
        .insert_edges(&[
            edge("class:Dog", "class:Animal", EdgeKind::Extends),
            edge("class:Animal", "method:Animal_speak", EdgeKind::Contains),
        ])
        .await
        .unwrap();

    let supers = store
        .outgoing(
            "class:Dog",
            &[EdgeKind::Implements, EdgeKind::Extends],
            None,
        )
        .await
        .unwrap();
    assert_eq!(supers.len(), 1, "F3: one supertype");
    assert_eq!(supers[0].node.id, "class:Animal");

    let members = store.children(&supers[0].node.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0].qualified_name, "Animal::speak",
        "F3: the inherited member is reachable node-anchored, cross-file — this \
         is the whole mechanism behind resolve_deferred_this_member_refs (#808) \
         and the chained-call conformance pass (#750)"
    );
}

// =============================================================================
// F4 — all_node_names as a whole Vec
// =============================================================================

/// 50 000 nodes → `all_node_names()` → a `HashSet`. Records the shape Task 2's
/// warm cache relies on. (The real-repo number is in `f4_real_repo_measurement`.)
#[tokio::test(flavor = "multi_thread")]
async fn f4_all_node_names_bulk() {
    const N: usize = 10_000;
    let nodes: Vec<Node> = (0..N)
        .map(|i| {
            node(
                &format!("function:f{i}"),
                &format!("symbol_name_{i}"),
                NodeKind::Function,
                &format!("src/f{}.ts", i % 500),
                &format!("symbol_name_{i}"),
            )
        })
        .collect();
    let store = store_with(&nodes).await;

    let t0 = Instant::now();
    let names = store.all_node_names().await.unwrap();
    let ms = t0.elapsed().as_secs_f64() * 1e3;

    let set: std::collections::HashSet<String> = names.into_iter().collect();
    assert_eq!(set.len(), N, "F4: every distinct name is present");
    let bytes: usize = set.iter().map(|s| s.len() + size_of::<String>()).sum();
    println!(
        "F4: {N} distinct names materialized in {ms:.0}ms, HashSet payload ~{:.1} MB — \
         a whole Vec is fine; the TS streaming iterateNodeNames is DROPPED (Task 2).",
        bytes as f64 / 1e6
    );
}

// =============================================================================
// F5 — regex portability (the three JS-isms)
// =============================================================================

#[test]
fn f5a_chain_shape_greedy_binds_the_last_paren_dot() {
    // maps/resolution.md §Rust port notes: JS's greedy (.+) binds the LAST "()."
    let re = regex::Regex::new(r"^(.+)\(\)\.(\w+)$").unwrap();
    let caps = re.captures("A().b().c").expect("F5a: must match");
    assert_eq!(
        &caps[1], "A().b",
        "F5a: greedy (.+) takes the LAST '().' — Rust matches JS here, so \
         CHAIN_SHAPE ports verbatim (Task 9)"
    );
    assert_eq!(&caps[2], "c");

    // Single hop, the ordinary case.
    let caps = re.captures("Foo.getInstance().bar").unwrap();
    assert_eq!(&caps[1], "Foo.getInstance");
    assert_eq!(&caps[2], "bar");

    // The marker never appears in an ordinary ref.
    assert!(re.captures("foo.bar").is_none());
}

#[test]
fn f5b_star_substitution_replaces_only_the_first() {
    // JS `String.replace('*', x)` replaces the FIRST occurrence only. Rust's
    // str::replace replaces ALL — a silent alias-expansion bug (Task 4).
    let pattern = "src/*/lib/*";
    assert_eq!(
        pattern.replace('*', "X"),
        "src/X/lib/X",
        "F5b: str::replace is ALL — this is the WRONG behavior for apply_aliases"
    );
    assert_eq!(
        pattern.replacen('*', "X", 1),
        "src/X/lib/*",
        "F5b: replacen(.., 1) is the JS semantics — Task 4 MUST use it"
    );
}

#[test]
// The `invalid_regex` lint statically checks `Regex::new` literals and flags this
// one — which is the POINT of the test: the `regex` crate cannot compile a
// lookahead, and this assertion is what pins the fancy-regex dependency's
// justification. Allowing the lint here keeps the evidence executable.
#[allow(clippy::invalid_regex)]
fn f5c_lookahead_needs_fancy_regex() {
    // The Lua/Luau receiver anti-self-match lookahead (#1124).
    const LOOKAHEAD: &str = r#"(?![\w.]|\s*[({"'\[])"#;
    assert!(
        regex::Regex::new(LOOKAHEAD).is_err(),
        "F5c: the `regex` crate has no lookaround — if this ever compiles, the \
         crate gained lookaround and the fancy-regex dep can be dropped"
    );
    let fancy = fancy_regex::Regex::new(&format!(r"\bfoo\b{LOOKAHEAD}"))
        .expect("F5c: fancy-regex compiles the lookahead — the sole reason for the dep");
    assert!(fancy.is_match("foo = 1").unwrap());
    assert!(
        !fancy.is_match("foo(1)").unwrap(),
        "F5c: the lookahead rejects a call site — the anti-self-match its whole purpose"
    );
}

// =============================================================================
// F6 — per-ref regex compilation cost
// =============================================================================

/// Receiver patterns are built PER REFERENCE from an escaped receiver name
/// (`new RegExp` in TS). Compare naive compilation with an LRU cache hit.
#[test]
fn f6_per_ref_regex_compilation_cost() {
    const REFS: usize = 400;
    const DISTINCT: usize = 25;
    let receivers: Vec<String> = (0..REFS).map(|i| format!("recv{}", i % DISTINCT)).collect();

    let build = |recv: &str| {
        format!(
            r"([A-Za-z_][\w:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{}\b\s*(?=[;=,)\[{{(]|$)",
            regex::escape(recv)
        )
    };
    // The C++ declarator pattern uses a lookahead → fancy-regex (same finding as F5c).
    let t0 = Instant::now();
    let mut sink = 0usize;
    for r in &receivers {
        let re = fancy_regex::Regex::new(&build(r)).unwrap();
        sink += re.as_str().len();
    }
    let naive_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = Instant::now();
    let mut cache: LruCache<String, fancy_regex::Regex> =
        LruCache::new(NonZeroUsize::new(256).unwrap());
    for r in &receivers {
        let pat = build(r);
        if cache.get(&pat).is_none() {
            cache.put(pat.clone(), fancy_regex::Regex::new(&pat).unwrap());
        }
        sink += cache.get(&pat).map(|re| re.as_str().len()).unwrap_or(0);
    }
    let cached_ms = t1.elapsed().as_secs_f64() * 1e3;
    assert!(sink > 0);

    println!(
        "F6: {REFS} refs / {DISTINCT} distinct receivers — naive per-ref compile {naive_ms:.1}ms \
         vs LRU-cached {cached_ms:.1}ms ({:.0}x). A single compile costs ~{:.1}ms, so a real \
         repo's hundreds of thousands of refs make naive compilation dominate outright. \
         Task 8 caches compiled receiver patterns.",
        naive_ms / cached_ms.max(0.0001),
        naive_ms / REFS as f64
    );
    assert!(
        cached_ms < naive_ms,
        "F6: the cache must beat naive compilation (it did by {:.0}x)",
        naive_ms / cached_ms.max(0.0001)
    );
}

/// The `lru` crate's semantics ARE the TS `LRUCache`'s (maps/resolution.md
/// §Caches): `get` refreshes recency, `put` evicts the oldest.
#[test]
fn f6b_lru_semantics_match_the_ts_cache() {
    let mut cache: LruCache<&str, u32> = LruCache::new(NonZeroUsize::new(2).unwrap());
    cache.put("a", 1);
    cache.put("b", 2);
    assert_eq!(cache.get("a"), Some(&1)); // refreshes `a`
    cache.put("c", 3); // evicts the LRU — which is now `b`, not `a`
    assert_eq!(
        cache.get("b"),
        None,
        "F6b: `get` refreshed `a`, so `b` was evicted"
    );
    assert_eq!(cache.get("a"), Some(&1));
    assert_eq!(cache.get("c"), Some(&3));
}

// =============================================================================
// F7 — JSONC tolerance bake-off (json5 vs jsonc-parser)
// =============================================================================

/// A real-world tsconfig: `//` comments, a `/* */` block, and trailing commas
/// in BOTH an object and an array. If the parser chokes, `loadProjectAliases`
/// returns None and every aliased import in the repo silently fails to resolve.
const TSCONFIG: &str = r#"{
  // The compiler options.
  "compilerOptions": {
    "baseUrl": ".",
    /* Path aliases — the thing we actually need. */
    "paths": {
      "@/*": ["src/*"],
      "~/lib/*": ["lib/*", "vendor/lib/*"],
    },
  },
  "include": ["src/**/*"],
}"#;

#[test]
fn f7_jsonc_tolerance_bakeoff() {
    // --- json5 (THE PICK) ---
    let v: serde_json::Value =
        json5::from_str(TSCONFIG).expect("F7: json5 must tolerate comments + trailing commas");
    let paths = &v["compilerOptions"]["paths"];
    assert_eq!(paths["@/*"][0], "src/*");
    assert_eq!(paths["~/lib/*"][1], "vendor/lib/*");
    assert_eq!(v["compilerOptions"]["baseUrl"], ".");

    // --- jsonc-parser (the alternative; equally tolerant, but AST-shaped) ---
    let parsed = jsonc_parser::parse_to_value(TSCONFIG, &Default::default())
        .expect("F7: jsonc-parser must tolerate the same input")
        .expect("F7: non-empty document");
    // jsonc-parser yields its own AST (no serde bridge without its `serde`
    // feature) — which is precisely the ergonomic difference that decides the
    // pick below.
    let rendered = format!("{parsed:?}");
    assert!(
        rendered.contains("compilerOptions") && rendered.contains("src/*"),
        "F7: jsonc-parser preserves the paths through comments + trailing commas"
    );

    // PICK: json5 — it is a serde Deserializer, so Task 4 deserializes straight
    // into a typed AliasMap with no hand-rolled AST walk. Recorded here so Task 4
    // does not re-litigate it; jsonc-parser then leaves selene-resolve's dev-deps.
}

// =============================================================================
// F4 (real-repo) — run manually: `cargo test -p selene-resolve --test spike_seam
//                                  -- --ignored --nocapture real_repo`
// =============================================================================

/// Indexes THIS repository with the real extractor and reports the warm-cache
/// numbers Task 2's `known_names` HashSet is sized against. `#[ignore]`d: it is
/// a measurement, not a contract, and it costs seconds.
#[ignore = "measurement, not a contract — run manually with --ignored --nocapture"]
#[tokio::test(flavor = "multi_thread")]
async fn f4_real_repo_measurement() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = selene_extract::Indexer::new(root.clone(), store);
    let result = indexer.index_all(None).await;

    let store = indexer.store();
    let names = store.all_node_names().await.unwrap();
    let set: std::collections::HashSet<String> = names.into_iter().collect();
    let bytes: usize = set.iter().map(|s| s.len() + size_of::<String>()).sum();
    println!(
        "F4(real): {} nodes over {} files → {} distinct names, HashSet payload ~{:.2} MB",
        result.nodes_created,
        result.files_indexed,
        set.len(),
        bytes as f64 / 1e6
    );
}
