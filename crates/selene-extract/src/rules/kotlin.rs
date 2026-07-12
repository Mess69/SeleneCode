//! Kotlin rules — port of `languages/kotlin.ts` (map §kotlin row) onto the
//! **kotlin-ng** grammar (different lineage than the WASM grammar TS used —
//! Task 1 spike + this task's probes; every adaptation carries a
//! `// kotlin-ng:` comment per the drift protocol).
//!
//! ## kotlin-ng drift ledger (all probe-verified on 1.1.0)
//!
//! - Identifiers are `identifier`, NOT `simple_identifier` (spike-pinned),
//!   and — beyond the map/rider — declarations carry a REAL `name` FIELD
//!   (`(class_declaration name: (identifier) …)`), so `name_field: "name"`
//!   resolves directly; the map's positional `simple_identifier` hunt is
//!   unnecessary. `type_alias` is the exception: its name sits in the
//!   `type` field → [`LanguageRules::resolve_name`].
//! - `fun interface` parses **CLEAN** (a `class_declaration` with `fun` +
//!   `interface` keyword children; no ERROR node — spike-pinned by
//!   `kotlin_fun_interface_probe`). The TS 2-pattern ERROR-node recovery
//!   (and its ERROR-preferring `resolveBody` special case) is **DROPPED
//!   entirely** — `classify_class_node` sees the `interface` keyword and the
//!   ladder does the rest. (For the parity gate's deviations ledger: nested
//!   `fun interface` members now nest properly instead of surfacing as
//!   siblings "due to grammar limitations".)
//! - Imports: the statement node is `import` (was `import_header`) and the
//!   dotted path is a `qualified_identifier` (was a dotted `identifier`);
//!   the import NAME joins the `identifier` children with `.` so a wildcard
//!   `.*` or `as Alias` tail never leaks into the name.
//! - Packages: `package_header` holds a `qualified_identifier` (was
//!   `identifier`).
//! - Properties: `val`/`var` are direct keyword children (was
//!   `binding_pattern_kind` with text), the name/type nest under
//!   `variable_declaration` (`identifier`, optional `user_type`/
//!   `nullable_type`), and `enum class`'s keyword hides inside
//!   `modifiers → class_modifier` — enum classification keys on the
//!   `enum_class_body` child instead.
//! - kotlin-ng wants a newline/semicolon after the last member of a
//!   single-line body (`class A { fun m() }` yields MISSING
//!   `_class_member_semi`) — fixtures use multiline bodies; the spike's
//!   `has_error()` hardening covers the MISSING case.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{NodeKind, Visibility};
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::{ClassKind, ImportInfo, LanguageRules, NodeTypeTables};
use crate::walker::{NodeExtra, Session};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["function_declaration"],
    class_types: &["class_declaration"],
    // Methods are functions inside classes.
    method_types: &["function_declaration"],
    // Interfaces + enums are class_declarations — classify_class_node.
    enum_member_types: &["enum_entry"],
    type_alias_types: &["type_alias"],
    // kotlin-ng: the import statement node is `import` (was `import_header`).
    import_types: &["import"],
    call_types: &["call_expression"],
    variable_types: &["property_declaration"],
    field_types: &["property_declaration"],
    extra_class_node_types: &["object_declaration"],
    package_types: &["package_header"],
    // kotlin-ng: declarations expose a real `name` field (module docs) —
    // supersedes the map's field-less `simple_identifier` positional hunt.
    name_field: "name",
    body_field: "function_body",
    params_field: "function_value_parameters",
    return_field: Some("type"),
    ..NodeTypeTables::EMPTY
};

/// Kotlin return types that can't be a chained-call receiver.
const KOTLIN_NON_CLASS_RETURN: [&str; 2] = ["Unit", "Nothing"];

static BARE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"^[A-Za-z_]\w*$").unwrap()
});

/// The declaration's `modifiers` child text, or `""`.
fn modifier_text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers")
        .map(|m| get_node_text(m, source))
        .unwrap_or("")
}

/// The positional return-type node: the first `user_type`/`nullable_type`
/// AFTER `function_value_parameters` (an extension receiver's type sits
/// before the params, so it's never mistaken); reaching the body or a
/// `where`-clause first means no declared return type.
fn positional_return_type<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut seen_params = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_value_parameters" => seen_params = true,
            "function_body" | "type_constraints" if seen_params => return None,
            "user_type" | "nullable_type" if seen_params => return Some(child),
            _ => {}
        }
    }
    None
}

pub(crate) struct KotlinRules;

