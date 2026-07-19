//! The inheritance pass (`extends`/`implements` refs) and type-alias
//! extraction (incl. the TS alias-member and tuple-contract surfacing).

use selene_core::{EdgeKind, NodeKind};
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::LanguageRules;
use crate::rules::cpp_preparse::strip_cpp_template_args;
use crate::{Language, UnresolvedReference};

use super::{NodeExtra, Session, extract_enum_members, extract_name, resolve_body, ts_core, visit};

/// One `extends`/`implements` ref named by `target`'s own text.
fn push_inheritance_ref(s: &mut Session<'_>, class_id: &str, target: Node<'_>, kind: EdgeKind) {
    let name = get_node_text(target, s.source()).trim().to_string();
    push_inheritance_ref_named(s, class_id, name, target, kind);
}

/// One `extends`/`implements` ref with an explicit name, positioned at `target`
/// (the C++ arm rewrites the name to strip template args).
fn push_inheritance_ref_named(
    s: &mut Session<'_>,
    class_id: &str,
    name: String,
    target: Node<'_>,
    kind: EdgeKind,
) {
    if name.is_empty() {
        return;
    }
    s.add_unresolved(UnresolvedReference {
        from_node_id: class_id.to_string(),
        reference_name: name,
        reference_kind: kind.as_str().to_string(),
        line: Some(u32::try_from(target.start_position().row).unwrap_or(0) + 1),
        column: Some(u32::try_from(target.start_position().column).unwrap_or(0)),
        file_path: None,
        language: None,
    });
}

/// The supertype named by a Kotlin `delegation_specifier`.
///
/// `class Foo : Bar` → `user_type` → the type name. `class Foo : Bar()` (the
/// base's constructor invoked) wraps it one level deeper:
/// `constructor_invocation` → `user_type` → the type name
/// (tree-sitter.ts:5462-5479).
///
/// **Grammar drift (Kotlin ledger).** We link `tree-sitter-kotlin-ng`; TS ran the
/// older `tree-sitter-kotlin`. Two shapes differ, and both are handled:
/// - the specifiers sit under a plural `delegation_specifiers` WRAPPER (the
///   caller recurses into it), not as direct children of `class_declaration`;
/// - a `user_type`'s name leaf is an `identifier`, not a `type_identifier`.
///
/// Falls back to the widest node available rather than dropping the supertype.
fn kotlin_delegation_target<'t>(child: Node<'t>) -> Option<Node<'t>> {
    let mut c = child.walk();
    let specifiers: Vec<Node<'t>> = child.named_children(&mut c).collect();
    let user_type = specifiers.iter().find(|c| c.kind() == "user_type");
    let ctor_invocation = specifiers
        .iter()
        .find(|c| c.kind() == "constructor_invocation");
    let target = user_type.or(ctor_invocation)?;

    let inner_user_type = if target.kind() == "user_type" {
        *target
    } else {
        let mut c2 = target.walk();
        target
            .named_children(&mut c2)
            .find(|c| c.kind() == "user_type")
            .unwrap_or(*target)
    };
    let mut c3 = inner_user_type.walk();
    Some(
        inner_user_type
            .named_children(&mut c3)
            .find(|c| matches!(c.kind(), "type_identifier" | "identifier"))
            .unwrap_or(inner_user_type),
    )
}

/// The node carrying a C# base type's NAME.
///
/// A `generic_name` (`ClientBase<T>`) unwraps to its `identifier` head, so the
/// ref matches the bare class the generic was declared as
/// (tree-sitter.ts:5446-5448).
///
/// **Deliberate, count-identical divergence from TS.** A record's
/// `primary_constructor_base_type` (`record D(int A) : Base(A)`) unwraps to its
/// type head too. TS takes the node's RAW TEXT and emits the literal `Base(A)` —
/// primary-constructor argument list included — a name that can never resolve to
/// the `Base` class node. The Global Constraints' *"silent beats wrong"* rule
/// forbids porting a malformed name, and the parity gate compares COUNTS per ref
/// kind, so emitting the correct `Base` holds `refs.extends` at parity while
/// producing a ref that actually resolves (task-19 report §4, BUG 4).
fn csharp_base_type_name_node<'t>(base: Node<'t>) -> Node<'t> {
    match base.kind() {
        "generic_name" | "primary_constructor_base_type" => get_child_by_field(base, "type")
            .or_else(|| {
                let mut c = base.walk();
                base.named_children(&mut c)
                    .find(|n| n.kind() == "identifier")
            })
            .unwrap_or(base),
        _ => base,
    }
}

