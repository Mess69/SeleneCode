//! Go rules — verbatim port of `languages/go.ts` (map §Per-language
//! highlights, go row) plus the two Go-specific pieces the TS CORE carried
//! (`tree-sitter.ts`): grouped/single import-spec extraction and the
//! var/const/short-var declaration shapes. Both live in [`GoRules::visit_node`]
//! here rather than in the walker: the walker is owned by the parallel core
//! chain (its `INSERTION POINT (Task 9)` comments notwithstanding), and the
//! hook surface reaches everything these need ([`Session`] exposes
//! `create_node`/`add_unresolved`/`push_scope`/`visit_function_body`).
//! Observable output is identical to the TS core placement.
//!
//! Go has no classes: `classTypes` is empty, `methodsAreTopLevel: true`
//! (`func (r T) M()` is a top-level declaration that is still a method), and
//! `type Foo struct/interface {…}` arrives as a `type_spec` reclassified via
//! [`LanguageRules::resolve_type_alias_kind`]. Interface `type_spec`s are
//! instead fully handled in `visit_node`: the TS core extracted a Go
//! interface's method specs as `method` nodes under the interface node
//! (implicit satisfaction needs the contract's method set — Go has no
//! `implements` keyword), which the generic ladder cannot do.
//!
//! Struct/interface *embedding* (`extends` refs from `extractInheritance`)
//! is deliberately NOT here: the generic inheritance pass is Task 7 core
//! work; Go embedding refs light up when it lands (flagged in the Task 9
//! report).

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{EdgeKind, NodeKind};
use tree_sitter::Node;

use crate::UnresolvedReference;
use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::{LanguageRules, NodeTypeTables};
use crate::walker::{NodeExtra, Session};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["function_declaration"],
    // Go doesn't have classes; struct/interface arrive via type_spec.
    method_types: &["method_declaration"],
    type_alias_types: &["type_spec"],
    import_types: &["import_declaration"],
    call_types: &["call_expression"],
    variable_types: &[
        "var_declaration",
        "short_var_declaration",
        "const_declaration",
    ],
    methods_are_top_level: true,
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("result"),
    ..NodeTypeTables::EMPTY
};

/// Receiver-type capture: `(sl *Type)`, `(sl Type)`, `(*Type)`, `(Type)` and
/// generic receivers `(s *Stack[T])`. Anchored on the opening `(`, optional
/// receiver var name skipped (#583 — the old `name)`-anchored pattern never
/// matched a `[T])` suffix, orphaning generic-type methods).
static RECEIVER_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"\(\s*(?:[A-Za-z_]\w*\s+)?\*?\s*([A-Za-z_]\w*)").unwrap()
});

/// Bare-type-name gate shared by the return normalization.
static BARE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"^[A-Za-z_]\w*$").unwrap()
});
static GENERIC_ARGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"<[^>]*>").unwrap()
});
static BRACKET_ARGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"\[[^\]]*\]").unwrap()
});

pub(crate) struct GoRules;

