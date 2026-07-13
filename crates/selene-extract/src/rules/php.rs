//! PHP rules — verbatim port of `languages/php.ts`: include/require(+_once)
//! expressions are imports with **static string-literal paths only** (#660 —
//! dynamic paths silently skipped, "silent beats wrong"); class constants
//! and trait `use` → `implements` refs via the `visit_node` hook; visibility
//! defaults to Public; `self|static|$this` returns become the marker
//! `'self'` (#608); the file-level `namespace Foo\Bar;` **unbraced** form
//! scopes qualified names.
//!
//! Grouped imports (`use X\{A, B}`): the TS config returned null and the TS
//! *core* fanned the group out into one import node per member. That core
//! machinery is walker-ladder territory (core-owned); here the fan-out lives
//! in this config's `visit_node` hook instead — same observable output, no
//! walker edit (mechanism divergence documented in the Task 14 report).

use selene_core::{NodeKind, Visibility};
use tree_sitter::Node;

use crate::UnresolvedReference;
use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::{ClassKind, ImportInfo, LanguageRules, NodeTypeTables};
use crate::walker::{NodeExtra, Session};

/// include / require (+ `_once`) expression node types — the file→file
/// dependency in procedural PHP (#660).
const PHP_INCLUDE_TYPES: [&str; 4] = [
    "include_expression",
    "include_once_expression",
    "require_expression",
    "require_once_expression",
];

/// PHP built-in return types that can't be a method receiver (nothing to
/// chain on). Copied verbatim from `PHP_NON_CLASS_RETURN` (18 names — the
/// task brief said 19; the TS source has 18, source wins).
const PHP_NON_CLASS_RETURN: [&str; 18] = [
    "array", "string", "int", "integer", "float", "double", "bool", "boolean", "void", "mixed",
    "never", "null", "false", "true", "object", "callable", "iterable", "resource",
];

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["function_definition"],
    class_types: &["class_declaration", "trait_declaration"],
    method_types: &["method_declaration"],
    interface_types: &["interface_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_case"],
    import_types: &[
        "namespace_use_declaration",
        "include_expression",
        "include_once_expression",
        "require_expression",
        "require_once_expression",
    ],
    call_types: &[
        "function_call_expression",
        "member_call_expression",
        "scoped_call_expression",
    ],
    variable_types: &["const_declaration"],
    field_types: &["property_declaration"],
    // PHP `namespace Foo\Bar;` is file-level (like a Java package) — see
    // `extract_package` for the unbraced-only gate.
    package_types: &["namespace_definition"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("return_type"),
    ..NodeTypeTables::EMPTY
};

/// Static string-literal path of an include/require expression, or `None`
/// for dynamic forms (`include $var`, `__DIR__ . '/x'`, interpolation).
fn php_static_include_path<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    // The path argument is the expression's first named child; the
    // call-style form `require("x")` wraps it in a parenthesized_expression.
    let mut arg = node.named_child(0)?;
    if arg.kind() == "parenthesized_expression" {
        arg = arg.named_child(0)?;
    }
    if arg.kind() != "string" && arg.kind() != "encapsed_string" {
        return None;
    }
    // Pure literal only: any non-`string_content` child (interpolated
    // variable, escape sequence, …) means the value isn't a static path.
    let mut cursor = arg.walk();
    let mut content: Option<Node<'_>> = None;
    for part in arg.named_children(&mut cursor) {
        if part.kind() != "string_content" {
            return None;
        }
        content.get_or_insert(part);
    }
    content.map(|c| get_node_text(c, source))
}

pub(crate) struct PhpRules;