/// The supertypes named by an `extends`/`implements` clause.
///
/// Java wraps multiples in a `type_list` (`super_interfaces` → `type_list` →
/// `type_identifier`); everything else lists them directly. `single` picks the
/// TS fallback when there is no `type_list`: the `extends`-family clauses take
/// only `namedChild(0)` (a class has ONE superclass), while the
/// `implements`-family takes every named child (tree-sitter.ts:5261-5262 vs
/// :5310-5311).
fn inheritance_targets<'t>(clause: Node<'t>, single: bool) -> Vec<Node<'t>> {
    let mut c = clause.walk();
    if let Some(type_list) = clause
        .named_children(&mut c)
        .find(|n| n.kind() == "type_list")
    {
        let mut c2 = type_list.walk();
        return type_list.named_children(&mut c2).collect();
    }
    if single {
        return clause.named_child(0).into_iter().collect();
    }
    let mut c3 = clause.walk();
    clause.named_children(&mut c3).collect()
}

/// `extends` / `implements` refs from a type declaration's inheritance clauses —
/// the core `extractInheritance` pass (tree-sitter.ts:5156-5549).
///
/// Every arm the v0 languages need is wired, and each is pinned by a parity
/// fixture (`tests/fixtures/parity/*/inherit.*`).
///
/// Two arms are excluded on purpose, both proven by the corpus:
/// - **Rust `trait_bounds`** (ts:5380) — [`crate::rules::rust_lang`] already owns
///   supertrait refs (and `impl Trait for Type` → `implements`). A second emit
///   here would DOUBLE-COUNT: `rust/inherit.rs` is at exact parity with this pass
///   NOT handling `trait_bounds`, which is the proof.
/// - **`enum` base lists** (ts:1873) — C#'s `enum E : byte` names a *storage
///   type*, not a supertype, and TS emits `extends:byte`. That is a false
///   inheritance edge ("silent beats wrong"), so [`extract_enum`] never calls
///   this pass.
///
/// Non-v0 arms in the TS source (Scala `with`-mixins, Dart mixins, VB.NET
/// `Inherits`/`Implements` statements, Objective-C `class_interface`, Swift
/// `inheritance_specifier`, CFML `component_attribute`) are not ported — those
/// languages are not in v0.
///
/// # Gating
///
/// Most arms match on the child's node kind alone and are **ungated by design**
/// (TS-faithful): no v0 grammar reuses `extends_clause` / `superclass` /
/// `base_clause` / `extends_interfaces` / `constraint_elem` / `type_elem` with
/// different semantics — collision-checked across the wave. Exactly two arms
/// need a guard and have one: Python's `argument_list` is gated on the OWNER
/// kind (`class_definition`, so a *call*'s arguments can never be read as base
/// classes), and Go's `field_declaration` is gated on the language — ungated it
/// reproduces TS's own phantom-base bug, because C++ spells member declarations
/// `field_declaration` too and nests the member name inside its declarator, so
/// TS reads the member's TYPE as an embedded base (see
/// `tests/fixtures/parity/deviations.toml`). **A wave-2 language must re-check
/// this collision set before reusing any of the ungated kinds.**
pub(super) fn extract_inheritance(s: &mut Session<'_>, node: Node<'_>, class_id: &str) {
    let node_kind = node.kind();
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        match child.kind() {
            // The `extends` family, all one shape (tree-sitter.ts:5198-5274):
            //   TS      `class C extends Base`      → extends_clause
            //   Java    `class C extends Base`      → superclass
            //   Ruby    `class C < Base`            → superclass
            //   PHP     `class C extends Base`      → base_clause
            //   Java    `interface I extends A, B`  → extends_interfaces (type_list)
            // A class has exactly one superclass, so the no-type_list fallback
            // takes namedChild(0) — but Java's `extends_interfaces` DOES carry a
            // type_list, and an interface may extend many.
            "extends_clause" | "superclass" | "base_clause" | "extends_interfaces" => {
                for target in inheritance_targets(child, true) {
                    push_inheritance_ref(s, class_id, target, EdgeKind::Extends);
                }
            }

            // The `implements` family (tree-sitter.ts:5302-5324):
            //   TS    `implements Serializable`               → implements_clause
            //   PHP   `implements Serializable, JsonSerial…`  → class_interface_clause
            //   Java  `implements A, B`                       → super_interfaces (type_list)
            // Every named child is an interface — no namedChild(0) fallback.
            "implements_clause" | "class_interface_clause" | "super_interfaces" => {
                for target in inheritance_targets(child, false) {
                    push_inheritance_ref(s, class_id, target, EdgeKind::Implements);
                }
            }

            // C++ `class Derived : public Base, private Other` — the clause holds
            // access specifiers AND base types; take only the type nodes. A
            // templated base (`Base<int>`, `ns::Tpl<int>`) arrives as a
            // `template_type` / `qualified_identifier`; strip the `<…>` args so
            // the ref matches the bare class the template was declared as, rather
            // than never resolving (tree-sitter.ts:5284-5300, #1043).
            "base_class_clause" => {
                let mut c2 = child.walk();
                let bases: Vec<Node<'_>> = child
                    .named_children(&mut c2)
                    .filter(|t| {
                        matches!(
                            t.kind(),
                            "type_identifier" | "qualified_identifier" | "template_type"
                        )
                    })
                    .collect();
                for base in bases {
                    let raw = get_node_text(base, s.source());
                    let name = strip_cpp_template_args(raw).into_owned();
                    push_inheritance_ref_named(s, class_id, name, base, EdgeKind::Extends);
                }
            }

            // C# `class Movie : BaseItem, IPlugin` — `base_list` merges the base
            // class AND the interfaces into one colon-separated list, and the
            // syntax does not distinguish them, so TS emits every entry as
            // `extends` (tree-sitter.ts:5439-5458).
            "base_list" => {
                let mut c2 = child.walk();
                let bases: Vec<Node<'_>> = child.named_children(&mut c2).collect();
                for base in bases {
                    let target = csharp_base_type_name_node(base);
                    push_inheritance_ref(s, class_id, target, EdgeKind::Extends);
                }
            }

            // Kotlin `class Foo : Bar, Baz` / `class Foo : Bar()` — the supertype
            // is a `user_type`, or a `constructor_invocation` wrapping one when the
            // base's constructor is called (tree-sitter.ts:5460-5480).
            "delegation_specifier" => {
                if let Some(target) = kotlin_delegation_target(child) {
                    push_inheritance_ref(s, class_id, target, EdgeKind::Extends);
                }
            }

            // Python `class Child(Base, Mixin):` — the superclass list is an
            // `argument_list` of identifiers (`attribute` for a dotted
            // `module.Base`). Gated on `class_definition` so a CALL's argument
            // list can never be mistaken for a base list (tree-sitter.ts:5326-5341).
            "argument_list" if node_kind == "class_definition" => {
                let mut c2 = child.walk();
                let args: Vec<Node<'_>> = child
                    .named_children(&mut c2)
                    .filter(|a| matches!(a.kind(), "identifier" | "attribute"))
                    .collect();
                for arg in args {
                    push_inheritance_ref(s, class_id, arg, EdgeKind::Extends);
                }
            }

            // Go interface embedding — `type ReadCloser interface { Reader; Closer }`
            // (tree-sitter.ts:5343-5357). The embedded interface arrives wrapped in
            // a `constraint_elem`; newer tree-sitter-go spells the same shape
            // `type_elem`, so both are accepted.
            "constraint_elem" | "type_elem" => {
                let mut c2 = child.walk();
                if let Some(type_id) = child
                    .named_children(&mut c2)
                    .find(|c| c.kind() == "type_identifier")
                {
                    push_inheritance_ref(s, class_id, type_id, EdgeKind::Extends);
                }
            }

            // Go struct embedding — `type DB struct { *Head; Queryable }`. An
            // embedded field has NO `field_identifier`: the type IS the name. A
            // named field (`Name string`) has one, and must not be read as a
            // supertype (tree-sitter.ts:5359-5376).
            //
            // GATED TO GO — and this gate is the fix for a real TS bug. TS does
            // NOT gate this arm, and C++ spells a member declaration
            // `field_declaration` too: `class Factory { public: static Widget
            // create(); };` has no `field_identifier` (the name lives inside the
            // `function_declarator`), so TS's ungated arm reads the RETURN TYPE as
            // an embedded base and emits a phantom `extends:Widget` from a class
            // with no base clause at all. That is the exact false positive pinned
            // in `deviations.toml` for `cpp/f.cpp` — this is its root cause, and
            // the language gate is why we do not reproduce it. "Silent beats
            // wrong" (Global Constraints).
            "field_declaration" if s.language() == Language::Go => {
                let mut c2 = child.walk();
                let has_field_identifier = child
                    .named_children(&mut c2)
                    .any(|c| c.kind() == "field_identifier");
                if !has_field_identifier {
                    let mut c3 = child.walk();
                    if let Some(type_id) = child
                        .named_children(&mut c3)
                        .find(|c| c.kind() == "type_identifier")
                    {
                        push_inheritance_ref(s, class_id, type_id, EdgeKind::Extends);
                    }
                }
            }

            // JavaScript `class Foo extends Bar {}` — `class_heritage` holds a BARE
            // identifier, with no `extends_clause` wrapper (that is the TS-grammar
            // shape). Reached through the recursion arm below, where `node` IS the
            // `class_heritage` (tree-sitter.ts:5499-5513).
            "identifier" | "type_identifier" if node_kind == "class_heritage" => {
                push_inheritance_ref(s, class_id, child, EdgeKind::Extends);
            }

            // Recurse into the containers that WRAP the clauses rather than being
            // one: TS/JS `class_heritage` (holds extends_clause/implements_clause,
            // or a bare identifier), and Go's `field_declaration_list` inside a
            // `struct_type` (tree-sitter.ts:5515-5519). Kotlin's plural
            // `delegation_specifiers` is the same idea — a wrapper the -ng grammar
            // adds that TS's grammar did not (see `kotlin_delegation_target`).
            "field_declaration_list" | "class_heritage" | "delegation_specifiers" => {
                extract_inheritance(s, child, class_id);
            }

            _ => {}
        }
    }
}

