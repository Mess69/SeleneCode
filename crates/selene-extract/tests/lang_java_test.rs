//! Ported Java conformance tests: the Java describe block of
//! `extraction.test.ts` (classes, methods, packages, anonymous classes) +
//! the Java imports block, the extraction-level assertions of
//! `lombok.test.ts` (#912 — the call-resolution halves stay with the
//! resolver phase), the `static final` constant gate, and one insta
//! snapshot. The two anonymous-class tests are `#[ignore]`d — they reach
//! `new T() { … }` through method/lambda bodies, i.e. Task 6's body walker.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{EdgeKind, NodeKind, Visibility};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Java)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

fn is_lombok(n: &selene_core::Node) -> bool {
    n.decorators.iter().any(|d| d == "lombok")
}

// =============================================================================
// extraction.test.ts — describe('Java Extraction')
// =============================================================================

#[test]
fn extracts_class_declarations() {
    let code = "\npublic class UserService {\n    private final UserRepository repository;\n\n    public UserService(UserRepository repository) {\n        this.repository = repository;\n    }\n\n    public User getUser(String id) {\n        return repository.findById(id);\n    }\n}\n";
    let r = extract("UserService.java", code);

    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let class = find(&r, NodeKind::Class, "UserService").unwrap();
    assert_eq!(class.visibility, Some(Visibility::Public));

    // Constructor is a method named like the class; getUser has the
    // `ReturnType (params)` signature and normalized returnType.
    let get_user = find(&r, NodeKind::Method, "getUser").unwrap();
    assert_eq!(get_user.qualified_name, "UserService::getUser");
    assert_eq!(get_user.signature.as_deref(), Some("User (String id)"));
    assert_eq!(get_user.return_type.as_deref(), Some("User"));
    // The instance field (final only, not static) stays a field.
    let repo = find(&r, NodeKind::Field, "repository").unwrap();
    assert_eq!(repo.signature.as_deref(), Some("UserRepository repository"));
    assert_eq!(repo.visibility, Some(Visibility::Private));
}

#[test]
fn extracts_method_declarations_with_static() {
    let code = "\npublic class Calculator {\n    public static int add(int a, int b) {\n        return a + b;\n    }\n}\n";
    let r = extract("Calculator.java", code);
    let add = find(&r, NodeKind::Method, "add").unwrap();
    assert_eq!(add.is_static, Some(true));
    // int (integral_type) is a non-class return → no returnType.
    assert_eq!(add.return_type, None);
}

#[test]
fn wraps_top_level_declarations_in_package_namespace() {
    let code = "\npackage com.example.foo;\n\npublic class Bar {\n    public String greet() { return \"hi\"; }\n}\n";
    let r = extract("Bar.java", code);

    let ns = find(&r, NodeKind::Namespace, "com.example.foo").unwrap();
    let cls = find(&r, NodeKind::Class, "Bar").unwrap();
    assert_eq!(cls.qualified_name, "com.example.foo::Bar");
    let greet = find(&r, NodeKind::Method, "greet").unwrap();
    assert_eq!(greet.qualified_name, "com.example.foo::Bar::greet");
    // Namespace contains the class.
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == ns.id && e.target == cls.id)
    );
}

#[test]
fn does_not_wrap_without_package_declaration() {
    let code = "\npublic class Bar {\n    public String greet() { return \"hi\"; }\n}\n";
    let r = extract("Bar.java", code);
    assert!(r.nodes.iter().all(|n| n.kind != NodeKind::Namespace));
    assert_eq!(
        find(&r, NodeKind::Class, "Bar").unwrap().qualified_name,
        "Bar"
    );
}

#[test]
fn annotation_type_declaration_is_an_interface() {
    let code = "\npublic @interface Marker {}\n";
    let r = extract("Marker.java", code);
    assert!(find(&r, NodeKind::Interface, "Marker").is_some());
}

#[test]
fn enums_and_constants() {
    let code = "\npublic class Config {\n    private static final int MAX_ITEMS = 10;\n    private final int perInstance = 1;\n}\n\nenum Level { LOW, HIGH }\n";
    let r = extract("Config.java", code);

    // static final → constant kind (value-reference target); final-only stays field.
    let max = find(&r, NodeKind::Constant, "MAX_ITEMS").unwrap();
    assert_eq!(max.signature.as_deref(), Some("int MAX_ITEMS"));
    assert_eq!(max.is_static, Some(true));
    assert!(find(&r, NodeKind::Field, "perInstance").is_some());

    let level = find(&r, NodeKind::Enum, "Level").unwrap();
    let low = find(&r, NodeKind::EnumMember, "LOW").unwrap();
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == level.id && e.target == low.id)
    );
    assert!(find(&r, NodeKind::EnumMember, "HIGH").is_some());
}

