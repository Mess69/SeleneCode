//! Ported Python conformance tests: the Python describe blocks of
//! `extraction.test.ts` (extraction, imports, #780 decorated docstrings)
//! run through the full `extract_from_source` pipeline, plus the Task 5
//! walker-core pins (file node, containment, the 0→1 line boundary, one
//! insta snapshot of a representative fixture).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{EdgeKind, NodeKind};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Python)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

// =============================================================================
// extraction.test.ts — describe('Python Extraction')
// =============================================================================

#[test]
fn extracts_function_definitions() {
    let code = "\ndef calculate_total(items: list, tax_rate: float) -> float:\n    \"\"\"Calculate total with tax.\"\"\"\n    subtotal = sum(item.price for item in items)\n    return subtotal * (1 + tax_rate)\n";
    let r = extract("calc.py", code);

    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let file = r.nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
    assert_eq!(file.id, "file:calc.py");
    assert_eq!(file.name, "calc.py");
    assert_eq!(file.qualified_name, "calc.py");
    assert_eq!(file.start_line, 1);

    let f = find(&r, NodeKind::Function, "calculate_total").unwrap();
    assert_eq!(f.language, "python");
    assert_eq!(f.qualified_name, "calculate_total");
    assert_eq!(
        f.signature.as_deref(),
        Some("(items: list, tax_rate: float) -> float")
    );
    // Local assignments inside the body are NOT extracted (top-level gate).
    assert!(find(&r, NodeKind::Variable, "subtotal").is_none());
}

#[test]
fn extracts_class_definitions_with_methods() {
    let code = "\nclass UserService:\n    \"\"\"Service for managing users.\"\"\"\n\n    def __init__(self, db):\n        self.db = db\n\n    def get_user(self, user_id: str) -> User:\n        return self.db.find_user(user_id)\n";
    let r = extract("service.py", code);

    let class = find(&r, NodeKind::Class, "UserService").unwrap();
    assert_eq!(class.qualified_name, "UserService");

    // Functions inside a class are METHODS with Class::method QNs.
    let init = find(&r, NodeKind::Method, "__init__").unwrap();
    assert_eq!(init.qualified_name, "UserService::__init__");
    let get_user = find(&r, NodeKind::Method, "get_user").unwrap();
    assert_eq!(get_user.qualified_name, "UserService::get_user");
    assert_eq!(
        get_user.signature.as_deref(),
        Some("(self, user_id: str) -> User")
    );

    // Containment: file → class, class → both methods.
    let contains: Vec<(&str, &str)> = r
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .map(|e| (e.source.as_str(), e.target.as_str()))
        .collect();
    let file_id = "file:service.py";
    assert!(contains.contains(&(file_id, class.id.as_str())));
    assert!(contains.contains(&(class.id.as_str(), init.id.as_str())));
    assert!(contains.contains(&(class.id.as_str(), get_user.id.as_str())));
}

// =============================================================================
// The 0→1 line boundary pin (controller-flagged): a symbol on the FIRST
// line of the file must embed line 1 in its id input — byte-identical to
// the id the TS engine would emit for the same symbol.
// =============================================================================

#[test]
fn first_line_symbol_gets_line_1_in_its_id() {
    let r = extract("m.py", "def f():\n    pass\n");
    let f = find(&r, NodeKind::Function, "f").unwrap();
    assert_eq!(f.start_line, 1, "tree-sitter row 0 must become line 1");
    assert_eq!(
        f.id,
        selene_core::node_id("m.py", NodeKind::Function, "f", 1),
        "id must embed the 1-based line"
    );
    // And NOT the 0-based row:
    assert_ne!(
        f.id,
        selene_core::node_id("m.py", NodeKind::Function, "f", 0)
    );
}

// =============================================================================
// extraction.test.ts — 'captures docstrings for decorated Python
// declarations, stripping `#` (#780)' — through the full pipeline this time
// (the helper-level port lives in src/helpers.rs).
// =============================================================================

#[test]
fn decorated_declaration_docstrings_and_decorates_refs() {
    let code = "# decorated function\n@app.route(\"/x\")\ndef py_handler():\n    return 1\n\n\n# plain function control\ndef py_plain():\n    return 1\n\n\n# decorated class\n@dataclass\nclass PyModel:\n    pass\n";
    let r = extract("mod.py", code);

    assert_eq!(
        find(&r, NodeKind::Function, "py_handler")
            .unwrap()
            .docstring
            .as_deref(),
        Some("decorated function")
    );
    assert_eq!(
        find(&r, NodeKind::Function, "py_plain")
            .unwrap()
            .docstring
            .as_deref(),
        Some("plain function control")
    );
    assert_eq!(
        find(&r, NodeKind::Class, "PyModel")
            .unwrap()
            .docstring
            .as_deref(),
        Some("decorated class")
    );

    // Decorator refs: invoked decorator unwraps its callee and takes the
    // last dotted segment; bare decorator keeps its name.
    let decorates: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "decorates")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(decorates.contains(&"route"), "decorates: {decorates:?}");
    assert!(decorates.contains(&"dataclass"), "decorates: {decorates:?}");
}

