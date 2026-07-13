//! TS/JS core machinery (Task 8): React HOC components, exported
//! object-of-functions / store collections (Zustand, RTK Query, Pinia,
//! Vuex), import-binding + re-export refs, and the type-annotation
//! `references` pass. All shapes are keyed on AST structure (or the
//! specific framework entry-point names the TS lineage keyed on), never on
//! arbitrary library detection.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{EdgeKind, NodeKind};
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::LanguageRules;
use crate::walker::{NodeExtra, Session};
use crate::{Language, UnresolvedReference};

/// RTK Query generated-hook naming convention.
static RTK_HOOK_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"^use[A-Z][A-Za-z0-9]*(?:Query|Mutation)$").unwrap()
});

/// React HOC wrappers whose result is itself a component (#841).
const REACT_COMPONENT_HOCS: [&str; 4] = ["forwardRef", "memo", "React.forwardRef", "React.memo"];

const VUE_STORE_COLLECTION_NAMES: [&str; 3] = ["actions", "mutations", "getters"];
const VUE_STORE_FACTORY_CALLEES: [&str; 2] = ["defineStore", "createStore"];

/// Distinct signals that a file is a Vuex/Pinia store (≥2 ⇒ treat a bare
/// `const actions = {…}` as a store collection).
static VUE_STORE_FILE_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"\bdefineStore\b|\bcreateStore\b|\bVuex\b|\bmutations\b|\bactions\b|\bgetters\b|\bnamespaced\b")
        .unwrap()
});

/// `TYPE_ANNOTATION_LANGUAGES ∩ v0`. C# and PHP have their own dispatch
/// paths inside [`Session::extract_type_annotations`] — neither grammar
/// produces the `type_identifier` leaf the generic subtree walk keys on.
pub(super) fn is_type_annotation_language(l: Language) -> bool {
    matches!(
        l,
        Language::Typescript
            | Language::Tsx
            | Language::Arkts
            | Language::Kotlin
            | Language::Rust
            | Language::Go
            | Language::Java
            | Language::CSharp
            | Language::Php
    )
}

/// Built-in/primitive type names that never create references — ported
/// VERBATIM from tree-sitter.ts `BUILTIN_TYPES` (TS/JS + Rust + Java/C# +
/// Go + Scala sets; duplicates collapse in the match).
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // TS/JS
        "string" | "number" | "boolean" | "void" | "null" | "undefined" | "never" | "any"
            | "unknown" | "object" | "symbol" | "bigint" | "true" | "false"
            // Rust
            | "str" | "bool" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16"
            | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "char"
            // Java/C#
            | "int" | "long" | "short" | "byte" | "float" | "double"
            // Go
            | "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
            | "float32" | "float64" | "complex64" | "complex128" | "rune" | "error"
            // Scala (capitalized primitives + ubiquitous stdlib aliases)
            | "Int" | "Long" | "Short" | "Byte" | "Float" | "Double" | "Boolean" | "Char"
            | "Unit" | "String" | "Any" | "AnyRef" | "AnyVal" | "Nothing" | "Null"
    )
}

/// PHP type-position wrappers — a type-hint arrives as one of these, never
/// as a bare `type_identifier`. Ported VERBATIM from `PHP_TYPE_NODES`
/// (tree-sitter.ts:309-313).
fn is_php_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "named_type"
            | "optional_type"
            | "nullable_type"
            | "union_type"
            | "intersection_type"
            | "disjunctive_normal_form_type"
            | "primitive_type"
    )
}

/// PHP pseudo-types that never create a reference (`self`, `static`, the
/// scalar hints). Ported VERBATIM from `PHP_PSEUDO_TYPES`
/// (tree-sitter.ts:5625-5628).
fn is_php_pseudo_type(name: &str) -> bool {
    matches!(
        name,
        "self"
            | "static"
            | "parent"
            | "mixed"
            | "object"
            | "iterable"
            | "callable"
            | "void"
            | "null"
            | "false"
            | "true"
            | "never"
            | "array"
            | "int"
            | "float"
            | "string"
            | "bool"
    )
}

