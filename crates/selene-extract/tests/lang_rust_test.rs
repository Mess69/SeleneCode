//! Ported Rust conformance tests: the Rust describe block of
//! `extraction.test.ts` (functions/structs/traits/impl-for/supertraits/plain
//! impl) + the Rust imports block, plus the Task 9 brief's pins (receiver
//! methods, `-> Self` marker, `pub(crate)` quirk, use-binding refs) and one
//! insta snapshot. The formerly-`#[ignore]`d call-shape tests are live
//! (the Task 6 body walker merged in via 7ba47e3).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{EdgeKind, NodeKind, Visibility};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Rust)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

// =============================================================================
// extraction.test.ts — describe('Rust Extraction')
// =============================================================================

#[test]
fn extracts_function_declarations() {
    let code = "\npub fn process_data(input: &str) -> Result<Output, Error> {\n    // Process data\n    Ok(Output::new())\n}\n";
    let r = extract("lib.rs", code);

    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let f = find(&r, NodeKind::Function, "process_data").unwrap();
    assert_eq!(f.visibility, Some(Visibility::Public));
    assert_eq!(
        f.signature.as_deref(),
        Some("(input: &str) -> Result<Output, Error>")
    );
    // `Result<Output, Error>` → generics stripped → `Result`.
    assert_eq!(f.return_type.as_deref(), Some("Result"));
}

#[test]
fn extracts_struct_declarations() {
    let code =
        "\npub struct User {\n    pub id: String,\n    pub name: String,\n    email: String,\n}\n";
    let r = extract("models.rs", code);
    assert!(find(&r, NodeKind::Struct, "User").is_some());
}

#[test]
fn extracts_trait_declarations_with_bodiless_methods() {
    let code = "\npub trait Repository {\n    fn find(&self, id: &str) -> Option<Entity>;\n    fn save(&mut self, entity: Entity) -> Result<(), Error>;\n}\n";
    let r = extract("traits.rs", code);

    let t = find(&r, NodeKind::Trait, "Repository").unwrap();
    // function_signature_item (bodiless trait method) is first-class.
    let f = find(&r, NodeKind::Method, "find").unwrap();
    assert_eq!(f.qualified_name, "Repository::find");
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == t.id && e.target == f.id)
    );
    assert!(find(&r, NodeKind::Method, "save").is_some());
}

#[test]
fn impl_trait_for_type_emits_implements_ref() {
    let code = "\npub struct MyCache {}\n\npub trait Cache {\n    fn get(&self, key: &str) -> Option<String>;\n}\n\nimpl Cache for MyCache {\n    fn get(&self, key: &str) -> Option<String> {\n        None\n    }\n}\n";
    let r = extract("cache.rs", code);

    let my_cache = find(&r, NodeKind::Struct, "MyCache").unwrap();
    let impl_ref = r
        .unresolved
        .iter()
        .find(|u| u.reference_kind == "implements" && u.reference_name == "Cache")
        .expect("implements ref");
    assert_eq!(impl_ref.from_node_id, my_cache.id);
}

#[test]
fn trait_supertraits_emit_extends_refs() {
    let code = "\npub trait Display {}\n\npub trait Error: Display {\n    fn description(&self) -> &str;\n}\n";
    let r = extract("error.rs", code);

    let error_trait = find(&r, NodeKind::Trait, "Error").unwrap();
    let extends_ref = r
        .unresolved
        .iter()
        .find(|u| u.reference_kind == "extends" && u.reference_name == "Display")
        .expect("extends ref");
    assert_eq!(extends_ref.from_node_id, error_trait.id);
}

#[test]
fn plain_impl_blocks_emit_no_implements_refs() {
    let code = "\npub struct Counter {\n    count: u32,\n}\n\nimpl Counter {\n    pub fn new() -> Counter {\n        Counter { count: 0 }\n    }\n    pub fn increment(&mut self) {\n        self.count += 1;\n    }\n}\n";
    let r = extract("counter.rs", code);
    assert!(
        !r.unresolved
            .iter()
            .any(|u| u.reference_kind == "implements"),
        "no trait involved — no implements refs"
    );
}

// =============================================================================
// Receiver methods (impl blocks) — Task 9 brief pins
// =============================================================================