/// The TS `findChildByTypes` (tree-sitter.ts:1347-1353): the first named
/// child whose kind is in `types`.
fn find_child_by_types<'t>(node: Node<'t>, types: &[&str]) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| types.contains(&c.kind()))
}

/// Type alias (`type X = ...`). Returns the TS `skipChildren` bool: `true`
/// when the alias was reclassified and its body walked; `false` for a plain
/// alias (the walker then recurses into the alias value, matching TS).
pub(super) fn extract_type_alias(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) -> bool {
    let name = extract_name(rules, node, s.source());
    if name == "<anonymous>" {
        return false;
    }
    let kind = rules
        .resolve_type_alias_kind(node, s.source())
        .unwrap_or(NodeKind::TypeAlias);
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };

    // Reclassified struct/enum (tree-sitter.ts:2840-2859 / 2861-2885): the
    // alias WRAPS the definition — Go `type Foo struct {…}` (type_spec →
    // struct_type) and, ubiquitously, the C/C++ header idiom
    // `typedef struct {…} Foo;` / `typedef enum {…} Status;`, whose inner
    // specifier is ANONYMOUS. TS creates the node from the TYPEDEF name,
    // pushes it, walks the inner specifier's body underneath, and returns
    // true (skip children). Skipping children is load-bearing: let the
    // generic ladder reach the inner `struct_specifier`/`enum_specifier` and
    // it mints a phantom `<anonymous>` node and hangs the members off it
    // (`<anonymous>::OK` — a QN no call site or FTS query can ever match).
    if matches!(kind, NodeKind::Struct | NodeKind::Enum) {
        let Some(id) = s
            .create_node(kind, &name, node, extra)
            .and_then(|i| s.id_of(i))
        else {
            return true; // TS: `if (!structNode) return true`
        };
        let id_for_inheritance = id.clone();
        s.push_scope(id);
        let t = rules.tables();
        if kind == NodeKind::Struct {
            // Go-style `type` field first, then the inner struct child (C
            // typedef struct).
            if let Some(type_child) = get_child_by_field(node, "type")
                .or_else(|| find_child_by_types(node, t.struct_types))
            {
                // Go struct embedding — `type DB struct { *Head; Queryable }`.
                // The clauses hang off the INNER `struct_type`, not the alias, so
                // this runs on `type_child` (tree-sitter.ts:2850).
                extract_inheritance(s, type_child, &id_for_inheritance);
                let body = get_child_by_field(type_child, t.body_field).unwrap_or(type_child);
                let mut cursor = body.walk();
                let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
                for child in children {
                    visit(rules, s, child);
                }
            }
        } else if let Some(inner_enum) = find_child_by_types(node, t.enum_types)
            && let Some(body) = resolve_body(rules, inner_enum)
        {
            // Enum members go through the enum-member path so `enumerator`
            // children become `enum_member` nodes under the alias.
            let mut cursor = body.walk();
            let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
            for child in children {
                if t.enum_member_types.contains(&child.kind()) {
                    extract_enum_members(s, child);
                } else {
                    visit(rules, s, child);
                }
            }
        }
        s.pop_scope();
        return true;
    }

    // Plain alias (and the `interface` reclassification, which no v0 language
    // reaches through this path — Go's `visit_node` consumes interface
    // `type_spec`s before the ladder gets here).
    let Some(idx) = s.create_node(kind, &name, node, extra) else {
        return false;
    };

    // Type refs from the alias value (`type X = ITextModel | null`) +
    // TS/TSX alias-member surfacing: `type X = { foo(): T }` members become
    // property/method nodes with `TypeAlias::member` QNs (#359), and
    // string-literal contract names in generic tuples become searchable
    // method nodes (#634).
    if let (Some(alias_id), Some(value)) = (s.id_of(idx), get_child_by_field(node, "value")) {
        if ts_core::is_type_annotation_language(s.language()) {
            s.extract_type_refs_from_subtree(value, &alias_id);
        }
        if matches!(s.language(), Language::Typescript | Language::Tsx) {
            let alias_name = name.clone();
            extract_ts_type_alias_members(rules, s, value, &alias_id, &alias_name);
            extract_ts_tuple_contract_names(s, value, &alias_id, &alias_name);
        }
    }
    false
}