impl LanguageRules for PhpRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    fn classify_class_node(&self, node: Node<'_>, _source: &str) -> Option<ClassKind> {
        Some(if node.kind() == "trait_declaration" {
            ClassKind::Trait
        } else {
            ClassKind::Class
        })
    }

    /// PHP defaults to public.
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let mut i = 0;
        while let Some(child) = node.child(i) {
            if child.kind() == "visibility_modifier" {
                match get_node_text(child, source) {
                    "public" => return Some(Visibility::Public),
                    "private" => return Some(Visibility::Private),
                    "protected" => return Some(Visibility::Protected),
                    _ => {}
                }
            }
            i += 1;
        }
        Some(Visibility::Public)
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        let mut i = 0;
        while let Some(child) = node.child(i) {
            if child.kind() == "static_modifier" {
                return Some(true);
            }
            i += 1;
        }
        Some(false)
    }

    /// Class constants, trait `use`, and grouped namespace imports — the
    /// three shapes the dispatch ladder can't route from tables alone.
    fn visit_node(&self, node: Node<'_>, s: &mut Session<'_>) -> bool {
        match node.kind() {
            // Class constants: `const STATUS = 'x';` inside a class body —
            // one `constant` node per const_element.
            "const_declaration" => {
                let mut cursor = node.walk();
                let elements: Vec<Node<'_>> = node
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "const_element")
                    .collect();
                for elem in elements {
                    let mut c2 = elem.walk();
                    let Some(name_node) = elem.named_children(&mut c2).find(|c| c.kind() == "name")
                    else {
                        continue;
                    };
                    let name = get_node_text(name_node, s.source()).to_string();
                    s.create_node(NodeKind::Constant, &name, elem, NodeExtra::default());
                }
                true
            }
            // Trait usage inside classes: `use TraitName, Other;` →
            // unresolved `implements` refs from the enclosing class.
            "use_declaration" => {
                let Some(parent_id) = s.node_stack().last().cloned() else {
                    return false;
                };
                let mut cursor = node.walk();
                let names: Vec<Node<'_>> = node
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "name" || c.kind() == "qualified_name")
                    .collect();
                for name_node in names {
                    let trait_name = get_node_text(name_node, s.source()).to_string();
                    s.add_unresolved(UnresolvedReference {
                        from_node_id: parent_id.clone(),
                        reference_name: trait_name,
                        reference_kind: "implements".to_string(),
                        line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
                        column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
                        file_path: None,
                        language: None,
                    });
                }
                true
            }
            // Grouped imports `use X\{A, B};`: one import node per member,
            // prefix-joined (the TS core's fan-out, relocated — module docs).
            "namespace_use_declaration" => {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                let prefix = children.iter().find(|c| c.kind() == "namespace_name");
                let group = children.iter().find(|c| c.kind() == "namespace_use_group");
                let (Some(prefix), Some(group)) = (prefix, group) else {
                    return false; // single-use forms: the ladder's import branch
                };
                let signature = get_node_text(node, s.source()).trim().to_string();
                let prefix_text = get_node_text(*prefix, s.source()).to_string();
                let mut g_cursor = group.walk();
                let clauses: Vec<Node<'_>> = group
                    .named_children(&mut g_cursor)
                    .filter(|c| {
                        c.kind() == "namespace_use_group_clause"
                            || c.kind() == "namespace_use_clause"
                    })
                    .collect();
                let parent_id = s.node_stack().last().cloned();
                for clause in clauses {
                    let mut c2 = clause.walk();
                    let Some(member) = clause.named_children(&mut c2).find(|c| {
                        c.kind() == "namespace_name"
                            || c.kind() == "name"
                            || c.kind() == "qualified_name"
                    }) else {
                        continue;
                    };
                    let module = format!("{prefix_text}\\{}", get_node_text(member, s.source()));
                    let extra = NodeExtra {
                        signature: Some(signature.clone()),
                        ..NodeExtra::default()
                    };
                    s.create_node(NodeKind::Import, &module, node, extra);
                    // The ref is the namespace-QUALIFIED `Foo\Bar::Baz` spelling —
                    // the form PHP classes are stored under, so it resolves to the
                    // right definition (tree-sitter.ts:3280 → pushPhpUseRef). The
                    // import NODE keeps the raw FQN above; only the REF is
                    // requalified. Emitting the raw FQN as the ref (what we did)
                    // silently never resolved.
                    if let Some(pid) = &parent_id {
                        let pid = pid.clone();
                        crate::walker::push_php_use_ref(s, &module, &pid, node);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// `self` / `static` / `$this` → the marker `'self'` (resolved to the
    /// declaring class later); concrete types → short name; primitives /
    /// unions / nullable non-class types → `None` (#608).
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut rt = get_child_by_field(node, "return_type")?;
        // Unwrap `?Type`. Union / intersection types are ambiguous — skip.
        if rt.kind() == "optional_type" {
            rt = rt.named_child(0).unwrap_or(rt);
        }
        if rt.kind() == "primitive_type" {
            return None;
        }
        let name_node = if rt.kind() == "named_type" {
            rt.named_child(0).unwrap_or(rt)
        } else {
            rt
        };
        let text = get_node_text(name_node, source)
            .trim()
            .trim_start_matches('\\');
        if text.is_empty() {
            return None;
        }
        let last = text.rsplit('\\').next().unwrap_or(text);
        let lc = last.to_lowercase();
        if lc == "self" || lc == "static" || lc == "this" || lc == "$this" {
            return Some("self".to_string());
        }
        if PHP_NON_CLASS_RETURN.contains(&lc.as_str()) {
            return None;
        }
        let mut chars = last.chars();
        let head_ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if !head_ok || !last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None; // union/intersection/complex
        }
        Some(last.to_string())
    }

    /// File-level `namespace Foo\Bar;` — the unbraced form only (a braced
    /// `namespace Foo { … }` has a body and is skipped).
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        let ns_name = children.iter().find(|c| c.kind() == "namespace_name")?;
        let has_body = children
            .iter()
            .any(|c| c.kind() == "compound_statement" || c.kind() == "declaration_list");
        if has_body {
            return None;
        }
        Some(get_node_text(*ns_name, source).to_string())
    }

    /// include/require static paths (#660) and single `use` clauses; grouped
    /// `use` is `visit_node`'s job and never reaches here.
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = get_node_text(node, source).trim().to_string();

        if PHP_INCLUDE_TYPES.contains(&node.kind()) {
            let path = php_static_include_path(node, source)?;
            return Some(ImportInfo {
                module_name: path.to_string(),
                signature,
                handled_refs: false,
            });
        }

        // Single import — find the namespace_use_clause.
        let mut cursor = node.walk();
        let use_clause = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "namespace_use_clause")?;
        let mut c2 = use_clause.walk();
        let target = use_clause
            .named_children(&mut c2)
            .find(|c| c.kind() == "qualified_name")
            .or_else(|| {
                let mut c3 = use_clause.walk();
                use_clause
                    .named_children(&mut c3)
                    .find(|c| c.kind() == "name")
            })?;
        Some(ImportInfo {
            module_name: get_node_text(target, source).to_string(),
            signature,
            handled_refs: false,
        })
    }
}