#[test]
fn impl_methods_get_receiver_qualified_names_and_owner_edge() {
    let code = "\nstruct S {\n    f: u32,\n}\n\nimpl S {\n    pub fn new() -> Self {\n        S { f: 0 }\n    }\n    pub async fn go(&self) -> Widget {\n        Widget\n    }\n}\n";
    let r = extract("s.rs", code);

    let new = find(&r, NodeKind::Method, "new").unwrap();
    assert_eq!(new.qualified_name, "S::new");
    assert_eq!(new.visibility, Some(Visibility::Public));
    // `-> Self` ⇒ the marker string "self".
    assert_eq!(new.return_type.as_deref(), Some("self"));

    let go = find(&r, NodeKind::Method, "go").unwrap();
    assert_eq!(go.is_async, Some(true));
    assert_eq!(go.return_type.as_deref(), Some("Widget"));

    // Owner contains edge from the same-file struct found by name.
    let s_struct = find(&r, NodeKind::Struct, "S").unwrap();
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == s_struct.id && e.target == new.id)
    );
}

#[test]
fn generic_impl_receiver_unwraps_to_inner_type() {
    let code = "\nstruct Wrap<T> {\n    v: T,\n}\n\nimpl<T> Wrap<T> {\n    fn get(&self) -> &T {\n        &self.v\n    }\n}\n";
    let r = extract("wrap.rs", code);
    let get = find(&r, NodeKind::Method, "get").unwrap();
    assert_eq!(get.qualified_name, "Wrap::get");
}

#[test]
fn pub_crate_reports_public_intended_quirk() {
    // extraction-langs.md §port notes: getVisibility checks `includes('pub')`
    // so `pub(crate)` reports PUBLIC — intended TS quirk, kept.
    let code = "\npub(crate) fn internal() {}\nfn private_fn() {}\n";
    let r = extract("vis.rs", code);
    assert_eq!(
        find(&r, NodeKind::Function, "internal").unwrap().visibility,
        Some(Visibility::Public)
    );
    assert_eq!(
        find(&r, NodeKind::Function, "private_fn")
            .unwrap()
            .visibility,
        Some(Visibility::Private)
    );
}

// =============================================================================
// Enums + variables
// =============================================================================

#[test]
fn enums_and_variants() {
    let code = "\npub enum Level {\n    Low,\n    High(u8),\n}\n";
    let r = extract("level.rs", code);
    let e = find(&r, NodeKind::Enum, "Level").unwrap();
    let low = find(&r, NodeKind::EnumMember, "Low").unwrap();
    assert!(find(&r, NodeKind::EnumMember, "High").is_some());
    assert!(
        r.edges
            .iter()
            .any(|x| x.kind == EdgeKind::Contains && x.source == e.id && x.target == low.id)
    );
}

#[test]
fn const_and_static_items_are_variables_ts_quirk() {
    // The TS config set no isConst hook, so const/static symbols land as
    // kind `variable` — kept as-is (exact-value port).
    let code = "\nconst MAX: u32 = 3;\nstatic COUNT: i64 = 0;\n";
    let r = extract("consts.rs", code);
    assert!(find(&r, NodeKind::Variable, "MAX").is_some());
    assert!(find(&r, NodeKind::Variable, "COUNT").is_some());
    assert!(find(&r, NodeKind::Constant, "MAX").is_none());
}

// =============================================================================
// extraction.test.ts — describe('Rust imports')
// =============================================================================

#[test]
fn simple_use_declaration() {
    let r = extract("main.rs", "use std::io;");
    let imp = find(&r, NodeKind::Import, "std").unwrap();
    assert_eq!(imp.signature.as_deref(), Some("use std::io;"));
}

#[test]
fn scoped_use_list() {
    let r = extract("main.rs", "use std::{ffi::OsStr, io, path::Path};");
    let imp = find(&r, NodeKind::Import, "std").unwrap();
    let sig = imp.signature.as_deref().unwrap();
    assert!(sig.contains("ffi::OsStr") && sig.contains("path::Path"));
}

#[test]
fn crate_imports() {
    let r = extract("lib.rs", "use crate::error::Error;");
    assert!(find(&r, NodeKind::Import, "crate").is_some());
}

#[test]
fn super_imports() {
    let r = extract("submod.rs", "use super::utils;");
    assert!(find(&r, NodeKind::Import, "super").is_some());
}

