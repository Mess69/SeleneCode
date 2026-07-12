#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Ported PHP conformance tests: the PHP describe blocks of
//! `extraction.test.ts` (classes, imports incl. include/require #660,
//! return-type capture #608), class constants + trait `use` → `implements`
//! refs, file-level namespace scoping, and one insta snapshot.

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Php)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

fn import_names(r: &ExtractionResult) -> Vec<&str> {
    r.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn extracts_class_declarations_with_default_public_visibility() {
    let code = "<?php\n\nclass UserController\n{\n    private UserService $userService;\n\n    public function __construct(UserService $userService)\n    {\n        $this->userService = $userService;\n    }\n\n    public function show(string $id): User\n    {\n        return $this->userService->find($id);\n    }\n\n    function helper() { return 1; }\n}\n";
    let r = extract("UserController.php", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    assert!(find(&r, NodeKind::Class, "UserController").is_some());
    let show = find(&r, NodeKind::Method, "show").unwrap();
    assert_eq!(show.qualified_name, "UserController::show");
    // PHP defaults to public when no visibility modifier is present.
    let helper = find(&r, NodeKind::Method, "helper").unwrap();
    assert_eq!(helper.visibility, Some(selene_core::Visibility::Public));
}

#[test]
fn traits_classify_as_trait() {
    let code = "<?php\ntrait Loggable\n{\n    public function log(string $m): void { }\n}\n";
    let r = extract("Loggable.php", code);
    assert!(find(&r, NodeKind::Trait, "Loggable").is_some());
    assert_eq!(
        find(&r, NodeKind::Method, "log").unwrap().qualified_name,
        "Loggable::log"
    );
}

// =============================================================================
// Imports (the PHP-imports describe block + #660)
// =============================================================================

#[test]
fn extracts_simple_aliased_and_function_use() {
    let r = extract("Test.php", "<?php use PHPUnit\\Framework\\TestCase;");
    assert_eq!(import_names(&r), vec!["PHPUnit\\Framework\\TestCase"]);

    let r = extract("Test.php", "<?php use Mockery as m;");
    let import = find(&r, NodeKind::Import, "Mockery").unwrap();
    assert!(import.signature.as_deref().unwrap_or("").contains("as m"));

    let r = extract(
        "helpers.php",
        "<?php use function Illuminate\\Support\\env;",
    );
    let import = find(&r, NodeKind::Import, "Illuminate\\Support\\env").unwrap();
    assert!(
        import
            .signature
            .as_deref()
            .unwrap_or("")
            .contains("function")
    );
}

#[test]
fn extracts_grouped_and_multiple_use() {
    let r = extract(
        "Models.php",
        "<?php use Illuminate\\Database\\{Model, Builder};",
    );
    let names = import_names(&r);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"Illuminate\\Database\\Model"));
    assert!(names.contains(&"Illuminate\\Database\\Builder"));

    let r = extract(
        "Service.php",
        "<?php\nuse Illuminate\\Support\\Collection;\nuse Illuminate\\Support\\Str;\nuse Closure;\n",
    );
    let names = import_names(&r);
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"Closure"));
}

/// #660: include/require(+_once) static string-literal paths become imports.
#[test]
fn extracts_include_require_static_paths() {
    let code = "<?php\nrequire_once(\"lib.php\");\ninclude 'other.php';\nrequire 'r.php';\ninclude_once(\"io.php\");\n";
    let r = extract("page.php", code);
    let names = import_names(&r);
    for p in ["lib.php", "other.php", "r.php", "io.php"] {
        assert!(names.contains(&p), "missing {p} in {names:?}");
    }
}

/// #660: dynamic paths have no resolvable compile-time value — silently
/// skipped ("silent beats wrong").
#[test]
fn skips_dynamic_include_require() {
    let code = "<?php\nrequire_once(__DIR__ . '/dyn.php');\ninclude $file;\ninclude \"tpl/{$name}.php\";\n";
    let r = extract("page.php", code);
    assert!(import_names(&r).is_empty(), "{:?}", import_names(&r));
}

#[test]
fn include_coexists_with_namespace_use() {
    let code = "<?php\nuse App\\Service\\Mailer;\nrequire_once(\"bootstrap.php\");\n";
    let r = extract("page.php", code);
    let names = import_names(&r);
    assert!(names.contains(&"App\\Service\\Mailer"));
    assert!(names.contains(&"bootstrap.php"));
}

// =============================================================================
// Class constants + trait use (visit_node hook)
// =============================================================================

#[test]
fn class_constants_become_constant_nodes() {
    let code = "<?php\nclass Order\n{\n    const STATUS_OPEN = 'open';\n    const STATUS_DONE = 'done';\n}\n";
    let r = extract("Order.php", code);
    let open = find(&r, NodeKind::Constant, "STATUS_OPEN").unwrap();
    assert_eq!(open.qualified_name, "Order::STATUS_OPEN");
    assert!(find(&r, NodeKind::Constant, "STATUS_DONE").is_some());
}