impl LanguageRules for KotlinRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// kotlin-ng: a `type_alias`'s name identifier sits in the `type` field
    /// (`(type_alias type: (identifier) …)`), not `name`.
    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() != "type_alias" {
            return None;
        }
        let name = get_child_by_field(node, "type")?;
        Some(get_node_text(name, source).to_string())
    }

    /// Positional return type normalized to the bare class name (#645/#608):
    /// `nullable_type` unwraps to its `user_type`, the type's base
    /// `identifier` is taken (kotlin-ng: was `type_identifier`), and
    /// `Unit`/`Nothing` are rejected.
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let rt = positional_return_type(node)?;
        let ut = if rt.kind() == "nullable_type" {
            let mut c = rt.walk();
            rt.named_children(&mut c)
                .find(|c| c.kind() == "user_type")
                .unwrap_or(rt)
        } else {
            rt
        };
        // kotlin-ng: user_type's base name is an `identifier` child.
        let mut c2 = ut.walk();
        let type_id = ut
            .named_children(&mut c2)
            .find(|c| c.kind() == "identifier");
        let name = get_node_text(type_id.unwrap_or(ut), source)
            .trim()
            .to_string();
        if name.is_empty()
            || !BARE_NAME_RE.is_match(&name)
            || KOTLIN_NON_CLASS_RETURN.contains(&name.as_str())
        {
            return None;
        }
        Some(name)
    }

    /// `(params): ReturnType` — params found by KIND (kotlin-ng exposes no
    /// params field), return type positionally, full text (keeps `?`).
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        let params = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "function_value_parameters")?;
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = positional_return_type(node) {
            sig.push_str(": ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    /// kotlin-ng: bodies are found by KIND — `function_body` for functions,
    /// `class_body` for classes/objects/interfaces, `enum_class_body` for
    /// enums. (The TS ERROR-preferring special case is dropped — module
    /// docs.)
    fn resolve_body<'t>(&self, node: Node<'t>, _body_field: &str) -> Option<Node<'t>> {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|c| matches!(c.kind(), "function_body" | "class_body" | "enum_class_body"))
    }

    /// Kotlin reuses `class_declaration` for classes, interfaces and enums:
    /// an `interface` keyword child ⇒ interface (covers `fun interface` —
    /// kotlin-ng parses it clean); an `enum_class_body` child ⇒ enum
    /// (kotlin-ng: the `enum` keyword nests inside `modifiers →
    /// class_modifier`, so the body kind is the reliable signal); else class.
    fn classify_class_node(&self, node: Node<'_>, _source: &str) -> Option<ClassKind> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "interface" {
                return Some(ClassKind::Interface);
            }
            if child.kind() == "enum" || child.kind() == "enum_class_body" {
                return Some(ClassKind::Enum);
            }
        }
        Some(ClassKind::Class)
    }

    /// Extension functions: `fun Type.method()` — the `user_type` before the
    /// `.` is the receiver; its base `identifier` is the type name
    /// (kotlin-ng: was `type_identifier`). Hitting the name identifier or
    /// the params first means no receiver.
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut found_user_type: Option<Node<'_>> = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "user_type" => found_user_type = Some(child),
                "." => {
                    if let Some(ut) = found_user_type {
                        let mut c2 = ut.walk();
                        let type_id = ut
                            .named_children(&mut c2)
                            .find(|c| c.kind() == "identifier");
                        return Some(get_node_text(type_id.unwrap_or(ut), source).to_string());
                    }
                }
                // kotlin-ng: the function's own name is an `identifier`
                // (field `name`) — past it (or the params), no receiver.
                "identifier" | "function_value_parameters" => break,
                _ => {}
            }
        }
        None
    }

    /// Default Public (the Kotlin default); modifiers text decides.
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let text = modifier_text(node, source);
        Some(if text.contains("public") {
            Visibility::Public
        } else if text.contains("private") {
            Visibility::Private
        } else if text.contains("protected") {
            Visibility::Protected
        } else if text.contains("internal") {
            Visibility::Internal
        } else {
            Visibility::Public
        })
    }

    /// Kotlin has no `static` (companion objects instead).
    fn is_static(&self, _node: Node<'_>, _source: &str) -> Option<bool> {
        Some(false)
    }

    /// `suspend` marks coroutines — the async analogue.
    fn is_async(&self, node: Node<'_>, source: &str) -> Option<bool> {
        Some(modifier_text(node, source).contains("suspend"))
    }

    /// Kotlin Multiplatform `expect`/`actual` markers: `modifiers →
    /// platform_modifier → (expect | actual)`. AST-matched (not raw text) so
    /// an identifier named "actual" can't false-positive; captured onto
    /// `Node.decorators` so the resolver can link expect ↔ actual.
    fn extract_modifiers(&self, node: Node<'_>, _source: &str) -> Vec<String> {
        let mut mods = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() != "modifiers" {
                continue;
            }
            let mut c2 = child.walk();
            for pm in child.children(&mut c2) {
                if pm.kind() != "platform_modifier" {
                    continue;
                }
                let mut c3 = pm.walk();
                for kw in pm.children(&mut c3) {
                    if matches!(kw.kind(), "expect" | "actual") {
                        mods.push(kw.kind().to_string());
                    }
                }
            }
        }
        mods
    }

    /// kotlin-ng: `import a.b.C [as D]` / `import a.b.*` — the dotted path
    /// is a `qualified_identifier`; the NAME joins its `identifier` children
    /// with `.` so neither a `.*` tail nor an `as` alias leaks in (the alias
    /// identifier is a sibling of the qualified_identifier).
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let mut cursor = node.walk();
        let qid = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "qualified_identifier")?;
        let mut c2 = qid.walk();
        let segments: Vec<&str> = qid
            .named_children(&mut c2)
            .filter(|c| c.kind() == "identifier")
            .map(|c| get_node_text(c, source))
            .collect();
        if segments.is_empty() {
            return None;
        }
        Some(ImportInfo {
            module_name: segments.join("."),
            signature: get_node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }

    /// `package_header` → `qualified_identifier` (kotlin-ng; single-segment
    /// packages arrive as a bare `identifier`).
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        let id = node
            .named_children(&mut cursor)
            .find(|c| matches!(c.kind(), "qualified_identifier" | "identifier"))?;
        Some(get_node_text(id, source).trim().to_string())
    }

    /// Kotlin properties (`val`/`var`/`const val`) — the name nests
    /// `property_declaration → variable_declaration → identifier`, which the
    /// generic variable/field paths can't read. Kind by enclosing scope:
    /// function body / lambda / init / getter / setter ⇒ local (skipped);
    /// `companion object` / `object` / top level ⇒ shared values —
    /// `val`→constant, `var`→variable (the Scala-object rule); a
    /// `class`/`interface`/`enum` instance property ⇒ `field`. Destructuring
    /// (`val (a, b) = …` — a `multi_variable_declaration`) falls through to
    /// the ladder's defaults, which extract nothing (TS parity).
    fn visit_node(&self, node: Node<'_>, session: &mut Session<'_>) -> bool {
        if node.kind() != "property_declaration" {
            return false;
        }
        extract_kotlin_property(node, session)
    }
}

