#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Ported Ruby conformance tests: mixins → `implements` refs (the
//! Ruby-mixins describe block), the Ruby-modules describe block (module
//! nodes + containment, nested modules), class-scope `CONST=` variables,
//! require/require_relative imports, method visibility, and one insta
//! snapshot. The `extract_bare_call` spec is unit-tested colocated in
//! `src/rules/ruby.rs` (its consumer is the body walker).

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

// =============================================================================
// Ruby modules (the Ruby-modules describe block — landed with Session::visit)
// =============================================================================

#[test]
fn module_extracts_as_module_node_with_containment() {
    let code = "\nmodule CachedCounting\n  def self.disable\n    @enabled = false\n  end\n\n  def perform_increment!(key, count)\n    write_cache!(key, count)\n  end\nend\n";
    let r = extract("concerns/cached_counting.rb", code);

    let module = find(&r, NodeKind::Module, "CachedCounting").unwrap();
    assert_eq!(module.qualified_name, "CachedCounting");

    // Methods inside the module get module-qualified names.
    let disable = find(&r, NodeKind::Method, "disable").unwrap();
    assert_eq!(disable.qualified_name, "CachedCounting::disable");
    let increment = find(&r, NodeKind::Method, "perform_increment!").unwrap();
    assert_eq!(
        increment.qualified_name,
        "CachedCounting::perform_increment!"
    );

    // Containment edges from the module to its methods.
    let contains = r
        .edges
        .iter()
        .filter(|e| e.source == module.id && e.kind == selene_core::EdgeKind::Contains)
        .count();
    assert!(
        contains >= 2,
        "expected >= 2 contains edges, got {contains}"
    );
}

#[test]
fn nested_modules_with_classes_qualify_names() {
    let code = "\nmodule Discourse\n  module Auth\n    class AuthProvider\n      def authenticate(params)\n        validate(params)\n      end\n    end\n  end\nend\n";
    let r = extract("lib/auth.rb", code);

    assert!(find(&r, NodeKind::Module, "Discourse").is_some());
    let auth = find(&r, NodeKind::Module, "Auth").unwrap();
    assert_eq!(auth.qualified_name, "Discourse::Auth");
    let provider = find(&r, NodeKind::Class, "AuthProvider").unwrap();
    assert_eq!(provider.qualified_name, "Discourse::Auth::AuthProvider");
    let auth_method = find(&r, NodeKind::Method, "authenticate").unwrap();
    assert_eq!(
        auth_method.qualified_name,
        "Discourse::Auth::AuthProvider::authenticate"
    );
}

/// Class/module-scope `CONST = …` (a `constant`-typed LHS — effectively
/// Ruby-only) extracts like a top-level variable; TS parity: kind stays
/// `variable` (Ruby's config has no is_const hook) and the name is the
/// constant. Locals inside methods stay unextracted.
#[test]
fn class_scope_constant_assignment_is_extracted() {
    let code = "\nclass Config\n  MAX_ITEMS = 50\n  module Nested\n    TIMEOUT = 30\n  end\n  def read\n    local = 1\n    local\n  end\nend\n";
    let r = extract("config.rb", code);

    let max = find(&r, NodeKind::Variable, "MAX_ITEMS").unwrap();
    assert_eq!(max.qualified_name, "Config::MAX_ITEMS");
    let timeout = find(&r, NodeKind::Variable, "TIMEOUT").unwrap();
    assert_eq!(timeout.qualified_name, "Config::Nested::TIMEOUT");
    // Locals inside method bodies are NOT variables (top-level gate).
    assert!(find(&r, NodeKind::Variable, "local").is_none());
}