#[test]
fn external_crate_imports() {
    let r = extract("types.rs", "use serde::{Serialize, Deserialize};");
    let imp = find(&r, NodeKind::Import, "serde").unwrap();
    let sig = imp.signature.as_deref().unwrap();
    assert!(sig.contains("Serialize") && sig.contains("Deserialize"));
}

#[test]
fn use_binding_refs_link_each_imported_leaf() {
    // emitRustUseBindingRefs: every bound path gets an imports ref; an alias
    // links its SOURCE path; `self`/`super`/`crate`/`*` leaves are skipped.
    let r = extract(
        "m.rs",
        "use crate::m::{Widget, gadget as G};\nuse super::helpers::*;\n",
    );
    let imports: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "imports")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(imports.contains(&"crate::m::Widget"), "{imports:?}");
    assert!(imports.contains(&"crate::m::gadget"), "{imports:?}");
    // The module ref for the use statement itself.
    assert!(imports.contains(&"crate"), "{imports:?}");
    // Wildcard use contributes no binding ref (and no import node — TS shape).
    assert!(!imports.iter().any(|i| i.ends_with("::*")));
    assert!(find(&r, NodeKind::Import, "super").is_none());
}

// =============================================================================
// Deferred call-shape tests (Task 6 body walker) — from the Task 9 brief.
// =============================================================================

#[test]
fn scoped_identifier_calls_keep_full_path() {
    let code = "\nfn caller() {\n    utils::parse::run();\n    helper();\n}\n";
    let r = extract("calls.rs", code);
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls")
        .map(|u| u.reference_name.as_str())
        .collect();
    // Rust scoped_identifier calls keep the full `Module::function` path.
    assert!(calls.contains(&"utils::parse::run"), "{calls:?}");
    assert!(calls.contains(&"helper"), "{calls:?}");
}

#[test]
fn chained_factory_reencodes_only_scoped_identifier_inner() {
    let code = "\nfn caller() {\n    Widget::new().render();\n    make().render();\n}\n";
    let r = extract("chain.rs", code);
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls")
        .map(|u| u.reference_name.as_str())
        .collect();
    // Chained factory `inner().method` re-encoding ONLY when inner is a
    // scoped_identifier (`Widget::new`), not a bare identifier (`make`).
    assert!(calls.iter().any(|c| c.contains("Widget::new")), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.contains("make().render")),
        "{calls:?}"
    );
}

// =============================================================================
// Snapshot: full ExtractionResult for a representative Rust fixture.
// =============================================================================

#[test]
fn representative_fixture_snapshot() {
    let code = "use std::collections::HashMap;\n\n/// Renderable things.\npub trait Render: Sized {\n    fn render(&self) -> String;\n}\n\n/// A widget.\npub struct Widget {\n    pub id: u32,\n}\n\nimpl Render for Widget {\n    fn render(&self) -> String {\n        String::new()\n    }\n}\n\nimpl Widget {\n    pub fn new() -> Self {\n        Widget { id: 0 }\n    }\n}\n\nconst LIMIT: usize = 10;\n";
    let r = extract("widget.rs", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}

/// Inheritance-gap closure, Rust arm — a NEGATIVE test as much as a positive one.
///
/// Rust's supertrait (`trait_bounds`) and `impl Trait for Type` refs are owned by
/// `rules/rust_lang.rs`, NOT by the walker's shared `extract_inheritance` pass —
/// which deliberately does not handle `trait_bounds` (tree-sitter.ts:5380). If it
/// ever did, these refs would DOUBLE. The exact-equality assertions below are what
/// pin that: a duplicate would fail them.
#[test]
fn rust_supertrait_and_impl_refs_are_emitted_exactly_once() {
    let code = "pub trait Display {}\n\npub trait Error: Display {\n    fn description(&self) -> &str;\n}\n\npub struct MyError {\n    code: u32,\n}\n\nimpl Error for MyError {\n    fn description(&self) -> &str {\n        \"e\"\n    }\n}\n";
    let r = extract("inherit.rs", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    let extends: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == EdgeKind::Extends.as_str())
        .map(|u| u.reference_name.as_str())
        .collect();
    let implements: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == EdgeKind::Implements.as_str())
        .map(|u| u.reference_name.as_str())
        .collect();

    // Exactly one each — NOT two. The supertrait bound, and the impl-for.
    assert_eq!(
        extends,
        vec!["Display"],
        "supertrait ref must not duplicate"
    );
    assert_eq!(implements, vec!["Error"], "impl-for ref must not duplicate");
}