impl LanguageRules for GoRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// A Go function's declared return type, normalized to the bare type a
    /// chained `New().Method()` could be called on (#645/#608): multi-return
    /// `(T, error)` takes the first result, `*Foo` unwraps, `<…>`/`[…]` args
    /// strip, qualified `pkg.Foo` reduces to `Foo`; the result must be a bare
    /// identifier.
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut result = get_child_by_field(node, "result")?;
        if result.kind() == "parameter_list" {
            let mut cursor = result.walk();
            let first = result
                .named_children(&mut cursor)
                .find(|c| c.kind() == "parameter_declaration")?;
            result = get_child_by_field(first, "type").unwrap_or(first);
        }
        if result.kind() == "pointer_type" {
            let mut cursor = result.walk();
            result = result
                .named_children(&mut cursor)
                .find(|c| {
                    matches!(
                        c.kind(),
                        "type_identifier" | "qualified_type" | "generic_type"
                    )
                })
                .unwrap_or(result);
        }
        let text = get_node_text(result, source).trim();
        let text = text.strip_prefix('*').unwrap_or(text);
        let text = GENERIC_ARGS_RE.replace_all(text, "");
        let text = BRACKET_ARGS_RE.replace_all(&text, "");
        let last = text.rsplit('.').next()?.trim();
        if last.is_empty() || !BARE_NAME_RE.is_match(last) {
            return None;
        }
        Some(last.to_string())
    }

    /// `(params) (results)` — the plain space join is the TS spelling.
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let mut sig = get_node_text(params, source).to_string();
        if let Some(result) = get_child_by_field(node, "result") {
            sig.push(' ');
            sig.push_str(get_node_text(result, source));
        }
        Some(sig)
    }

    /// `type Foo struct {…}` / `type Bar interface {…}` — the inner node
    /// sits in the type_spec's `type` field. (Interface type_specs are fully
    /// handled by `visit_node` before the ladder consults this; the hook
    /// still answers for them for completeness.)
    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        let type_child = get_child_by_field(node, "type")?;
        match type_child.kind() {
            "struct_type" => Some(NodeKind::Struct),
            "interface_type" => Some(NodeKind::Interface),
            _ => None,
        }
    }

    /// Go: exported iff the identifier's first char is A–Z (byte compare,
    /// exactly the TS charCode 65..=90 check).
    fn is_exported(&self, node: Node<'_>, source: &str) -> Option<bool> {
        let name_node = get_child_by_field(node, "name")?;
        let text = get_node_text(name_node, source);
        Some(matches!(text.as_bytes().first(), Some(b'A'..=b'Z')))
    }

    /// Receiver type from the `receiver` field text via [`RECEIVER_RE`].
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let receiver = get_child_by_field(node, "receiver")?;
        let text = get_node_text(receiver, source);
        RECEIVER_RE
            .captures(text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// The TS-core Go branches, hook-hosted (module docs): grouped/single
    /// import specs, var/const/short-var declarations, and interface
    /// `type_spec`s (method specs become `method` nodes under the interface).
    fn visit_node(&self, node: Node<'_>, session: &mut Session<'_>) -> bool {
        match node.kind() {
            "import_declaration" => {
                extract_go_imports(node, session);
                true
            }
            "var_declaration" | "const_declaration" | "short_var_declaration" => {
                extract_go_variables(node, session);
                true
            }
            "type_spec" => {
                let is_interface =
                    get_child_by_field(node, "type").is_some_and(|t| t.kind() == "interface_type");
                if !is_interface {
                    return false; // struct/alias: the ladder handles it.
                }
                extract_go_interface(node, session);
                true
            }
            _ => false,
        }
    }
}

/// One import node + one `imports` ref per import spec (single or grouped) —
/// the TS core's Go import branch. Path = the interpreted string literal with
/// quotes stripped; the node anchors at the SPEC (its line), signature = the
/// spec's text.
fn extract_go_imports(node: Node<'_>, s: &mut Session<'_>) {
    let parent_id = s.node_stack().last().cloned();

    let mut specs: Vec<Node<'_>> = Vec::new();
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in &children {
        match child.kind() {
            "import_spec_list" => {
                let mut c2 = child.walk();
                specs.extend(
                    child
                        .named_children(&mut c2)
                        .filter(|c| c.kind() == "import_spec"),
                );
            }
            "import_spec" => specs.push(*child),
            _ => {}
        }
    }

    for spec in specs {
        let mut c = spec.walk();
        let Some(lit) = spec
            .named_children(&mut c)
            .find(|n| n.kind() == "interpreted_string_literal")
        else {
            continue;
        };
        let import_path = get_node_text(lit, s.source()).replace(['\'', '"'], "");
        if import_path.is_empty() {
            continue;
        }
        let signature = get_node_text(spec, s.source()).trim().to_string();
        s.create_node(
            NodeKind::Import,
            &import_path,
            spec,
            NodeExtra {
                signature: Some(signature),
                ..NodeExtra::default()
            },
        );
        if let Some(pid) = &parent_id {
            s.add_unresolved(UnresolvedReference {
                from_node_id: pid.clone(),
                reference_name: import_path,
                reference_kind: EdgeKind::Imports.as_str().to_string(),
                line: Some(u32::try_from(spec.start_position().row).unwrap_or(0) + 1),
                column: Some(u32::try_from(spec.start_position().column).unwrap_or(0)),
                file_path: None,
                language: None,
            });
        }
    }
}

/// `= <first 100 chars>` initializer signature (the shared TS spelling).
fn init_signature(value: Node<'_>, source: &str) -> String {
    let init: String = get_node_text(value, source).chars().take(100).collect();
    let ellipsis = if init.chars().count() >= 100 {
        "..."
    } else {
        ""
    };
    format!("= {init}{ellipsis}")
}

