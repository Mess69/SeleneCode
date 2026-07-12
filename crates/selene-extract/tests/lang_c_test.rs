//! Ported C conformance tests: functions (declarator unwrapping incl.
//! pointer returns), structs/enums/typedef reclassification, const
//! detection via `type_qualifier`, `#include` imports, and one insta
//! snapshot.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::C)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

#[test]
fn extracts_functions_with_declarator_unwrapping() {
    let code = "int add(int a, int b) { return a + b; }\nchar* dup_name(const char* s) { return strdup(s); }\n";
    let r = extract("math.c", code);
    assert!(find(&r, NodeKind::Function, "add").is_some());
    // Pointer-returning function: the pointer_declarator unwraps — the
    // node is named `dup_name`, never `*dup_name(...)`.
    assert!(
        find(&r, NodeKind::Function, "dup_name").is_some(),
        "names: {:?}",
        r.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    // Its body call attributes to it.
    let dup = find(&r, NodeKind::Function, "dup_name").unwrap();
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "strdup"
        && u.from_node_id == dup.id));
}

#[test]
fn extracts_structs_enums_and_typedefs() {
    let code = "struct point { int x; int y; };\nenum color { RED, GREEN };\ntypedef struct { int w; int h; } size_t2;\ntypedef enum { LOW, HIGH } level_t;\n";
    let r = extract("types.c", code);
    assert!(find(&r, NodeKind::Struct, "point").is_some());
    let color = find(&r, NodeKind::Enum, "color").unwrap();
    // Enum members extracted under the enum.
    let red = find(&r, NodeKind::EnumMember, "RED").unwrap();
    assert_eq!(red.qualified_name, "color::RED");
    assert!(
        r.edges
            .iter()
            .any(|e| e.source == color.id && e.target == red.id)
    );
    // Anonymous typedef struct/enum reclassify to the typedef NAME.
    assert!(find(&r, NodeKind::Struct, "size_t2").is_some());
    assert!(find(&r, NodeKind::Enum, "level_t").is_some());
}

#[test]
fn const_globals_are_constants() {
    let code = "const int MAX_ITEMS = 128;\nint counter = 0;\n";
    let r = extract("globals.c", code);
    assert!(find(&r, NodeKind::Constant, "MAX_ITEMS").is_some());
    assert!(find(&r, NodeKind::Variable, "counter").is_some());
}

#[test]
fn includes_extract_both_forms() {
    let code = "#include <stdio.h>\n#include \"myheader.h\"\n";
    let r = extract("main.c", code);
    let sys = find(&r, NodeKind::Import, "stdio.h").unwrap();
    assert!(sys.signature.as_deref().unwrap().contains("<stdio.h>"));
    assert!(find(&r, NodeKind::Import, "myheader.h").is_some());
}

#[test]
fn representative_c_fixture_snapshot() {
    let code = "#include <stdlib.h>\n\n// buffer bounds\nconst int BUF_MAX = 4096;\n\nstruct buffer { char* data; int len; };\n\ntypedef enum { OK, ERR } status_t;\n\n// grow to fit\nstatus_t grow(struct buffer* b, int need) {\n    b->data = realloc(b->data, need);\n    return b->data ? OK : ERR;\n}\n";
    let r = extract("buf.c", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}
