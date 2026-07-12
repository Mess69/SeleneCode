#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Ported Ruby conformance tests: mixins → `implements` refs (the
//! Ruby-mixins describe block), require/require_relative imports, method
//! visibility, and one insta snapshot.
//!
//! DEFERRED (documented, follow-up once the core chain's `Session::visit`
//! lands on this branch): the `module` visit_node hook needs dispatch-ladder
//! re-entry for the module body, and the class-scope `CONST=` gate is a
//! walker-ladder branch — both live behind core-owned walker/mod.rs. The
//! Ruby-modules describe block ports with that follow-up. The
//! `extract_bare_call` spec is fully implemented + unit-tested colocated in
//! `src/rules/ruby.rs` (its walker consumer is the Task 6 body walker).

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Ruby)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

#[test]
fn extracts_classes_and_methods() {
    let code = "\nclass OrdersController\n  def index\n    render\n  end\n\n  def show\n    render\n  end\nend\n";
    let r = extract("orders_controller.rb", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    assert!(find(&r, NodeKind::Class, "OrdersController").is_some());
    let index = find(&r, NodeKind::Method, "index").unwrap();
    assert_eq!(index.qualified_name, "OrdersController::index");
    assert!(find(&r, NodeKind::Method, "show").is_some());
}

/// The Ruby-mixins block (#include/extend/prepend → `implements` from the
/// enclosing scope; constant/scope_resolution args only).
#[test]
fn mixins_emit_implements_refs() {
    let code = "\nclass Topic\n  include Searchable\n  extend ClassMethods\n  prepend Auditing, Extra::Hooks\nend\n";
    let r = extract("topic.rb", code);
    let class_id = &find(&r, NodeKind::Class, "Topic").unwrap().id;

    let implements: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "implements" && &u.from_node_id == class_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    for m in ["Searchable", "ClassMethods", "Auditing", "Extra::Hooks"] {
        assert!(implements.contains(&m), "missing {m} in {implements:?}");
    }
    // No spurious call-to-"include" import/keyword artifacts.
    assert!(find(&r, NodeKind::Import, "include").is_none());
}

#[test]
fn mixin_dynamic_and_self_args_are_skipped() {
    let code = "\nmodule M\nend\nclass K\n  extend self\n  include compute_mixin()\nend\n";
    let r = extract("k.rb", code);
    let implements: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "implements")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(implements.is_empty(), "{implements:?}");
}

#[test]
fn extracts_require_imports() {
    let code = "\nrequire 'json'\nrequire_relative 'lib/helper'\nputs 'hi'\n";
    let r = extract("main.rb", code);
    let imports: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect();
    assert!(imports.contains(&"json"), "{imports:?}");
    assert!(imports.contains(&"lib/helper"), "{imports:?}");
    // A non-require call is not an import.
    assert!(!imports.contains(&"puts"), "{imports:?}");
}

#[test]
fn visibility_follows_preceding_modifier_calls() {
    let code = "\nclass Svc\n  def open_api\n  end\n\n  private\n\n  def hidden\n  end\nend\n";
    let r = extract("svc.rb", code);
    assert_eq!(
        find(&r, NodeKind::Method, "open_api").unwrap().visibility,
        Some(selene_core::Visibility::Public)
    );
    assert_eq!(
        find(&r, NodeKind::Method, "hidden").unwrap().visibility,
        Some(selene_core::Visibility::Private)
    );
}

#[test]
fn representative_fixture_snapshot() {
    let code = "\nrequire 'active_support'\n\nclass Cart\n  include Discountable\n\n  def add(item)\n    item\n  end\nend\n";
    let mut r = extract("cart.rb", code);
    for n in &mut r.nodes {
        n.updated_at = 0;
    }
    r.duration_ms = 0;
    insta::assert_yaml_snapshot!("ruby_representative_fixture", r);
}
