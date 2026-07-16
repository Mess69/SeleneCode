//! Ported JavaScript conformance tests: the JS cases of the Arrow Function
//! Export block, the #808 `field_definition` name divergence (the
//! `property` field), and one insta snapshot.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Javascript)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

#[test]
fn exported_async_arrow_extracts_as_function() {
    let code = "\nexport const fetchData = async () => {\n  const response = await fetch('/api/data');\n  return response.json();\n};\n";
    let r = extract("api.js", code);
    let f = find(&r, NodeKind::Function, "fetchData").unwrap();
    assert_eq!(f.is_exported, Some(true));
    assert_eq!(f.language.as_str(), "javascript");
}

#[test]
fn js_field_definition_names_via_property_field_808() {
    // JS `field_definition` names its key with the `property` field —
    // without resolve_name these fields extracted no node at all.
    let code = "class Widget {\n  onClick = (e) => {\n    this.handle(e);\n  };\n  count = 0;\n}\n";
    let r = extract("widget.js", code);
    let on_click = find(&r, NodeKind::Method, "onClick").unwrap();
    assert_eq!(on_click.qualified_name, "Widget::onClick");
    // Plain field → property (#808 classify shared with TS).
    assert!(find(&r, NodeKind::Property, "count").is_some());
    // Body calls attribute to the method.
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "handle"
        && u.from_node_id == on_click.id));
}

#[test]
fn js_imports_and_calls() {
    let code = "import { load } from './loader';\n\nexport function run() {\n  const data = load();\n  helpers.process(data);\n}\n";
    let r = extract("run.js", code);
    let imp = find(&r, NodeKind::Import, "./loader").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains("load"));
    let run_id = &find(&r, NodeKind::Function, "run").unwrap().id;
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls" && &u.from_node_id == run_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(calls.contains(&"load"));
    assert!(calls.contains(&"helpers.process"));
}

#[test]
fn representative_js_fixture_snapshot() {
    let code = "// legacy module wrapper\n(function () {\n  function init() {\n    render();\n  }\n  init();\n})();\n\nexport class App {\n  start = () => {\n    boot();\n  };\n}\n";
    let r = extract("app.js", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}

/// Inheritance-gap closure — JS `class_heritage` holds a BARE identifier, with
/// no `extends_clause` wrapper (tree-sitter.ts:5499-5513).
#[test]
fn extracts_javascript_class_extends_ref() {
    let code = "class Animal {\n  speak() {\n    return 'generic';\n  }\n}\n\nexport class Dog extends Animal {\n  speak() {\n    return 'woof';\n  }\n}\n";
    let r = extract("inherit.js", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    let extends: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "extends")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert_eq!(extends, vec!["Animal"]);

    // Attributed to the derived class, not the file.
    let dog = r
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "Dog")
        .unwrap();
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "extends" && u.from_node_id == dog.id)
    );
}