/// #359: surface `type X = { foo: T; bar(): U }` (or intersection) members
/// as property/method nodes under the alias. Only the immediate
/// object_type / intersection operands — nested anonymous object types
/// inside generic args yield no phantom members. A `foo: () => T`
/// function-typed property counts as a method.
fn extract_ts_type_alias_members(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    value: Node<'_>,
    alias_id: &str,
    alias_name: &str,
) {
    let mut object_types: Vec<Node<'_>> = Vec::new();
    if value.kind() == "object_type" {
        object_types.push(value);
    } else if value.kind() == "intersection_type" {
        let mut cursor = value.walk();
        object_types.extend(
            value
                .named_children(&mut cursor)
                .filter(|op| op.kind() == "object_type"),
        );
    } else {
        return;
    }

    s.push_scope(alias_id.to_string());
    for obj in object_types {
        let mut cursor = obj.walk();
        let members: Vec<Node<'_>> = obj
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "property_signature" || c.kind() == "method_signature")
            .collect();
        for child in members {
            let Some(name_node) = get_child_by_field(child, "name") else {
                continue;
            };
            let member_name = get_node_text(name_node, s.source()).to_string();
            if member_name.is_empty() {
                continue;
            }
            let member_kind =
                if child.kind() == "method_signature" || is_ts_function_typed_property(child) {
                    NodeKind::Method
                } else {
                    NodeKind::Property
                };
            let extra = NodeExtra {
                docstring: get_preceding_docstring(child, s.source()),
                signature: Some(get_node_text(child, s.source()).to_string()),
                qualified_name: Some(format!("{alias_name}::{member_name}")),
                ..NodeExtra::default()
            };
            s.create_node(member_kind, &member_name, child, extra);
            // Type refs from the member's signature attach to the ALIAS
            // (consistent with interface-member treatment, #432 — Task 8).
            let alias_owned = alias_id.to_string();
            s.extract_type_annotations(rules, child, &alias_owned);
        }
    }
    s.pop_scope();
}

