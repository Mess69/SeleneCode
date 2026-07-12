//! Ported Go conformance tests: the Go describe block of
//! `extraction.test.ts` + the Go imports block, plus the Task 9 brief's
//! pins (receiver regex #583 generics, uppercase-export rule, `type_spec`
//! reclassification, return-type normalization, interface method specs) and
//! one insta snapshot. The formerly-`#[ignore]`d call-shape tests are live
//! (the Task 6 body walker merged in via 7ba47e3).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{EdgeKind, NodeKind};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Go)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

// =============================================================================
// extraction.test.ts — describe('Go Extraction')
// =============================================================================

#[test]
fn extracts_function_declarations() {
    let code = "\npackage main\n\nfunc ProcessOrder(order Order) (Receipt, error) {\n    // Process the order\n    return Receipt{}, nil\n}\n";
    let r = extract("main.go", code);

    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let f = find(&r, NodeKind::Function, "ProcessOrder").unwrap();
    assert_eq!(f.language, "go");
    // Uppercase first letter ⇒ exported (the Go visibility rule).
    assert_eq!(f.is_exported, Some(true));
    // Multi-return `(Receipt, error)` → first result.
    assert_eq!(f.return_type.as_deref(), Some("Receipt"));
    assert_eq!(
        f.signature.as_deref(),
        Some("(order Order) (Receipt, error)")
    );
}

#[test]
fn extracts_method_declarations_with_receiver() {
    let code = "\npackage main\n\ntype Service struct {\n    db *Database\n}\n\nfunc (s *Service) GetUser(id string) (*User, error) {\n    return s.db.FindUser(id)\n}\n";
    let r = extract("service.go", code);

    let m = find(&r, NodeKind::Method, "GetUser").unwrap();
    // Receiver methods get the `Receiver::name` qualified-name override.
    assert_eq!(m.qualified_name, "Service::GetUser");

    // The core links a same-file contains edge from the struct found by name.
    let svc = find(&r, NodeKind::Struct, "Service").unwrap();
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == svc.id && e.target == m.id),
        "struct → method contains edge missing"
    );
}

#[test]
fn generic_receiver_methods_are_not_orphaned() {
    // #583: `(s *Stack[T])` — the receiver regex must skip the var name and
    // match through the generic suffix.
    let code = "\npackage main\n\ntype Stack[T any] struct{ items []T }\n\nfunc (s *Stack[T]) Push(v T) {}\nfunc (Stack[T]) Len() int { return 0 }\n";
    let r = extract("stack.go", code);

    let push = find(&r, NodeKind::Method, "Push").unwrap();
    assert_eq!(push.qualified_name, "Stack::Push");
    let len = find(&r, NodeKind::Method, "Len").unwrap();
    assert_eq!(len.qualified_name, "Stack::Len");
}

#[test]
fn lowercase_symbols_are_unexported() {
    let code = "\npackage main\n\nfunc helper() {}\nfunc Public() {}\n";
    let r = extract("vis.go", code);
    assert_eq!(
        find(&r, NodeKind::Function, "helper").unwrap().is_exported,
        Some(false)
    );
    assert_eq!(
        find(&r, NodeKind::Function, "Public").unwrap().is_exported,
        Some(true)
    );
}

// =============================================================================
// type_spec reclassification (struct / interface / plain alias)
// =============================================================================

#[test]
fn type_spec_reclassifies_struct_interface_and_alias() {
    let code = "\npackage main\n\ntype Point struct{ X int }\n\ntype Reader interface {\n\tRead(p []byte) (int, error)\n\tClose() error\n}\n\ntype Celsius float64\n";
    let r = extract("types.go", code);

    assert!(find(&r, NodeKind::Struct, "Point").is_some(), "struct");
    let iface = find(&r, NodeKind::Interface, "Reader").unwrap();
    assert_eq!(iface.is_exported, Some(true));
    assert!(
        find(&r, NodeKind::TypeAlias, "Celsius").is_some(),
        "plain type alias stays type_alias"
    );

    // Interface method specs become `method` nodes under the interface (the
    // implicit-satisfaction contract set — Go has no `implements` keyword).
    let read = find(&r, NodeKind::Method, "Read").unwrap();
    assert_eq!(read.qualified_name, "Reader::Read");
    assert_eq!(read.signature.as_deref(), Some("(p []byte) (int, error)"));
    let close = find(&r, NodeKind::Method, "Close").unwrap();
    assert_eq!(close.qualified_name, "Reader::Close");
    // Containment: interface → its method specs.
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == iface.id && e.target == read.id)
    );
}

// =============================================================================
// Return-type normalization
// =============================================================================