#[test]
fn async_and_staticmethod_flags() {
    let code = "class C:\n    @staticmethod\n    def s():\n        pass\n\n    async def a(self):\n        pass\n";
    let r = extract("flags.py", code);
    assert_eq!(
        find(&r, NodeKind::Method, "s").unwrap().is_static,
        Some(true)
    );
    assert_eq!(
        find(&r, NodeKind::Method, "a").unwrap().is_async,
        Some(true)
    );
    assert_eq!(
        find(&r, NodeKind::Method, "a").unwrap().is_static,
        Some(false)
    );
}

// =============================================================================
// extraction.test.ts — describe('Python imports') — all seven cases
// =============================================================================

#[test]
fn simple_import_statement() {
    let r = extract("utils.py", "import json");
    let imp = find(&r, NodeKind::Import, "json").unwrap();
    assert_eq!(imp.signature.as_deref(), Some("import json"));
    // Bare `import mod` pushes an imports ref (Django signals pattern).
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "imports" && u.reference_name == "json")
    );
}

#[test]
fn from_import_statement() {
    let r = extract("utils.py", "from os import path");
    let imp = find(&r, NodeKind::Import, "os").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains("path"));
    // Per-name ref for the imported binding.
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "imports" && u.reference_name == "path")
    );
}

#[test]
fn multiple_names_from_same_module() {
    let r = extract("types.py", "from typing import List, Dict, Optional");
    let imp = find(&r, NodeKind::Import, "typing").unwrap();
    let sig = imp.signature.as_deref().unwrap();
    assert!(sig.contains("List") && sig.contains("Dict"));
    for name in ["List", "Dict", "Optional"] {
        assert!(
            r.unresolved
                .iter()
                .any(|u| u.reference_kind == "imports" && u.reference_name == name),
            "missing per-name ref for {name}"
        );
    }
}

#[test]
fn multiple_import_statements() {
    let r = extract("main.py", "\nimport os\nimport sys\n");
    let names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"os") && names.contains(&"sys"));
}

#[test]
fn aliased_import() {
    let r = extract("data.py", "import numpy as np");
    let imp = find(&r, NodeKind::Import, "numpy").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains("as np"));
}

#[test]
fn relative_import() {
    let r = extract("module.py", "from .utils import helper");
    let imp = find(&r, NodeKind::Import, ".utils").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains("helper"));
}

#[test]
fn wildcard_import() {
    let r = extract("types.py", "from typing import *");
    let imp = find(&r, NodeKind::Import, "typing").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains('*'));
    // The wildcard itself gets NO per-name ref.
    assert!(
        !r.unresolved
            .iter()
            .any(|u| u.reference_kind == "imports" && u.reference_name == "*")
    );
}

// =============================================================================
// Top-level variables + unsupported-language shapes
// =============================================================================

#[test]
fn top_level_assignment_is_a_variable_with_init_signature() {
    let r = extract("consts.py", "MAX_RETRIES = 3\n");
    let v = find(&r, NodeKind::Variable, "MAX_RETRIES").unwrap();
    assert_eq!(v.signature.as_deref(), Some("= 3"));
}

#[test]
fn unsupported_language_is_a_warning_not_an_error() {
    use selene_extract::{ErrorCode, Severity};
    // Wave-2 language (detects, no v0 rules): warning.
    let r = extract_from_source("x.swift", "func f() {}", Language::Swift);
    assert!(r.nodes.is_empty());
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].code, ErrorCode::UnsupportedLanguage);
    assert_eq!(r.errors[0].severity, Severity::Warning);
    // Unknown: error.
    let r = extract_from_source("x.zzz", "?", Language::Unknown);
    assert_eq!(r.errors[0].severity, Severity::Error);
}

// =============================================================================
// Snapshot: the full ExtractionResult for a representative fixture.
// Deliberately NOT sorted — walk order is deterministic and the snapshot
// pins it. updatedAt/durationMs redacted (the only non-deterministic
// fields).
// =============================================================================