impl Session<'_> {
    // =========================================================================
    // Type-annotation references
    // =========================================================================

    /// Type refs from a function/method/property/field node's annotations
    /// (parameter types, return type, direct `type_annotation`) → `references`
    /// UnresolvedReferences.
    ///
    /// C# and PHP dispatch to their own paths first: neither grammar produces
    /// the `type_identifier` leaf the generic subtree walk keys on, so the
    /// generic path emits nothing for them (tree-sitter.ts:5653-5677).
    pub(crate) fn extract_type_annotations(
        &mut self,
        rules: &'static dyn LanguageRules,
        node: Node<'_>,
        node_id: &str,
    ) {
        if !is_type_annotation_language(self.language()) {
            return;
        }

        // C# tree-sitter produces no `type_identifier` leaf — it uses
        // `identifier` / `predefined_type` / `qualified_name` / `generic_name` —
        // so the generic subtree walk below emits ZERO refs for it. Dispatch to
        // a C#-aware path that descends only KNOWN type positions, so parameter
        // NAMES never surface as type refs (tree-sitter.ts:5663-5666, #381).
        if self.language() == Language::CSharp {
            self.extract_csharp_type_refs(node, node_id);
            return;
        }

        // PHP type-hints are `named_type`/`optional_type`/`union_type` wrapping
        // a `name`/`qualified_name` — never `type_identifier` — so the generic
        // walk emits nothing. Dispatch to a PHP-aware path over type positions
        // only, so a `variable_name` like `$events` can't mis-emit as a ref
        // (tree-sitter.ts:5674-5677).
        if self.language() == Language::Php {
            self.extract_php_type_refs(node, node_id);
            return;
        }

        if let Some(params) = get_child_by_field(node, rules.tables().params_field) {
            self.extract_type_refs_from_subtree(params, node_id);
        }
        let return_field = rules.tables().return_field.unwrap_or("return_type");
        if let Some(ret) = get_child_by_field(node, return_field) {
            self.extract_type_refs_from_subtree(ret, node_id);
        }
        let mut cursor = node.walk();
        let type_annotation = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = type_annotation {
            self.extract_type_refs_from_subtree(ta, node_id);
        }
    }

    /// Local/variable `type_annotation` child → type refs (TS `: Type`).
    pub(super) fn extract_variable_type_annotation(&mut self, node: Node<'_>, node_id: &str) {
        if !is_type_annotation_language(self.language()) {
            return;
        }
        let mut cursor = node.walk();
        let ta = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "type_annotation");
        if let Some(ta) = ta {
            self.extract_type_refs_from_subtree(ta, node_id);
        }
    }

    /// Every `type_identifier` leaf in the subtree (unions, intersections,
    /// generics, arrays), builtins filtered, → `references` refs.
    pub(super) fn extract_type_refs_from_subtree(&mut self, node: Node<'_>, from_id: &str) {
        if node.kind() == "type_identifier" {
            let type_name = get_node_text(node, self.source());
            if !type_name.is_empty() && !is_builtin_type(type_name) {
                self.add_unresolved(UnresolvedReference {
                    from_node_id: from_id.to_string(),
                    reference_name: type_name.to_string(),
                    reference_kind: EdgeKind::References.as_str().to_string(),
                    line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
                    column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
                    file_path: None,
                    language: None,
                });
            }
            return; // leaf
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            self.extract_type_refs_from_subtree(child, from_id);
        }
    }

    /// One `references` ref at `node`'s position — the shared tail of every
    /// type-position emit below.
    fn push_type_ref(&mut self, name: String, node: Node<'_>, from_id: &str) {
        self.add_unresolved(UnresolvedReference {
            from_node_id: from_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::References.as_str().to_string(),
            line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }

    /// Type refs from a C# node that OWNS a type position — a method /
    /// constructor / property / field declaration. Walks only into the known
    /// type fields, so a parameter name (`request` in `Build(UserDto request)`)
    /// is never mis-emitted as a type ref. Port of `extractCsharpTypeRefs`
    /// (tree-sitter.ts:5758-5790, #381).
    pub(crate) fn extract_csharp_type_refs(&mut self, node: Node<'_>, node_id: &str) {
        // A property's type is under `type`; a method/ctor's RETURN type is
        // under `returns` (tree-sitter-c-sharp 0.23.x). A node carries only one
        // of the two, so checking both covers each without conflating them
        // (tree-sitter.ts:5763-5764).
        if let Some(direct) =
            get_child_by_field(node, "type").or_else(|| get_child_by_field(node, "returns"))
        {
            self.walk_csharp_type_position(direct, node_id);
        }

        // A `field_declaration` has no `type` field of its own — it wraps its
        // declarators in a `variable_declaration` that carries it, so descend
        // one level (tree-sitter.ts:5766-5774).
        let mut cursor = node.walk();
        let var_decl = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "variable_declaration");
        if let Some(vd) = var_decl
            && let Some(vd_type) = get_child_by_field(vd, "type")
        {
            self.walk_csharp_type_position(vd_type, node_id);
        }

        // Method / constructor parameters: walk ONLY each `parameter`'s `type`
        // field — walking the parameter itself would emit its NAME as a type
        // ref (tree-sitter.ts:5776-5789).
        if let Some(params) = get_child_by_field(node, "parameters") {
            let mut c2 = params.walk();
            let types: Vec<Node<'_>> = params
                .named_children(&mut c2)
                .filter(|c| c.kind() == "parameter")
                .filter_map(|p| get_child_by_field(p, "type"))
                .collect();
            for t in types {
                self.walk_csharp_type_position(t, node_id);
            }
        }
    }

    /// The dependencies declared by a C# PRIMARY CONSTRUCTOR —
    /// `class Svc(IRepo repo) { … }` (C# 12+) and EVERY positional record
    /// (`record GenericRec<T>(T Value)`). The parameter list hangs off the type
    /// declaration as an unnamed-field `parameter_list` child, not the
    /// `parameters` field a method uses, so it is found by node type. Port of
    /// `extractCsharpPrimaryCtorParamRefs` (tree-sitter.ts:5803-5813, #237).
    pub(super) fn extract_csharp_primary_ctor_param_refs(
        &mut self,
        node: Node<'_>,
        owner_id: &str,
    ) {
        if self.language() != Language::CSharp {
            return;
        }
        let mut cursor = node.walk();
        let Some(param_list) = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "parameter_list")
        else {
            return;
        };
        let mut c2 = param_list.walk();
        let types: Vec<Node<'_>> = param_list
            .named_children(&mut c2)
            .filter(|c| c.kind() == "parameter")
            .filter_map(|p| get_child_by_field(p, "type"))
            .collect();
        for t in types {
            self.walk_csharp_type_position(t, owner_id);
        }
    }

    /// Walk a C# subtree KNOWN to be in a type position (return / parameter /
    /// property / field type, generic argument). Identifiers reached here are
    /// type names, not parameter names — the callers gate that. Port of
    /// `walkCsharpTypePosition` (tree-sitter.ts:5820-5877).
    fn walk_csharp_type_position(&mut self, node: Node<'_>, from_id: &str) {
        match node.kind() {
            // int/string/bool/… — never a project ref (tree-sitter.ts:5822).
            "predefined_type" => {}

            // Bare type name: `Foo` in `Foo bar`, or the `Foo` inside
            // `List<Foo>` (tree-sitter.ts:5825-5836).
            "identifier" => {
                let name = get_node_text(node, self.source());
                if !name.is_empty() && !is_builtin_type(name) {
                    let name = name.to_string();
                    self.push_type_ref(name, node, from_id);
                }
            }

            // `Namespace.Foo` → the rightmost identifier is the type; the
            // resolver matches on the trailing simple name
            // (tree-sitter.ts:5842-5854).
            "qualified_name" => {
                let text = get_node_text(node, self.source());
                let last = text.rsplit('.').next().unwrap_or(text);
                if !last.is_empty() && !is_builtin_type(last) {
                    let last = last.to_string();
                    self.push_type_ref(last, node, from_id);
                }
            }

            // `(int Code, Foo Payload)` — a tuple element has BOTH a `type` and
            // a `name` field; descending into all named children would emit the
            // element NAME (`Code`) as a type ref (tree-sitter.ts:5861-5865).
            "tuple_element" => {
                if let Some(t) = get_child_by_field(node, "type") {
                    self.walk_csharp_type_position(t, from_id);
                }
            }

            // Composite type nodes — `generic_name`, `nullable_type`,
            // `array_type`, `pointer_type`, `tuple_type`, `ref_type`, and any
            // newer wrapper the grammar adds (tree-sitter.ts:5867-5876).
            _ => {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                for child in children {
                    self.walk_csharp_type_position(child, from_id);
                }
            }
        }
    }

    /// Type refs from a PHP method / function / property declaration. Walks
    /// ONLY type positions: each parameter's type child (inside
    /// `formal_parameters`), the return type, and a property's type — all
    /// direct children of the declaration. Parameter and property NAMES are
    /// `variable_name` (`$x`), never type nodes, so they cannot be mis-emitted.
    /// Port of `extractPhpTypeRefs` (tree-sitter.ts:5887-5902).
    fn extract_php_type_refs(&mut self, node: Node<'_>, node_id: &str) {
        let mut cursor = node.walk();
        let params = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "formal_parameters");

        // simple_parameter / property_promotion_parameter / variadic_parameter
        // each carry their type as a direct child (tree-sitter.ts:5890-5895).
        let mut types: Vec<Node<'_>> = Vec::new();
        if let Some(params) = params {
            let mut c2 = params.walk();
            for p in params.named_children(&mut c2) {
                let mut c3 = p.walk();
                types.extend(
                    p.named_children(&mut c3)
                        .filter(|c| is_php_type_node(c.kind())),
                );
            }
        }

        // The return type (method/function) and a property's type are TYPE
        // nodes that are DIRECT children of the declaration
        // (tree-sitter.ts:5897-5901).
        let mut c4 = node.walk();
        types.extend(
            node.named_children(&mut c4)
                .filter(|c| is_php_type_node(c.kind())),
        );

        for t in types {
            self.walk_php_type_position(t, node_id);
        }
    }

    /// Walk a PHP subtree KNOWN to be in a type position; emit class/interface
    /// refs. Port of `walkPhpTypePosition` (tree-sitter.ts:5905-5934).
    fn walk_php_type_position(&mut self, node: Node<'_>, from_id: &str) {
        match node.kind() {
            // int/string/void/… (tree-sitter.ts:5906).
            "primitive_type" => {}

            "name" => {
                let name = get_node_text(node, self.source());
                if !name.is_empty() && !is_php_pseudo_type(name) {
                    let name = name.to_string();
                    self.push_type_ref(name, node, from_id);
                }
            }

            // `App\Contracts\Logger` → the trailing simple name: what the class
            // node is stored as, and what a `use` import brings into scope
            // (tree-sitter.ts:5917-5928).
            "qualified_name" => {
                let text = get_node_text(node, self.source());
                let last = text.rsplit('\\').next().unwrap_or("");
                if !last.is_empty() && !is_php_pseudo_type(last) {
                    let last = last.to_string();
                    self.push_type_ref(last, node, from_id);
                }
            }

            // optional_type / nullable_type / union_type / intersection_type /
            // named_type → recurse (tree-sitter.ts:5929-5933).
            _ => {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                for child in children {
                    self.walk_php_type_position(child, from_id);
                }
            }
        }
    }

    // =========================================================================
    // Import-binding + re-export refs
    // =========================================================================

    /// One `imports` ref per LOCAL binding of an ES import (default named,
    /// `{A, B as C}` aliases, `* as NS`) — imported-but-not-called symbols
    /// still record a cross-file dependency.
    pub(super) fn emit_import_binding_refs(&mut self, node: Node<'_>, from_id: &str) {
        let mut cursor = node.walk();
        let clause = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "import_clause");
        let Some(clause) = clause else {
            return; // side-effect import (`import './x'`)
        };

        let mut c2 = clause.walk();
        let children: Vec<Node<'_>> = clause.named_children(&mut c2).collect();
        for child in children {
            match child.kind() {
                "identifier" => self.push_import_ref_at(child, from_id),
                "named_imports" => {
                    let mut c3 = child.walk();
                    let specs: Vec<Node<'_>> = child.named_children(&mut c3).collect();
                    for spec in specs {
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let name = get_child_by_field(spec, "alias")
                            .or_else(|| get_child_by_field(spec, "name"))
                            .or_else(|| spec.named_child(0));
                        if let Some(n) = name {
                            self.push_import_ref_at(n, from_id);
                        }
                    }
                }
                "namespace_import" => {
                    let mut c3 = child.walk();
                    let ns = child
                        .named_children(&mut c3)
                        .find(|c| c.kind() == "identifier")
                        .or_else(|| child.named_child(0));
                    if let Some(n) = ns {
                        self.push_import_ref_at(n, from_id);
                    }
                }
                _ => {}
            }
        }
    }

    /// One `imports` ref per re-exported SOURCE-side binding of
    /// `export { A, B as C } from './y'` (barrels). `export * from` and
    /// `default` skipped (nothing name-matchable).
    pub(super) fn emit_re_export_refs(&mut self, node: Node<'_>, from_id: &str) {
        let mut cursor = node.walk();
        let clause = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "export_clause");
        let Some(clause) = clause else { return };
        let mut c2 = clause.walk();
        let specs: Vec<Node<'_>> = clause.named_children(&mut c2).collect();
        for spec in specs {
            if spec.kind() != "export_specifier" {
                continue;
            }
            let Some(name_node) = get_child_by_field(spec, "name").or_else(|| spec.named_child(0))
            else {
                continue;
            };
            let name = get_node_text(name_node, self.source());
            if name.is_empty() || name == "default" {
                continue;
            }
            self.push_import_ref_at(name_node, from_id);
        }
    }

    fn push_import_ref_at(&mut self, name_node: Node<'_>, from_id: &str) {
        let name = get_node_text(name_node, self.source()).to_string();
        if name.is_empty() {
            return;
        }
        self.add_unresolved(UnresolvedReference {
            from_node_id: from_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::Imports.as_str().to_string(),
            line: Some(u32::try_from(name_node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(name_node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }

    // =========================================================================
    // React HOC components (#841)
    // =========================================================================

    /// A component declared via an HOC wrapper: `forwardRef(...)`, `memo(...)`,
    /// `React.forwardRef/memo(...)`, `styled.tag\`…\`` / `styled(Base)\`…\``.
    /// `Some(inner)` = recognized, with the inline render fn when present;
    /// `None` = not a component wrapper.
    pub(super) fn react_component_hoc<'t>(&self, value: Node<'t>) -> Option<Option<Node<'t>>> {
        if value.kind() != "call_expression" {
            return None;
        }
        let callee = get_child_by_field(value, "function")?;
        let callee_text = get_node_text(callee, self.source());
        // styled-components / emotion (`\b` via the char check — `styledFoo`
        // must not match).
        if callee_text == "styled"
            || (callee_text.starts_with("styled")
                && callee_text[6..].starts_with(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
        {
            return Some(None); // no inline render fn (the arg is CSS)
        }
        if !REACT_COMPONENT_HOCS.contains(&callee_text) {
            return None;
        }
        let mut inner = None;
        if let Some(args) = get_child_by_field(value, "arguments") {
            let mut cursor = args.walk();
            inner = args
                .named_children(&mut cursor)
                .find(|a| a.kind() == "arrow_function" || a.kind() == "function_expression");
        }
        Some(inner)
    }

    /// Emit the `component` node for an HOC-wrapped declaration, walking the
    /// inline render fn's body (when present) so hooks/helpers attribute to
    /// the component like a plain arrow component.
    pub(super) fn extract_react_component_node(
        &mut self,
        rules: &'static dyn LanguageRules,
        name: &str,
        declarator: Node<'_>,
        inner_fn: Option<Node<'_>>,
        extra: NodeExtra,
    ) {
        let Some(idx) = self.create_node(NodeKind::Component, name, declarator, extra) else {
            return;
        };
        let Some(id) = self.nodes().get(idx).map(|n| n.id.clone()) else {
            return;
        };
        let Some(inner) = inner_fn else { return };
        let body = rules
            .resolve_body(inner, rules.tables().body_field)
            .or_else(|| get_child_by_field(inner, rules.tables().body_field));
        if let Some(body) = body {
            self.push_scope(id.clone());
            self.visit_function_body(body, &id);
            self.pop_scope();
        }
    }

    // =========================================================================
    // Object-of-functions / store collections
    // =========================================================================

    /// Property-key text with surrounding quotes stripped.
    pub(super) fn object_key_name(&self, key: Node<'_>) -> String {
        get_node_text(key, self.source())
            .trim_matches(['\'', '"', '`'])
            .to_string()
    }

    /// Each function-valued member of an object literal becomes a function
    /// node named by its key (pair arrows + method shorthand).
    pub(super) fn extract_object_literal_functions(
        &mut self,
        rules: &'static dyn LanguageRules,
        obj: Node<'_>,
    ) {
        let mut cursor = obj.walk();
        let members: Vec<Node<'_>> = obj.named_children(&mut cursor).collect();
        for member in members {
            if member.kind() == "pair" {
                let key = get_child_by_field(member, "key");
                let value = get_child_by_field(member, "value");
                if let (Some(key), Some(value)) = (key, value)
                    && (value.kind() == "arrow_function" || value.kind() == "function_expression")
                {
                    let name = self.object_key_name(key);
                    super::extract_function_named(rules, self, value, Some(&name));
                }
            } else if member.kind() == "method_definition" {
                // Shorthand `{ fetchUser() {…} }` — route through the
                // function extractor with an explicit name.
                if let Some(key) = get_child_by_field(member, "name") {
                    let name = self.object_key_name(key);
                    super::extract_function_named(rules, self, member, Some(&name));
                }
            }
        }
    }

    /// The object literal RETURNED by a function argument of a call
    /// initializer, unwrapping middleware call layers (Zustand
    /// `create(persist((set) => ({…}), {…}))`).
    pub(super) fn find_initializer_returned_object<'t>(
        &self,
        call: Node<'t>,
        depth: u32,
    ) -> Option<Node<'t>> {
        if depth > 4 {
            return None;
        }
        let args = get_child_by_field(call, "arguments")?;
        let mut cursor = args.walk();
        let children: Vec<Node<'t>> = args.named_children(&mut cursor).collect();
        for arg in children {
            match arg.kind() {
                "arrow_function" | "function_expression" => {
                    if let Some(obj) = self.function_returned_object(arg) {
                        return Some(obj);
                    }
                }
                "call_expression" => {
                    if let Some(obj) = self.find_initializer_returned_object(arg, depth + 1) {
                        return Some(obj);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The object a function returns: `=> ({…})` (parenthesized) or a
    /// `return {…}` in a block body.
    pub(super) fn function_returned_object<'t>(&self, fn_node: Node<'t>) -> Option<Node<'t>> {
        fn as_object<'t>(n: Node<'t>) -> Option<Node<'t>> {
            if n.kind() == "object" || n.kind() == "object_expression" {
                return Some(n);
            }
            if n.kind() == "parenthesized_expression" {
                let mut cursor = n.walk();
                let children: Vec<Node<'t>> = n.named_children(&mut cursor).collect();
                for inner in children {
                    if let Some(o) = as_object(inner) {
                        return Some(o);
                    }
                }
            }
            None
        }
        let body = get_child_by_field(fn_node, "body")?;
        if let Some(direct) = as_object(body) {
            return Some(direct);
        }
        if body.kind() == "statement_block" {
            let mut cursor = body.walk();
            let stmts: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
            for stmt in stmts {
                if stmt.kind() != "return_statement" {
                    continue;
                }
                let mut c2 = stmt.walk();
                let rets: Vec<Node<'t>> = stmt.named_children(&mut c2).collect();
                for r in rets {
                    if let Some(o) = as_object(r) {
                        return Some(o);
                    }
                }
            }
        }
        None
    }

    /// ≥1 inline function member — distinguishes an inline action map from
    /// a Pinia setup store's all-shorthand `return { foo, bar }`.
    pub(super) fn object_has_inline_functions(&self, obj: Node<'_>) -> bool {
        let mut cursor = obj.walk();
        let members: Vec<Node<'_>> = obj.named_children(&mut cursor).collect();
        for member in members {
            if member.kind() == "method_definition" {
                return true;
            }
            if member.kind() == "pair"
                && get_child_by_field(member, "value").is_some_and(|v| {
                    v.kind() == "arrow_function" || v.kind() == "function_expression"
                })
            {
                return true;
            }
        }
        false
    }

    // ---- RTK Query -----------------------------------------------------

    /// The endpoints object of `createApi({ endpoints: build => ({…}) })` /
    /// `api.injectEndpoints({…})` (pair-arrow and method-shorthand forms).
    pub(super) fn find_rtk_endpoints_object<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let callee = get_child_by_field(call, "function")?;
        let callee_name = match callee.kind() {
            "identifier" => get_node_text(callee, self.source()).to_string(),
            "member_expression" => get_child_by_field(callee, "property")
                .map(|p| get_node_text(p, self.source()).to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if callee_name != "createApi" && callee_name != "injectEndpoints" {
            return None;
        }
        let args = get_child_by_field(call, "arguments")?;
        let mut cursor = args.walk();
        let objects: Vec<Node<'t>> = args
            .named_children(&mut cursor)
            .filter(|a| a.kind() == "object" || a.kind() == "object_expression")
            .collect();
        for arg in objects {
            let mut c2 = arg.walk();
            let members: Vec<Node<'t>> = arg.named_children(&mut c2).collect();
            for member in members {
                if member.kind() == "pair" {
                    let key = get_child_by_field(member, "key");
                    if key.is_none_or(|k| get_node_text(k, self.source()) != "endpoints") {
                        continue;
                    }
                    if let Some(value) = get_child_by_field(member, "value")
                        && (value.kind() == "arrow_function"
                            || value.kind() == "function_expression")
                    {
                        return self.function_returned_object(value);
                    }
                } else if member.kind() == "method_definition" {
                    let key = get_child_by_field(member, "name");
                    if key.is_none_or(|k| get_node_text(k, self.source()) != "endpoints") {
                        continue;
                    }
                    return self.function_returned_object(member);
                }
            }
        }
        None
    }

    /// Each `getX: build.query|mutation|infiniteQuery({…})` endpoint becomes
    /// a function node named by the key, spanning its primary handler.
    pub(super) fn extract_rtk_endpoints(
        &mut self,
        rules: &'static dyn LanguageRules,
        obj: Node<'_>,
    ) {
        let mut cursor = obj.walk();
        let members: Vec<Node<'_>> = obj.named_children(&mut cursor).collect();
        for member in members {
            if member.kind() != "pair" {
                continue;
            }
            let key = get_child_by_field(member, "key");
            let value = get_child_by_field(member, "value");
            let (Some(key), Some(value)) = (key, value) else {
                continue;
            };
            if value.kind() != "call_expression" {
                continue;
            }
            let Some(callee) = get_child_by_field(value, "function") else {
                continue;
            };
            if callee.kind() != "member_expression" {
                continue;
            }
            let method = get_child_by_field(callee, "property")
                .map(|p| get_node_text(p, self.source()))
                .unwrap_or_default();
            if method != "query" && method != "mutation" && method != "infiniteQuery" {
                continue;
            }
            let name = self.object_key_name(key);
            if let Some(handler) = self.rtk_endpoint_handler(value) {
                super::extract_function_named(rules, self, handler, Some(&name));
            } else {
                // Config-only endpoint: bare node spanning the builder call;
                // walk it so a handler factory is captured as an edge.
                let sig: String = get_node_text(value, self.source())
                    .chars()
                    .take(80)
                    .collect();
                let extra = NodeExtra {
                    signature: Some(sig),
                    ..NodeExtra::default()
                };
                if let Some(idx) = self.create_node(NodeKind::Function, &name, value, extra)
                    && let Some(id) = self.nodes().get(idx).map(|n| n.id.clone())
                {
                    self.push_scope(id.clone());
                    self.visit_function_body(value, &id);
                    self.pop_scope();
                }
            }
        }
    }

    /// The primary handler arrow of an endpoint config: `queryFn` >
    /// `query` > first function-valued property (pair or shorthand).
    fn rtk_endpoint_handler<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let args = get_child_by_field(call, "arguments")?;
        let mut cursor = args.walk();
        let objects: Vec<Node<'t>> = args
            .named_children(&mut cursor)
            .filter(|a| a.kind() == "object" || a.kind() == "object_expression")
            .collect();
        for arg in objects {
            let mut query_fn = None;
            let mut query = None;
            let mut first_fn = None;
            let mut c2 = arg.walk();
            let members: Vec<Node<'t>> = arg.named_children(&mut c2).collect();
            for member in members {
                let (fn_node, key_name) = match member.kind() {
                    "pair" => {
                        let v = get_child_by_field(member, "value");
                        match v {
                            Some(v)
                                if v.kind() == "arrow_function"
                                    || v.kind() == "function_expression" =>
                            {
                                let k = get_child_by_field(member, "key")
                                    .map(|k| get_node_text(k, self.source()).to_string())
                                    .unwrap_or_default();
                                (Some(v), k)
                            }
                            _ => (None, String::new()),
                        }
                    }
                    "method_definition" => {
                        let k = get_child_by_field(member, "name")
                            .map(|k| get_node_text(k, self.source()).to_string())
                            .unwrap_or_default();
                        (Some(member), k)
                    }
                    _ => (None, String::new()),
                };
                let Some(fn_node) = fn_node else { continue };
                match key_name.as_str() {
                    "queryFn" => query_fn = query_fn.or(Some(fn_node)),
                    "query" => query = query.or(Some(fn_node)),
                    _ => first_fn = first_fn.or(Some(fn_node)),
                }
            }
            if let Some(h) = query_fn.or(query).or(first_fn) {
                return Some(h);
            }
        }
        None
    }

    /// RTK generated-hook destructures: `export const { useGetXQuery } =
    /// api` — mint a function node per hook-conventional binding.
    pub(super) fn extract_rtk_hook_bindings(
        &mut self,
        pattern: Node<'_>,
        is_exported: Option<bool>,
    ) {
        let mut cursor = pattern.walk();
        let bindings: Vec<Node<'_>> = pattern.named_children(&mut cursor).collect();
        for binding in bindings {
            if binding.kind() != "shorthand_property_identifier_pattern" {
                continue;
            }
            let name = get_node_text(binding, self.source()).to_string();
            if !RTK_HOOK_NAME_RE.is_match(&name) {
                continue;
            }
            let extra = NodeExtra {
                is_exported,
                signature: Some("= RTK Query generated hook".to_string()),
                ..NodeExtra::default()
            };
            self.create_node(NodeKind::Function, &name, binding, extra);
        }
    }

    // ---- Pinia / Vuex ----------------------------------------------------

    /// ≥2 distinct Vue-store signals in the file (cached per session).
    pub(super) fn looks_like_vue_store_file(&mut self) -> bool {
        if let Some(v) = self.vue_store_file_cache() {
            return v;
        }
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for m in VUE_STORE_FILE_SIGNAL.find_iter(self.source()) {
            seen.insert(m.as_str());
            if seen.len() >= 2 {
                break;
            }
        }
        let v = seen.len() >= 2;
        self.set_vue_store_file_cache(v);
        v
    }

    /// Inline store collections: `defineStore({actions:{…}})`,
    /// `defineStore('id', {…})`, `createStore({mutations:{…}})`,
    /// `new Vuex.Store({actions:{…}})`.
    pub(super) fn find_vue_store_collection_objects<'t>(&self, call: Node<'t>) -> Vec<Node<'t>> {
        let callee = get_child_by_field(call, "function")
            .or_else(|| get_child_by_field(call, "constructor"));
        let Some(callee) = callee else {
            return Vec::new();
        };
        let callee_name = match callee.kind() {
            "identifier" => get_node_text(callee, self.source()).to_string(),
            "member_expression" => get_child_by_field(callee, "property")
                .map(|p| get_node_text(p, self.source()).to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if !VUE_STORE_FACTORY_CALLEES.contains(&callee_name.as_str()) && callee_name != "Store" {
            return Vec::new();
        }
        let Some(args) = get_child_by_field(call, "arguments") else {
            return Vec::new();
        };
        let mut objects = Vec::new();
        let mut cursor = args.walk();
        let arg_objs: Vec<Node<'t>> = args
            .named_children(&mut cursor)
            .filter(|a| a.kind() == "object" || a.kind() == "object_expression")
            .collect();
        for arg in arg_objs {
            let mut c2 = arg.walk();
            let members: Vec<Node<'t>> = arg.named_children(&mut c2).collect();
            for member in members {
                if member.kind() != "pair" {
                    continue;
                }
                let key = get_child_by_field(member, "key");
                if key.is_none_or(|k| {
                    !VUE_STORE_COLLECTION_NAMES.contains(&get_node_text(k, self.source()))
                }) {
                    continue;
                }
                if let Some(value) = get_child_by_field(member, "value")
                    && (value.kind() == "object" || value.kind() == "object_expression")
                {
                    objects.push(value);
                }
            }
        }
        objects
    }

    /// Vuex MODULE default export: the config object's
    /// actions/mutations/getters collections → method nodes.
    pub(super) fn extract_store_collection_methods(
        &mut self,
        rules: &'static dyn LanguageRules,
        config: Node<'_>,
    ) {
        let mut cursor = config.walk();
        let members: Vec<Node<'_>> = config.named_children(&mut cursor).collect();
        for member in members {
            if member.kind() != "pair" {
                continue;
            }
            let key = get_child_by_field(member, "key");
            if key.is_none_or(|k| {
                !VUE_STORE_COLLECTION_NAMES.contains(&get_node_text(k, self.source()))
            }) {
                continue;
            }
            if let Some(value) = get_child_by_field(member, "value")
                && (value.kind() == "object" || value.kind() == "object_expression")
            {
                self.extract_object_literal_functions(rules, value);
            }
        }
    }

    /// The SETUP function of `defineStore('id', () => {…})` (block-bodied
    /// arrow/function arg); `None` for the options form.
    pub(super) fn find_pinia_setup_fn<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let callee = get_child_by_field(call, "function")?;
        if callee.kind() != "identifier" || get_node_text(callee, self.source()) != "defineStore" {
            return None;
        }
        let args = get_child_by_field(call, "arguments")?;
        let mut cursor = args.walk();
        let fns: Vec<Node<'t>> = args
            .named_children(&mut cursor)
            .filter(|a| a.kind() == "arrow_function" || a.kind() == "function_expression")
            .collect();
        fns.into_iter().find(|arg| {
            get_child_by_field(*arg, "body").is_some_and(|b| b.kind() == "statement_block")
        })
    }

    /// A Pinia setup store's actions: body-local `const foo = () => …` /
    /// `function foo(){}`, named by the binding.
    pub(super) fn extract_pinia_setup_body(
        &mut self,
        rules: &'static dyn LanguageRules,
        setup_fn: Node<'_>,
    ) {
        let Some(body) = get_child_by_field(setup_fn, "body") else {
            return;
        };
        if body.kind() != "statement_block" {
            return;
        }
        let mut cursor = body.walk();
        let stmts: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
        for stmt in stmts {
            if stmt.kind() == "function_declaration" {
                super::extract_function_named(rules, self, stmt, None);
            } else if rules.tables().variable_types.contains(&stmt.kind()) {
                let mut c2 = stmt.walk();
                let decls: Vec<Node<'_>> = stmt
                    .named_children(&mut c2)
                    .filter(|d| d.kind() == "variable_declarator")
                    .collect();
                for decl in decls {
                    if let Some(v) = get_child_by_field(decl, "value")
                        && (v.kind() == "arrow_function" || v.kind() == "function_expression")
                    {
                        super::extract_function_named(rules, self, v, None);
                    }
                }
            }
        }
    }
}
