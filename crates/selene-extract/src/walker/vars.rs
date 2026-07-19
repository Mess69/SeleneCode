//! Property, field, and variable extraction (incl. the per-language
//! declarator shapes and store-initializer handling).

use selene_core::NodeKind;
use tree_sitter::Node;

use crate::Language;
use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::LanguageRules;

use super::{NodeExtra, Session, extract_decorators_for, extract_function, extract_name};

/// Class property (C# property_declaration and TS/JS #808 demotions).
///
/// Returns the created node's id — the TS/JS caller pushes it as a scope to
/// walk the field's initializer (TS `tree-sitter.ts:996-1006`); every other
/// caller discards it.
pub(super) fn extract_property(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) -> Option<String> {
    let name = rules.extract_property_name(node, s.source()).or_else(|| {
        get_child_by_field(node, "name")
            .or_else(|| get_child_by_field(node, "property"))
            .or_else(|| {
                let mut cursor = node.walk();
                node.named_children(&mut cursor)
                    .find(|c| c.kind() == "identifier")
            })
            .map(|n| get_node_text(n, s.source()).to_string())
    });
    let name = name?;

    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_static: rules.is_static(node, s.source()),
        ..NodeExtra::default()
    };
    let created = s.create_node(NodeKind::Property, &name, node, extra);
    // `@Inject() private svc: Foo` — decorator + type-annotation refs on
    // class properties too (Task 8).
    let id = created.and_then(|idx| s.id_of(idx))?;
    extract_decorators_for(s, node, &id);
    s.extract_type_annotations(rules, node, &id);
    Some(id)
}

/// Class field declarations (Java/C#/PHP shapes — Task 10/14). The generic
/// declarator scan only; language-specific wrappers land with their tasks.
pub(super) fn extract_field(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    let docstring = get_preceding_docstring(node, s.source());
    let visibility = rules.get_visibility(node, s.source());
    let is_static = rules.is_static(node, s.source());

    let mut cursor = node.walk();
    let declarators: Vec<Node<'_>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .collect();

    // PHP `private UserService $userService;` — the grammar wraps each property
    // in `property_element` → `variable_name` → `name`, NEVER a
    // `variable_declarator`, so the declarator loop below finds nothing and the
    // field node was silently dropped (tree-sitter.ts:2015-2042).
    if declarators.is_empty() {
        let mut c2 = node.walk();
        let prop_elements: Vec<Node<'_>> = node
            .named_children(&mut c2)
            .filter(|c| c.kind() == "property_element")
            .collect();
        if !prop_elements.is_empty() {
            // The type hint, if present: the first named child that is neither a
            // modifier nor a property_element (tree-sitter.ts:2020-2025).
            let mut c3 = node.walk();
            let type_text = node
                .named_children(&mut c3)
                .find(|c| {
                    !matches!(
                        c.kind(),
                        "visibility_modifier"
                            | "static_modifier"
                            | "readonly_modifier"
                            | "property_element"
                            | "var_modifier"
                    )
                })
                .map(|t| get_node_text(t, s.source()).to_string());

            for elem in prop_elements {
                let mut c4 = elem.walk();
                let var_name = elem
                    .named_children(&mut c4)
                    .find(|c| c.kind() == "variable_name");
                let name_node = var_name.and_then(|v| {
                    let mut c5 = v.walk();
                    v.named_children(&mut c5).find(|c| c.kind() == "name")
                });
                let Some(name_node) = name_node else { continue };
                let name = get_node_text(name_node, s.source()).to_string();
                let signature = Some(match &type_text {
                    Some(t) => format!("{t} ${name}"),
                    None => format!("${name}"),
                });
                let extra = NodeExtra {
                    docstring: docstring.clone(),
                    signature,
                    visibility,
                    is_static,
                    ..NodeExtra::default()
                };
                s.create_node(NodeKind::Field, &name, elem, extra);
            }
            // NOTE: TS returns here WITHOUT calling extractTypeAnnotations
            // (tree-sitter.ts:2040) — a PHP property's type hint is carried in
            // the node's `signature`, not emitted as a `references` ref. Adding
            // one would over-emit vs. the parity baseline.
            return;
        }
    }

    for d in declarators {
        let Some(name_node) = get_child_by_field(d, "name").or_else(|| d.named_child(0)) else {
            continue;
        };
        let name = get_node_text(name_node, s.source()).to_string();
        let extra = NodeExtra {
            docstring: docstring.clone(),
            visibility,
            is_static,
            ..NodeExtra::default()
        };
        let Some(idx) = s.create_node(NodeKind::Field, &name, d, extra) else {
            continue;
        };
        // The field's declared TYPE is a `references` dependency. The OUTER
        // declaration is the right scope to search from — the type sits beside
        // the declarators, not inside them (tree-sitter.ts:2077).
        if let Some(id) = s.id_of(idx) {
            s.extract_type_annotations(rules, node, &id);
        }
    }
}

