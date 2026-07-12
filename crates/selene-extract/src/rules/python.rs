//! Python rules — verbatim port of `languages/python.ts` (map §11 python
//! row): functionTypes = methodTypes = `[function_definition]` (a function
//! is a method iff it sits inside a class-like scope), classes
//! `[class_definition]`, imports `[import_statement,
//! import_from_statement]`, calls `[call]` (wired in Task 6), variables
//! `[assignment]` (top-level only, per the core walker gate).
//!
//! The core walker carries Python's multi-import machinery: `import a, b`
//! inline (one `import` node + `imports` ref per dotted_name /
//! aliased_import) and `from m import X, Y` per-name refs
//! (`emit_py_from_import_refs`) — the `extract_import` hook here handles
//! ONLY `import_from_statement` and declines the rest (`None` = "I didn't
//! handle this", NOT "use a generic fallback").

use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::{ImportInfo, LanguageRules, NodeTypeTables};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["function_definition"],
    class_types: &["class_definition"],
    // Methods are functions inside classes.
    method_types: &["function_definition"],
    import_types: &["import_statement", "import_from_statement"],
    call_types: &["call"],
    // Python uses assignment for variable declarations.
    variable_types: &["assignment"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("return_type"),
    ..NodeTypeTables::EMPTY
};

pub(crate) struct PythonRules;

impl LanguageRules for PythonRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// `(params) -> ReturnType` (the `" -> "` join is the TS spelling).
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let mut sig = get_node_text(params, source).to_string();
        if let Some(ret) = get_child_by_field(node, "return_type") {
            sig.push_str(" -> ");
            sig.push_str(get_node_text(ret, source));
        }
        Some(sig)
    }

    /// `async def` — DIVERGENCE vs the TS config: tree-sitter-python 0.25
    /// nests the `async` keyword as the def's first (unnamed) child; the
    /// WASM-era grammar exposed it as a preceding sibling (which TS
    /// checked). Both shapes are checked so a future grammar shift can't
    /// silently drop the flag.
    fn is_async(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        let child_async = node.child(0).is_some_and(|c| c.kind() == "async");
        let sibling_async = node.prev_sibling().is_some_and(|p| p.kind() == "async");
        Some(child_async || sibling_async)
    }

    /// `@staticmethod` — a preceding `decorator` named sibling containing
    /// the marker (text match, exactly as TS).
    fn is_static(&self, node: Node<'_>, source: &str) -> Option<bool> {
        Some(node.prev_named_sibling().is_some_and(|p| {
            p.kind() == "decorator" && get_node_text(p, source).contains("staticmethod")
        }))
    }

    /// `from m import ...` → module name + full statement text as signature.
    /// `import_statement` returns `None` — the core's inline multi-import
    /// handler owns it.
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        if node.kind() != "import_from_statement" {
            return None;
        }
        let module = get_child_by_field(node, "module_name")?;
        Some(ImportInfo {
            module_name: get_node_text(module, source).to_string(),
            signature: get_node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
}