#[test]
fn representative_fixture_snapshot() {
    let code = "# Repo layer\nfrom typing import Optional\n\nimport json\n\n\nclass Repo:\n    \"\"\"docstring in body (not captured by preceding-comment rules)\"\"\"\n\n    @staticmethod\n    def parse(raw: str) -> Optional[dict]:\n        return json.loads(raw)\n\n\n# module entry\nasync def main() -> None:\n    pass\n\n\nDEFAULT_LIMIT = 50\n";
    let r = extract("repo.py", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}

// =============================================================================
// Task 6 — body walker (Python-only; TS/Go/Rust call shapes land with
// their configs' tasks)
// =============================================================================

#[test]
fn call_refs_receiver_skip_set_and_bare() {
    let code = "def run(self):\n    obj.method()\n    self.helper()\n    cls.make()\n    plain()\n    json.loads(\"{}\")\n";
    let r = extract("calls.py", code);
    let calls: Vec<(&str, Option<u32>, Option<u32>)> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls")
        .map(|u| (u.reference_name.as_str(), u.line, u.column))
        .collect();
    let names: Vec<&str> = calls.iter().map(|c| c.0).collect();
    assert!(names.contains(&"obj.method"), "calls: {names:?}");
    assert!(names.contains(&"helper"), "self.x() strips receiver");
    assert!(names.contains(&"make"), "cls.x() strips receiver");
    assert!(names.contains(&"plain"));
    assert!(names.contains(&"json.loads"), "module calls keep receiver");
    // Line/column pin: obj.method() sits on line 2, column 4.
    assert!(calls.contains(&("obj.method", Some(2), Some(4))));
}

#[test]
fn nested_named_functions_become_nodes() {
    let code = "def outer():\n    def inner():\n        leaf()\n    inner()\n";
    let r = extract("nested.py", code);
    let inner = find(&r, NodeKind::Function, "inner").unwrap();
    assert_eq!(inner.qualified_name, "outer::inner");
    // inner's body call attributes to inner, outer's call to outer.
    let outer_id = &find(&r, NodeKind::Function, "outer").unwrap().id;
    let inner_calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls" && u.from_node_id == inner.id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert_eq!(inner_calls, vec!["leaf"]);
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.from_node_id == *outer_id
        && u.reference_name == "inner"));
}

#[test]
fn value_refs_emit_and_shadow_prune() {
    // KEPT: DEFAULT_LIMIT read by reader() (distinctive, unshadowed).
    // PRUNED: CONFIG — shadowed by a local `CONFIG = {}` binding.
    // SKIPPED: xy (too short / no [A-Z_]).
    let code = "DEFAULT_LIMIT = 50\nCONFIG = {}\nxy = 1\n\n\ndef reader():\n    a = DEFAULT_LIMIT\n    return a\n\n\ndef shadower():\n    CONFIG = {}\n    return CONFIG\n";
    let r = extract("vals.py", code);
    let value_refs: Vec<(&str, &str)> = r
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::References
                && e.metadata
                    .as_ref()
                    .is_some_and(|m| m.get("valueRef") == Some(&serde_json::Value::Bool(true)))
        })
        .map(|e| (e.source.as_str(), e.target.as_str()))
        .collect();

    let reader_id = &find(&r, NodeKind::Function, "reader").unwrap().id;
    let limit_id = &find(&r, NodeKind::Variable, "DEFAULT_LIMIT").unwrap().id;
    assert!(
        value_refs.contains(&(reader_id.as_str(), limit_id.as_str())),
        "reader must reference DEFAULT_LIMIT: {value_refs:?}"
    );
    // CONFIG shadow-pruned: no value ref targets it from anywhere.
    let config_id = &find(&r, NodeKind::Variable, "CONFIG").unwrap().id;
    assert!(
        !value_refs.iter().any(|(_, t)| t == &config_id.as_str()),
        "shadowed CONFIG must be pruned: {value_refs:?}"
    );
    // Provenance stamped on value-ref edges.
    assert!(r.edges.iter().all(|e| e.provenance.is_some()));
}

#[test]
fn python_constructor_calls_stay_calls_not_instantiates() {
    // Python has no INSTANTIATION_KINDS node — `Foo()` is a `call`. The
    // instantiates branch is exercised with TS's new_expression (Task 7).
    let code = "def make():\n    return Widget()\n";
    let r = extract("mk.py", code);
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "calls" && u.reference_name == "Widget")
    );
    assert!(
        !r.unresolved
            .iter()
            .any(|u| u.reference_kind == "instantiates")
    );
}

/// Inheritance-gap closure — Python's superclass list is an `argument_list` of
/// identifiers, gated on `class_definition` so a CALL's arguments can never be
/// read as base classes (tree-sitter.ts:5326-5341).
#[test]
fn extracts_python_base_class_refs() {
    let code = "class Base:\n    def handle(self):\n        return 0\n\n\nclass Mixin:\n    pass\n\n\nclass Child(Base, Mixin):\n    def handle(self):\n        return 1\n";
    let r = extract("inherit.py", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    let extends: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "extends")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert_eq!(extends, vec!["Base", "Mixin"]);
}

/// The `class_definition` gate: a plain function call's `argument_list` must not
/// produce inheritance refs.
#[test]
fn python_call_arguments_are_not_base_classes() {
    let code = "def go():\n    return handle(Base, Mixin)\n";
    let r = extract("call.py", code);
    assert!(
        !r.unresolved.iter().any(|u| u.reference_kind == "extends"),
        "a call's args leaked as base classes: {:?}",
        r.unresolved
    );
}