/// Vue store collection key names (`ts_core.rs` owns the sets; this thin
/// check keeps the variable branch readable).
fn ts_core_is_store_collection_name(name: &str) -> bool {
    matches!(name, "actions" | "mutations" | "getters")
}

/// `= <first 100 chars>[...]` initializer signature (searchable context).
fn init_signature(s: &Session<'_>, value: Node<'_>) -> String {
    let init: String = get_node_text(value, s.source()).chars().take(100).collect();
    let ellipsis = if init.chars().count() >= 100 {
        "..."
    } else {
        ""
    };
    format!("= {init}{ellipsis}")
}

/// Top-level variable declarations. Task 7 lands the TS/JS declarator loop;
/// Python/Ruby `assignment` shape from Task 5; Go specs land with Task 9.
pub(super) fn extract_variable(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    let kind = if rules.is_const(node, s.source()).unwrap_or(false) {
        NodeKind::Constant
    } else {
        NodeKind::Variable
    };
    let docstring = get_preceding_docstring(node, s.source());

    // TS/JS: lexical_declaration / variable_declaration → declarator loop.
    if matches!(
        s.language(),
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx
    ) {
        let is_exported = rules.is_exported(node, s.source());
        let mut cursor = node.walk();
        let declarators: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        for child in declarators {
            let Some(name_node) = get_child_by_field(child, "name") else {
                continue;
            };
            let value_node = get_child_by_field(child, "value");
            // Skip destructured patterns (`let { x, y } = props()`) — ugly
            // multi-line names. EXCEPT RTK generated-hook destructures off a
            // bare-identifier RHS (`export const { useGetXQuery } = api`).
            if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
                if name_node.kind() == "object_pattern"
                    && value_node.is_some_and(|v| v.kind() == "identifier")
                {
                    s.extract_rtk_hook_bindings(name_node, is_exported);
                }
                continue;
            }
            let name = get_node_text(name_node, s.source()).to_string();
            // Arrow/function-expression values extract as FUNCTIONS (named
            // via the declarator by extract_function), never as variables.
            if let Some(value) = value_node
                && (value.kind() == "arrow_function" || value.kind() == "function_expression")
            {
                extract_function(rules, s, value);
                continue;
            }
            let signature = value_node.map(|v| init_signature(s, v));

            // React HOC-wrapped components (#841): PascalCase-gated so a
            // memoization util (`const cache = memo(fn)`) stays a constant.
            if let Some(value) = value_node
                && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && let Some(inner) = s.react_component_hoc(value)
            {
                let extra = NodeExtra {
                    docstring: docstring.clone(),
                    signature,
                    is_exported,
                    ..NodeExtra::default()
                };
                s.extract_react_component_node(rules, &name, child, inner, extra);
                continue;
            }

            let extra = NodeExtra {
                docstring: docstring.clone(),
                signature,
                is_exported,
                ..NodeExtra::default()
            };
            let created = s.create_node(kind, &name, child, extra);
            if let Some(id) = created.and_then(|idx| s.id_of(idx)) {
                s.extract_variable_type_annotation(child, &id);
            }

            // Store-shaped initializers (Task 8): exported object-of-
            // functions (SvelteKit actions, Zustand `create(...)` returns),
            // RTK createApi endpoints, Pinia setup stores, Vue store
            // collections — their nested function members become nodes;
            // otherwise the initializer body is walked for calls (object
            // literals excepted). NOTE (TS parity): no scope push — walk
            // attributes to the enclosing scope; only Go pushes (#693).
            let object_of_fns = match value_node {
                Some(v) if v.kind() == "object" || v.kind() == "object_expression" => Some(v),
                Some(v) if v.kind() == "call_expression" => {
                    s.find_initializer_returned_object(v, 0)
                }
                _ => None,
            };
            let has_inline_fns = object_of_fns.is_some_and(|o| s.object_has_inline_functions(o));
            let extract_object_methods =
                is_exported == Some(true) && object_of_fns.is_some() && has_inline_fns;

            let rtk_endpoints = value_node
                .filter(|v| v.kind() == "call_expression")
                .and_then(|v| s.find_rtk_endpoints_object(v));
            let pinia_setup = value_node
                .filter(|v| v.kind() == "call_expression")
                .and_then(|v| s.find_pinia_setup_fn(v));

            let mut store_collections: Vec<Node<'_>> = Vec::new();
            if let Some(v) = value_node
                && (v.kind() == "call_expression" || v.kind() == "new_expression")
            {
                store_collections.extend(s.find_vue_store_collection_objects(v));
            }
            if let Some(obj) = object_of_fns
                && !extract_object_methods
                && ts_core_is_store_collection_name(&name)
                && s.looks_like_vue_store_file()
            {
                store_collections.push(obj);
            }

            if let Some(value) = value_node
                && value.kind() != "object"
                && value.kind() != "object_expression"
                && !(extract_object_methods && value.kind() == "call_expression")
                && rtk_endpoints.is_none()
                && pinia_setup.is_none()
                && store_collections.is_empty()
            {
                s.visit_function_body(value, "");
            }
            if extract_object_methods && let Some(obj) = object_of_fns {
                s.extract_object_literal_functions(rules, obj);
            }
            if let Some(endpoints) = rtk_endpoints {
                s.extract_rtk_endpoints(rules, endpoints);
            }
            if let Some(setup) = pinia_setup {
                s.extract_pinia_setup_body(rules, setup);
            }
            for coll in store_collections {
                s.extract_object_literal_functions(rules, coll);
            }
        }
        return;
    }

    // C: a declaration's name nests inside declarator fields; only
    // file-scope init/pointer/array declarators are tracked (a BARE
    // identifier declarator is a macro-misparsed prototype — skipping it
    // costs only uninitialized scalar globals). Several declarators per
    // declaration (`int a = 1, b = 2;`) all extract.
    if s.language() == Language::C {
        if has_function_ancestor(node) {
            return;
        }
        let is_exported = rules.is_exported(node, s.source());
        let mut cursor = node.walk();
        let declarators: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|c| {
                matches!(
                    c.kind(),
                    "init_declarator" | "pointer_declarator" | "array_declarator"
                )
            })
            .collect();
        for child in declarators {
            let Some(name_node) = c_declarator_identifier(Some(child)) else {
                continue;
            };
            let name = get_node_text(name_node, s.source()).to_string();
            if name.is_empty() {
                continue;
            }
            let signature = (child.kind() == "init_declarator")
                .then(|| get_child_by_field(child, "value"))
                .flatten()
                .map(|v| init_signature(s, v));
            let extra = NodeExtra {
                docstring: docstring.clone(),
                signature,
                is_exported,
                ..NodeExtra::default()
            };
            s.create_node(kind, &name, child, extra);
        }
        return;
    }

    // C++ file-scope `Foo x;` — the TS GENERIC variable fallback
    // (tree-sitter.ts:2802-2818): scan the named children for a bare
    // `identifier` (or `variable_declarator`) declarator and mint it. A C++
    // `declaration`'s type sits in the `type` field, so the left/named_child(0)
    // shape below only ever sees the TYPE node and a global object
    // (`Foo gInstance;`, `static Registry reg;`) went unextracted entirely —
    // no node, so no impact-radius edges into it.
    //
    // Gated to C++ although TS runs this fallback for every language that has
    // no branch of its own (C++/Java/C#/Kotlin/PHP/Rust). The others cannot
    // reach here or must not: Rust/PHP/Kotlin/Go consume their variable kinds
    // in `visit_node` hooks first; Java/C# variable kinds are body-local
    // (their file-scope walk never sees one). Kotlin is the sharp edge — a
    // destructuring `val (a, b) = pair` falls THROUGH its hook to here, and
    // the unfiltered TS scan mints the RHS `pair` as a phantom variable
    // (verified against this tree). C++ is therefore the only language whose
    // shape the fallback actually governs for us.
    if s.language() == Language::Cpp {
        let is_exported = rules.is_exported(node, s.source());
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            if !matches!(child.kind(), "identifier" | "variable_declarator") {
                continue;
            }
            let name = if child.kind() == "identifier" {
                get_node_text(child, s.source()).to_string()
            } else {
                extract_name(rules, child, s.source())
            };
            if name.is_empty() || name == "<anonymous>" {
                continue;
            }
            let extra = NodeExtra {
                docstring: docstring.clone(),
                is_exported,
                ..NodeExtra::default()
            };
            s.create_node(kind, &name, child, extra);
        }
        return;
    }

    // Python/Ruby assignment: left = right.
    let left = get_child_by_field(node, "left").or_else(|| node.named_child(0));
    let right = get_child_by_field(node, "right").or_else(|| node.named_child(1));
    let Some(left) = left else { return };
    if left.kind() != "identifier" && left.kind() != "constant" {
        return;
    }
    let name = get_node_text(left, s.source()).to_string();
    let signature = right.map(|r| init_signature(s, r));
    let extra = NodeExtra {
        docstring,
        signature,
        ..NodeExtra::default()
    };
    s.create_node(kind, &name, node, extra);
}

/// `cDeclaratorIdentifier`: dig through declarator wrappers to the
/// identifier; a `function_declarator` is a prototype → `None`.
fn c_declarator_identifier<'t>(node: Option<Node<'t>>) -> Option<Node<'t>> {
    let mut cur = node;
    let mut guard = 0;
    while let Some(n) = cur {
        guard += 1;
        if guard > 12 {
            return None;
        }
        match n.kind() {
            "identifier" => return Some(n),
            "function_declarator" => return None,
            "init_declarator"
            | "pointer_declarator"
            | "array_declarator"
            | "parenthesized_declarator" => cur = get_child_by_field(n, "declarator"),
            _ => return None,
        }
    }
    None
}

/// Any `function_definition` ancestor? (C file-scope-only variable gate.)
fn has_function_ancestor(node: Node<'_>) -> bool {
    let mut p = node.parent();
    while let Some(n) = p {
        if n.kind() == "function_definition" {
            return true;
        }
        p = n.parent();
    }
    false
}