#[test]
fn return_type_normalization() {
    let code = "\npackage main\n\nfunc A() *Point { return nil }\nfunc B() (Point, error) { return Point{}, nil }\nfunc C() pkg.Foo { return pkg.Foo{} }\nfunc D() Foo[T] { return Foo[T]{} }\nfunc E() {}\n";
    let r = extract("rt.go", code);
    let rt = |name: &str| {
        find(&r, NodeKind::Function, name)
            .unwrap()
            .return_type
            .clone()
    };
    assert_eq!(rt("A").as_deref(), Some("Point"), "pointer unwrap");
    assert_eq!(rt("B").as_deref(), Some("Point"), "multi-return first");
    assert_eq!(rt("C").as_deref(), Some("Foo"), "qualified last segment");
    assert_eq!(rt("D").as_deref(), Some("Foo"), "generic args stripped");
    assert_eq!(rt("E"), None, "no result field");
}

// =============================================================================
// extraction.test.ts — describe('Go imports')
// =============================================================================

#[test]
fn single_import() {
    let r = extract("main.go", "\npackage main\n\nimport \"fmt\"\n");
    let imp = find(&r, NodeKind::Import, "fmt").unwrap();
    assert_eq!(imp.signature.as_deref(), Some("\"fmt\""));
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "imports" && u.reference_name == "fmt")
    );
}

#[test]
fn grouped_imports() {
    let r = extract(
        "main.go",
        "\npackage main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\t\"encoding/json\"\n)\n",
    );
    let names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names.len(), 3);
    for want in ["fmt", "os", "encoding/json"] {
        assert!(names.contains(&want), "missing {want}: {names:?}");
    }
}

#[test]
fn aliased_import() {
    let r = extract("main.go", "\npackage main\n\nimport f \"fmt\"\n");
    let imp = find(&r, NodeKind::Import, "fmt").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains('f'));
}

#[test]
fn dot_import() {
    let r = extract("main.go", "\npackage main\n\nimport . \"math\"\n");
    let imp = find(&r, NodeKind::Import, "math").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains('.'));
}

#[test]
fn blank_import() {
    let r = extract(
        "main.go",
        "\npackage main\n\nimport _ \"github.com/go-sql-driver/mysql\"\n",
    );
    let imp = find(&r, NodeKind::Import, "github.com/go-sql-driver/mysql").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains('_'));
}

// =============================================================================
// Top-level var/const declarations (the TS core Go branch)
// =============================================================================

#[test]
fn top_level_const_and_var_declarations() {
    let code = "\npackage main\n\n// registry doc\nconst MaxSize = 100\n\nvar Registry = map[string]int{}\n\nvar a, b = 1, 2\n";
    let r = extract("vars.go", code);

    let max = find(&r, NodeKind::Constant, "MaxSize").unwrap();
    assert_eq!(max.signature.as_deref(), Some("= 100"));
    let reg = find(&r, NodeKind::Variable, "Registry").unwrap();
    assert_eq!(reg.signature.as_deref(), Some("= map[string]int{}"));
    // Multi-identifier var_spec: first identifier only (TS shape).
    assert!(find(&r, NodeKind::Variable, "a").is_some());
    assert!(find(&r, NodeKind::Variable, "b").is_none());
}

// =============================================================================
// Deferred call-shape tests (Task 6 body walker) — from the Task 9 brief.
// =============================================================================

#[test]
fn chained_factory_call_only_for_bare_identifier_inner() {
    let code = "\npackage main\n\nfunc Use() {\n\tNewClient().Run()\n\tpkg.New().Run()\n}\n";
    let r = extract("chain.go", code);
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls")
        .map(|u| u.reference_name.as_str())
        .collect();
    // Chained factory re-encoding only when the inner callee is a bare
    // identifier: `NewClient().Run` kept; `pkg.New().Run` NOT re-encoded.
    assert!(calls.iter().any(|c| c.contains("NewClient")));
    assert!(!calls.iter().any(|c| c.contains("pkg.New().Run")));
}

#[test]
fn conversion_call_normalizes_pointer_conversion() {
    let code = "\npackage main\n\nfunc Cast(x interface{}) {\n\t_ = (*Config)(x)\n}\n";
    let r = extract("conv.go", code);
    // `(*T)(x)` conversion normalization via /^\(\s*\*?\s*([A-Za-z_][\w.]*)\s*\)$/.
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "calls" && u.reference_name == "Config")
    );
}

// =============================================================================
// Snapshot: full ExtractionResult for a representative Go fixture.
// =============================================================================

#[test]
fn representative_fixture_snapshot() {
    let code = "package registry\n\nimport (\n\t\"fmt\"\n\t\"encoding/json\"\n)\n\n// Item doc\ntype Item struct{ ID int }\n\ntype Store interface {\n\tGet(id int) (*Item, error)\n}\n\nconst DefaultCap = 8\n\nfunc (i *Item) Label() string { return fmt.Sprint(i.ID) }\n\nfunc New() *Item { return &Item{} }\n";
    let r = extract("registry.go", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}