#[test]
#[ignore = "anonymous classes (`new T() { … }`) are reached through method \
            bodies — Task 6's body walker, a no-op at this branch's base \
            5fb90cd; un-ignore after the core chain merges"]
fn extracts_anonymous_class_overrides() {
    let code = "\npackage com.example;\n\nabstract class Base {\n  abstract int compute(int x);\n}\n\npublic class Factory {\n  public Base make() {\n    return new Base() {\n      @Override\n      int compute(int x) { return x + 1; }\n    };\n  }\n}\n";
    let r = extract("Factory.java", code);

    let anon = r
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name.contains("Base$anon@"))
        .expect("anonymous Base subclass extracted as a class");
    let compute = r
        .nodes
        .iter()
        .find(|n| {
            n.kind == NodeKind::Method && n.name == "compute" && n.qualified_name.contains("$anon@")
        })
        .expect("override method on the anon class");
    assert!(
        compute
            .qualified_name
            .contains("Factory::make::<Base$anon@")
    );
    assert!(compute.qualified_name.ends_with("::compute"));
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "extends"
        && u.reference_name == "Base"
        && u.from_node_id == anon.id));
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "instantiates" && u.reference_name == "Base")
    );
}

#[test]
#[ignore = "anonymous classes inside lambda bodies are reached through Task \
            6's body walker, a no-op at this branch's base 5fb90cd; un-ignore \
            after the core chain merges"]
fn extracts_anonymous_class_inside_lambda_body() {
    let code = "\npackage com.example;\n\ninterface Strategy {\n  java.util.Iterator<String> iterator(String s);\n}\n\nabstract class BaseIter implements java.util.Iterator<String> {\n  abstract int separatorStart(int start);\n}\n\npublic class Splitter {\n  private final Strategy strategy;\n  public Splitter(Strategy s) { this.strategy = s; }\n\n  public static Splitter on(char c) {\n    return new Splitter((seq) ->\n        new BaseIter() {\n          @Override\n          int separatorStart(int start) { return start + 1; }\n          @Override public boolean hasNext() { return false; }\n          @Override public String next() { return null; }\n        });\n  }\n}\n";
    let r = extract("Splitter.java", code);
    assert!(
        r.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name.contains("BaseIter$anon@"))
    );
    assert!(r.nodes.iter().any(|n| {
        n.kind == NodeKind::Method
            && n.name == "separatorStart"
            && n.qualified_name.contains("$anon@")
    }));
}

// =============================================================================
// extraction.test.ts — describe('Java imports')
// =============================================================================

#[test]
fn simple_import() {
    let r = extract("Main.java", "import java.util.List;");
    let imp = find(&r, NodeKind::Import, "java.util.List").unwrap();
    assert_eq!(imp.signature.as_deref(), Some("import java.util.List;"));
}

#[test]
fn static_import() {
    let r = extract(
        "Utils.java",
        "import static java.util.Collections.emptyList;",
    );
    let imp = find(&r, NodeKind::Import, "java.util.Collections.emptyList").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains("static"));
}

#[test]
fn wildcard_import() {
    let r = extract("App.java", "import java.util.*;");
    let imp = find(&r, NodeKind::Import, "java.util").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains(".*"));
}

#[test]
fn nested_class_import() {
    let r = extract("MapUtil.java", "import java.util.Map.Entry;");
    assert!(find(&r, NodeKind::Import, "java.util.Map.Entry").is_some());
}

#[test]
fn multiple_imports() {
    let r = extract(
        "Multi.java",
        "\nimport java.util.List;\nimport java.util.Map;\n",
    );
    let names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"java.util.List") && names.contains(&"java.util.Map"));
}

// =============================================================================
// lombok.test.ts — extraction-level assertions (#912)
// =============================================================================