#[test]
fn trait_use_emits_implements_refs() {
    let code = "<?php\nclass Report\n{\n    use Loggable, Cacheable;\n    use App\\Concerns\\Sortable;\n}\n";
    let r = extract("Report.php", code);
    let class_id = &find(&r, NodeKind::Class, "Report").unwrap().id;

    let implements: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "implements" && &u.from_node_id == class_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(implements.contains(&"Loggable"), "{implements:?}");
    assert!(implements.contains(&"Cacheable"), "{implements:?}");
    assert!(
        implements.contains(&"App\\Concerns\\Sortable"),
        "{implements:?}"
    );
}

// =============================================================================
// Return-type capture (#608) + namespace (file-level, unbraced only)
// =============================================================================

#[test]
fn return_types_normalize_to_chainable_class_names() {
    let code = "<?php\nclass Factory\n{\n    public function make(): Widget { return new Widget(); }\n    public function self_ret(): static { return $this; }\n    public function nullable(): ?Widget { return null; }\n    public function qualified(): \\App\\Widget { return new Widget(); }\n    public function scalar(): int { return 1; }\n}\n";
    let r = extract("Factory.php", code);
    let ret = |name: &str| {
        find(&r, NodeKind::Method, name)
            .unwrap()
            .return_type
            .clone()
    };
    assert_eq!(ret("make").as_deref(), Some("Widget"));
    // `self|static|$this` → the marker 'self', resolved at resolution time.
    assert_eq!(ret("self_ret").as_deref(), Some("self"));
    assert_eq!(ret("nullable").as_deref(), Some("Widget"));
    assert_eq!(ret("qualified").as_deref(), Some("Widget"));
    // The lowercase non-class return set yields nothing to chain on.
    assert_eq!(ret("scalar"), None);
}

#[test]
fn file_level_namespace_scopes_qualified_names_unbraced_only() {
    let code =
        "<?php\nnamespace App\\Core;\n\nclass Widget { public function render(): void {} }\n";
    let r = extract("Widget.php", code);
    assert!(find(&r, NodeKind::Namespace, "App\\Core").is_some());
    assert_eq!(
        find(&r, NodeKind::Class, "Widget").unwrap().qualified_name,
        "App\\Core::Widget"
    );

    // Braced `namespace Foo { … }` is NOT the file-level form — skipped.
    let braced = "<?php\nnamespace Legacy {\n    class Old {}\n}\n";
    let r2 = extract("Old.php", braced);
    assert!(find(&r2, NodeKind::Namespace, "Legacy").is_none());
}

#[test]
fn representative_fixture_snapshot() {
    let code = "<?php\nnamespace Shop;\n\nuse App\\Contracts\\Sellable;\n\nclass Cart\n{\n    use Discountable;\n    const LIMIT = 10;\n    public function add(Item $item): self { return $this; }\n}\n";
    let mut r = extract("Cart.php", code);
    for n in &mut r.nodes {
        n.updated_at = 0;
    }
    r.duration_ms = 0;
    insta::assert_yaml_snapshot!("php_representative_fixture", r);
}

// =============================================================================
// Object+name callee branch (post-merge fix): parent/static skip
// =============================================================================

#[test]
fn parent_and_static_receivers_emit_bare_method_names() {
    // `parent::boot()` / `static::create()` must NOT emit `parent.boot` /
    // `static.create` qualified refs (unresolvable); the bare name lets
    // same-file/hierarchy resolution work. `self::` was already skipped.
    let code = "<?php\nclass Child extends Base {\n    public function boot(): void {\n        parent::boot();\n        static::create();\n        self::helper();\n        Other::make();\n    }\n}\n";
    let r = extract("Child.php", code);
    let boot_id = &find(&r, NodeKind::Method, "boot").unwrap().id;
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls" && &u.from_node_id == boot_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(calls.contains(&"boot"), "parent:: → bare: {calls:?}");
    assert!(calls.contains(&"create"), "static:: → bare: {calls:?}");
    assert!(calls.contains(&"helper"), "self:: → bare: {calls:?}");
    assert!(
        calls.contains(&"Other.make"),
        "real class receivers stay qualified: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("parent.") || c.starts_with("static.")),
        "no parent./static. qualified refs: {calls:?}"
    );
}

#[test]
fn php_static_factory_fluent_chain_reencodes() {
    // `Cls::for($x)->method()` — encode `Cls::for().method` so the PHP
    // resolver splits on the `().` marker and infers the class from the
    // factory's declared return (#608).
    let code = "<?php\nclass Runner {\n    public function go(): void {\n        Builder::make($x)->run();\n    }\n}\n";
    let r = extract("Runner.php", code);
    let go_id = &find(&r, NodeKind::Method, "go").unwrap().id;
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls" && &u.from_node_id == go_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(
        calls.contains(&"Builder::make().run"),
        "php fluent chain re-encode: {calls:?}"
    );
}
