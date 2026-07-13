//! Ported C++ conformance tests: free functions + out-of-line qualified
//! methods (`Widget::size` receiver QNs), export-macro class recovery
//! (#1061 blanker + #946 phantom-function guard), namespace prefix QNs
//! (#387), forward-declaration skip (#1093), template-arg stripping in
//! calls, macro-defined kernel names, `using` aliases, and one snapshot.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{NodeKind, Visibility};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Cpp)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

#[test]
fn free_functions_and_out_of_line_methods() {
    let code = "namespace app {\nclass Widget {\n public:\n  int size() const;\n};\nint Widget::size() const { return helper(); }\nvoid boot() { run(); }\n}\n";
    let r = extract("widget.cpp", code);

    // Namespace prefixes QNs; no namespace node is minted.
    assert!(!r.nodes.iter().any(|n| n.kind == NodeKind::Namespace));
    let class = find(&r, NodeKind::Class, "Widget").unwrap();
    assert_eq!(class.qualified_name, "app::Widget");

    // Out-of-line `Widget::size` → METHOD named size, receiver override QN.
    let size = find(&r, NodeKind::Method, "size").unwrap();
    assert_eq!(size.qualified_name, "Widget::size");
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "helper"
        && u.from_node_id == size.id));

    // Free function gets the namespace prefix.
    let boot = find(&r, NodeKind::Function, "boot").unwrap();
    assert_eq!(boot.qualified_name, "app::boot");
}

#[test]
fn export_macro_class_recovers_via_blanker() {
    // `class ENGINE_API Widget : public Base { … }` misparses without the
    // pre-parse blank (#1061); post-blank it extracts as a real class with
    // its member — and NO phantom whole-body function survives (#946).
    let code =
        "class ENGINE_API Widget : public Base {\n public:\n  void render() { draw(); }\n};\n";
    let r = extract("Widget.h", code);
    let class = find(&r, NodeKind::Class, "Widget").unwrap();
    assert!(
        r.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Method && n.name == "render")
    );
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "Widget"),
        "no phantom whole-body function for the class"
    );
    let _ = class;
}

#[test]
fn forward_declarations_do_not_mint_classes() {
    let code = "class Widget;\nstruct Point;\nclass Real { public: int x; };\n";
    let r = extract("fwd.cpp", code);
    assert!(find(&r, NodeKind::Class, "Widget").is_none(), "#1093 skip");
    assert!(find(&r, NodeKind::Struct, "Point").is_none());
    assert!(find(&r, NodeKind::Class, "Real").is_some());
}

#[test]
fn template_args_stripped_from_callees() {
    let code =
        "void launch() {\n  compute<float, 256>(data);\n  ns::reduce<int>(x);\n  helper(y);\n}\n";
    let r = extract("kern.cpp", code);
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(
        calls.contains(&"compute"),
        "template args stripped: {calls:?}"
    );
    assert!(
        calls.contains(&"ns::reduce"),
        "qualified keeps path: {calls:?}"
    );
    assert!(calls.contains(&"helper"));
    assert!(
        !calls.iter().any(|c| c.contains('<')),
        "no <> survives: {calls:?}"
    );
}

#[test]
fn macro_defined_kernel_names_recover() {
    // `DEFINE_KERNEL(real_name, typed args…) { … }` — the node is named by
    // the first lone-identifier param, not the macro (#1093 follow-up).
    let code =
        "DEFINE_FLASH_KERNEL(flash_fwd_kernel, bool Is_dropout, int kBlockM) {\n  run();\n}\n";
    let r = extract("kernels.cu", code);
    assert!(
        find(&r, NodeKind::Function, "flash_fwd_kernel").is_some(),
        "names: {:?}",
        r.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(find(&r, NodeKind::Function, "DEFINE_FLASH_KERNEL").is_none());
}

#[test]
fn misparse_guards_drop_phantom_functions() {
    // A macro-confused namespace block misparsing as a function named
    // "namespace detail" must not mint a node (body still walked).
    let code = "NLOHMANN_JSON_NAMESPACE_BEGIN\nnamespace detail {\nvoid inner() { work(); }\n}\nNLOHMANN_JSON_NAMESPACE_END\n";
    let r = extract("json.hpp", code);
    assert!(
        !r.nodes.iter().any(|n| n.name.starts_with("namespace")),
        "no namespace-named phantom: {:?}",
        r.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    // The real inner function still extracts.
    assert!(find(&r, NodeKind::Function, "inner").is_some());
}

#[test]
fn using_alias_and_member_visibility() {
    let code = "class Cfg {\n public:\n  using Map = std::map<std::string, int>;\n  int get() const { return 1; }\n};\n";
    let r = extract("cfg.cpp", code);
    assert!(
        find(&r, NodeKind::TypeAlias, "Map").is_some(),
        "using alias"
    );
    let get = find(&r, NodeKind::Method, "get").unwrap();
    assert_eq!(get.visibility, Some(Visibility::Public));
}

#[test]
fn anonymous_typedef_bodies_nest_under_the_alias_node() {
    // Ubiquitous C-header idiom, also legal C++: the inner specifier is
    // anonymous, so TS (tree-sitter.ts:2840-2859 struct, 2861-2885 enum)
    // pushes the TYPEDEF node and walks the inner body under it. Members must
    // carry the alias QN and the inner specifier must mint no phantom.
    let code = "typedef struct {\n  int run() { return helper(); }\n} Runner;\ntypedef enum { LOW, HIGH } Level;\n";
    let r = extract("runner.cpp", code);

    let runner = find(&r, NodeKind::Struct, "Runner").unwrap();
    let run = find(&r, NodeKind::Method, "run").unwrap();
    assert_eq!(run.qualified_name, "Runner::run");
    assert!(
        r.edges
            .iter()
            .any(|e| e.source == runner.id && e.target == run.id)
    );

    let level = find(&r, NodeKind::Enum, "Level").unwrap();
    let low = find(&r, NodeKind::EnumMember, "LOW").unwrap();
    assert_eq!(low.qualified_name, "Level::LOW");
    assert!(
        r.edges
            .iter()
            .any(|e| e.source == level.id && e.target == low.id)
    );

    assert!(
        !r.nodes.iter().any(|n| n.name == "<anonymous>"),
        "phantom node minted: {:?}",
        r.nodes
            .iter()
            .map(|n| (n.kind, n.name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn file_scope_class_typed_declaration_mints_a_variable() {
    // `Foo x;` — a C++ `declaration` whose declarator is a BARE identifier
    // (the type sits in the `type` field, not in a declarator). TS's generic
    // variable fallback (tree-sitter.ts:2802-2818) scans the named children
    // for such an identifier and mints the global; the left/named_child(0)
    // shape only ever sees the TYPE node, so the global went unextracted (no
    // impact-radius edges into it).
    let code = "class Foo {\n public:\n  void go();\n};\nFoo gInstance;\n";
    let r = extract("globals.cpp", code);
    assert!(
        find(&r, NodeKind::Variable, "gInstance").is_some(),
        "file-scope `Foo gInstance;`: {:?}",
        r.nodes
            .iter()
            .map(|n| (n.kind, n.name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn representative_cpp_fixture_snapshot() {
    let code = "#include <vector>\n\nnamespace geo {\n\n// a 2D point\nstruct Point {\n  double x;\n  double y;\n};\n\nclass MYLIB_API Shape {\n public:\n  double area() const;\n};\n\ndouble Shape::area() const {\n  return compute_area<double>(*this);\n}\n\n}  // namespace geo\n";
    let r = extract("geo.cpp", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}