fn extract_kotlin_property(node: Node<'_>, s: &mut Session<'_>) -> bool {
    let mut cursor = node.walk();
    let Some(var_decl) = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "variable_declaration")
    else {
        return false; // destructuring etc. — leave to the ladder's defaults
    };
    // kotlin-ng: the property name is `identifier` (was simple_identifier).
    let mut c2 = var_decl.walk();
    let Some(name_node) = var_decl
        .named_children(&mut c2)
        .find(|c| c.kind() == "identifier")
    else {
        return false;
    };
    let name = get_node_text(name_node, s.source()).to_string();
    if name.is_empty() {
        return false;
    }

    // Walk to the nearest enclosing definition. kotlin-ng wraps statement
    // lists in plain `block` nodes (if/while bodies included) — the walk
    // passes through them to the owning construct.
    #[derive(PartialEq)]
    enum Scope {
        Local,
        Const,
        Instance,
    }
    let mut scope = Scope::Const;
    let mut parent = node.parent();
    while let Some(p) = parent {
        match p.kind() {
            "function_body"
            | "function_declaration"
            | "lambda_literal"
            | "anonymous_initializer"
            | "control_structure_body"
            | "getter"
            | "setter" => {
                scope = Scope::Local;
                break;
            }
            "companion_object" | "object_declaration" => {
                scope = Scope::Const;
                break;
            }
            "class_declaration" => {
                scope = Scope::Instance;
                break;
            }
            _ => parent = p.parent(),
        }
    }
    if scope == Scope::Local {
        return true; // a local — don't extract
    }

    // kotlin-ng: `val`/`var` are direct keyword children (was a
    // binding_pattern_kind node with text).
    let mut c3 = node.walk();
    let is_val = node.children(&mut c3).any(|c| c.kind() == "val");
    let kind = if scope == Scope::Instance {
        NodeKind::Field
    } else if is_val {
        NodeKind::Constant
    } else {
        NodeKind::Variable
    };

    // kotlin-ng: the declared type nests inside variable_declaration
    // (`identifier : user_type`), not in a property-level `type` field.
    let mut c4 = var_decl.walk();
    let type_node = var_decl
        .named_children(&mut c4)
        .find(|c| matches!(c.kind(), "user_type" | "nullable_type"));
    let signature = type_node.map(|t| {
        format!(
            "{} {}: {}",
            if is_val { "val" } else { "var" },
            name,
            get_node_text(t, s.source())
        )
    });

    s.create_node(
        &KotlinRules,
        kind,
        &name,
        node,
        NodeExtra {
            signature,
            ..NodeExtra::default()
        },
    );
    true
}