#[test]
fn synthesizes_accessors_builder_data_contract_and_log_field() {
    let code = "package model;\nimport lombok.Data;\nimport lombok.Builder;\nimport lombok.extern.slf4j.Slf4j;\n\n@Data\n@Builder\n@Slf4j\npublic class User {\n    private String name;\n    private boolean active;\n    private static final int MAX = 10;\n}\n";
    let r = extract("User.java", code);

    let lombok = |name: &str| {
        r.nodes
            .iter()
            .find(|n| n.name == name && is_lombok(n))
            .unwrap_or_else(|| panic!("expected synthesized {name}"))
    };

    // Accessors + Data contract + builder are synthesized and marked.
    for m in [
        "getName",
        "setName",
        "isActive",
        "setActive",
        "builder",
        "equals",
        "hashCode",
        "toString",
    ] {
        assert_eq!(lombok(m).kind, NodeKind::Method);
        assert_eq!(lombok(m).visibility, Some(Visibility::Public));
    }
    let get_name = lombok("getName");
    assert!(
        get_name
            .docstring
            .as_deref()
            .unwrap()
            .contains("Lombok-generated")
    );
    assert_eq!(
        get_name.docstring.as_deref(),
        Some("Lombok-generated (@Data)")
    );
    assert_eq!(get_name.signature.as_deref(), Some("String getName()"));
    assert_eq!(get_name.qualified_name, "model::User::getName");
    // boolean → is-prefix getter.
    assert_eq!(
        lombok("isActive").signature.as_deref(),
        Some("boolean isActive()")
    );
    assert_eq!(
        lombok("setActive").signature.as_deref(),
        Some("void setActive(boolean active)")
    );
    // builder: static, returns <Class>Builder.
    let builder = lombok("builder");
    assert!(builder.signature.as_deref().unwrap().contains("static "));
    assert_eq!(builder.is_static, Some(true));
    assert_eq!(builder.return_type.as_deref(), Some("UserBuilder"));

    // @Slf4j → a `log` field, private static.
    let log = r
        .nodes
        .iter()
        .find(|n| n.name == "log" && n.kind == NodeKind::Field && is_lombok(n))
        .expect("log field");
    assert_eq!(log.signature.as_deref(), Some("Logger log"));
    assert_eq!(log.docstring.as_deref(), Some("Lombok-generated (@Slf4j)"));
    assert_eq!(log.is_static, Some(true));

    // PRECISION: a static field gets no accessor.
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.name == "getMAX" || n.name == "getMax")
    );
}

#[test]
fn never_overrides_a_hand_written_accessor() {
    let code = "package model;\nimport lombok.Getter;\n\n@Getter\npublic class Account {\n    private int balance;\n    private String owner;\n\n    // explicit getter — Lombok skips it, so must we\n    public int getBalance() { return balance < 0 ? 0 : balance; }\n}\n";
    let r = extract("Account.java", code);

    let get_balance: Vec<&selene_core::Node> =
        r.nodes.iter().filter(|n| n.name == "getBalance").collect();
    assert_eq!(get_balance.len(), 1, "exactly one, not duplicated");
    assert!(!is_lombok(get_balance[0]), "the hand-written one survives");
    // The un-shadowed field still gets its synthesized getter.
    assert!(r.nodes.iter().any(|n| n.name == "getOwner" && is_lombok(n)));
}

#[test]
fn field_level_annotations_and_final_field_rules() {
    let code = "package model;\nimport lombok.Getter;\nimport lombok.Setter;\n\npublic class Box {\n    @Getter @Setter private String label;\n    @Getter private final long id;     // final → getter only, no setter\n    private int hidden;                // no annotation → nothing\n}\n";
    let r = extract("Box.java", code);

    assert!(r.nodes.iter().any(|n| n.name == "getLabel" && is_lombok(n)));
    assert!(r.nodes.iter().any(|n| n.name == "setLabel" && is_lombok(n)));
    assert!(r.nodes.iter().any(|n| n.name == "getId" && is_lombok(n)));
    assert!(
        !r.nodes.iter().any(|n| n.name == "setId"),
        "final → no setter"
    );
    assert!(
        !r.nodes.iter().any(|n| n.name == "getHidden"),
        "un-annotated → nothing"
    );
    // Field-level attribution names the field's own annotation.
    assert_eq!(
        r.nodes
            .iter()
            .find(|n| n.name == "getLabel")
            .unwrap()
            .docstring
            .as_deref(),
        Some("Lombok-generated (@Getter)")
    );
}

#[test]
fn plain_java_class_synthesizes_nothing() {
    let code = "package model;\n\npublic class Plain {\n    private int value;\n    public int getValue() { return value; }\n    public void setValue(int v) { this.value = v; }\n}\n";
    let r = extract("Plain.java", code);
    assert!(
        !r.nodes.iter().any(is_lombok),
        "clean control: no synthesized members"
    );
}

// =============================================================================
// Snapshot: full ExtractionResult for a representative Java fixture.
// =============================================================================

#[test]
fn representative_fixture_snapshot() {
    let code = "package com.example.store;\n\nimport lombok.Data;\n\n// Aggregate root\n@Data\npublic class Order {\n    private String id;\n    private static final int VERSION = 1;\n\n    public String describe() {\n        return id;\n    }\n}\n\ninterface Repo {\n    Order load(String id);\n}\n";
    let r = extract("Order.java", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}