/// The TS core's Go variable branch: one node per `var_spec`/`const_spec`
/// (first identifier only; `const_declaration` ⇒ constant), initializers
/// walked as a body scoped to the declared symbol (#693 — a cobra `RunE:
/// func(){…}` handler attributes to the var, not the file); plus the
/// `short_var_declaration` `expression_list` shape.
fn extract_go_variables(node: Node<'_>, s: &mut Session<'_>) {
    let docstring = get_preceding_docstring(node, s.source());
    let spec_kind = if node.kind() == "const_declaration" {
        NodeKind::Constant
    } else {
        NodeKind::Variable
    };

    let mut cursor = node.walk();
    let specs: Vec<Node<'_>> = node
        .named_children(&mut cursor)
        .filter(|c| matches!(c.kind(), "var_spec" | "const_spec"))
        .collect();
    for spec in specs {
        let mut created_id: Option<String> = None;
        if let Some(name_node) = spec.named_child(0)
            && name_node.kind() == "identifier"
        {
            let name = get_node_text(name_node, s.source()).to_string();
            let value_node = if spec.named_child_count() > 1 {
                spec.named_child(u32::try_from(spec.named_child_count() - 1).unwrap_or(0))
            } else {
                None
            };
            let idx = s.create_node(
                spec_kind,
                &name,
                spec,
                NodeExtra {
                    docstring: docstring.clone(),
                    signature: value_node.map(|v| init_signature(v, s.source())),
                    ..NodeExtra::default()
                },
            );
            created_id = idx.and_then(|i| s.nodes().get(i).map(|n| n.id.clone()));
        }
        // Walk the initializer scoped to the declared symbol (#693), so a
        // call inside an anonymous func initializer — a cobra `RunE` handler,
        // a callback closure — attributes to the var, not the file.
        if let Some(value_field) = get_child_by_field(spec, "value") {
            if let Some(id) = &created_id {
                s.push_scope(id.clone());
            }
            let fn_id = created_id.clone().unwrap_or_default();
            s.visit_function_body(value_field, &fn_id);
            if created_id.is_some() {
                s.pop_scope();
            }
        }
    }

    if node.kind() == "short_var_declaration"
        && let Some(left) = get_child_by_field(node, "left")
    {
        let right = get_child_by_field(node, "right");
        let idents: Vec<Node<'_>> = if left.kind() == "expression_list" {
            let mut c = left.walk();
            left.named_children(&mut c)
                .filter(|n| n.kind() == "identifier")
                .collect()
        } else {
            vec![left]
        };
        for id in idents {
            let name = get_node_text(id, s.source()).to_string();
            s.create_node(
                NodeKind::Variable,
                &name,
                node,
                NodeExtra {
                    docstring: docstring.clone(),
                    signature: right.map(|r| init_signature(r, s.source())),
                    ..NodeExtra::default()
                },
            );
        }
    }
}

/// A Go interface `type_spec`: the interface node plus its method specs as
/// `method` nodes (the TS core's `extractGoInterfaceMethods` — implicit
/// interface satisfaction matches a struct's method set against these).
fn extract_go_interface(node: Node<'_>, s: &mut Session<'_>) {
    let Some(name_node) = get_child_by_field(node, "name") else {
        return;
    };
    let name = get_node_text(name_node, s.source()).to_string();
    let rules: &'static GoRules = &GoRules;
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(NodeKind::Interface, &name, node, extra) else {
        return;
    };
    let Some(iface_id) = s.nodes().get(idx).map(|n| n.id.clone()) else {
        return;
    };

    let Some(interface_type) = get_child_by_field(node, "type") else {
        return;
    };
    s.push_scope(iface_id);
    let mut cursor = interface_type.walk();
    let members: Vec<Node<'_>> = interface_type
        .named_children(&mut cursor)
        .filter(|m| matches!(m.kind(), "method_elem" | "method_spec"))
        .collect();
    for m in members {
        let name_node = get_child_by_field(m, "name").or_else(|| m.named_child(0));
        let Some(name_node) = name_node else { continue };
        let mname = get_node_text(name_node, s.source()).to_string();
        if mname.is_empty() {
            continue;
        }
        let extra = NodeExtra {
            signature: rules.get_signature(m, s.source()),
            ..NodeExtra::default()
        };
        s.create_node(NodeKind::Method, &mname, m, extra);
    }
    s.pop_scope();
}