/// `foo: () => T` — a property_signature whose type annotation contains a
/// `function_type` is method-shaped (`obj.foo()` ≡ `bar(): T`).
fn is_ts_function_typed_property(property_signature: Node<'_>) -> bool {
    let Some(type_anno) = get_child_by_field(property_signature, "type") else {
        return false;
    };
    let mut cursor = type_anno.walk();
    type_anno
        .named_children(&mut cursor)
        .any(|inner| inner.kind() == "function_type")
}

/// #634: string-literal contract names in a generic tuple type alias
/// (`type L = [Service<'query_apply_record', …>, …]`) become searchable
/// `method` nodes under the alias. Deliberately narrow: only a string
/// literal that is a DIRECT type argument of a `generic_type` that is
/// itself a DIRECT tuple element; names must be valid identifiers.
fn extract_ts_tuple_contract_names(
    s: &mut Session<'_>,
    value: Node<'_>,
    alias_id: &str,
    alias_name: &str,
) {
    fn collect_tuples<'t>(n: Node<'t>, depth: u32, out: &mut Vec<Node<'t>>) {
        if depth > 6 {
            return; // a type expression is shallow; cap defensively
        }
        if n.kind() == "tuple_type" {
            out.push(n);
        }
        let mut cursor = n.walk();
        let children: Vec<Node<'t>> = n.named_children(&mut cursor).collect();
        for c in children {
            collect_tuples(c, depth + 1, out);
        }
    }
    let mut tuples = Vec::new();
    collect_tuples(value, 0, &mut tuples);
    if tuples.is_empty() {
        return;
    }

    fn is_valid_ident(name: &str) -> bool {
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        (first.is_ascii_alphabetic() || first == '_' || first == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    }

    s.push_scope(alias_id.to_string());
    for tuple in tuples {
        let mut cursor = tuple.walk();
        let entries: Vec<Node<'_>> = tuple
            .named_children(&mut cursor)
            .filter(|e| e.kind() == "generic_type")
            .collect();
        for entry in entries {
            let Some(type_args) = get_child_by_field(entry, "type_arguments") else {
                continue;
            };
            let mut c2 = type_args.walk();
            let literals: Vec<Node<'_>> = type_args
                .named_children(&mut c2)
                .filter(|a| a.kind() == "literal_type")
                .collect();
            for arg in literals {
                let Some(str_node) = arg.named_child(0) else {
                    continue;
                };
                if str_node.kind() != "string" {
                    continue;
                }
                let name: String = get_node_text(str_node, s.source())
                    .trim()
                    .trim_matches(['\'', '"', '`'])
                    .to_string();
                if !is_valid_ident(&name) {
                    continue;
                }
                let signature: String = get_node_text(entry, s.source())
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(120)
                    .collect();
                let extra = NodeExtra {
                    signature: Some(signature),
                    qualified_name: Some(format!("{alias_name}::{name}")),
                    ..NodeExtra::default()
                };
                s.create_node(NodeKind::Method, &name, entry, extra);
            }
        }
    }
    s.pop_scope();
}
