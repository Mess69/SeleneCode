//! The function-body walker — the `visitFunctionBody` port
//! (extraction-core.md §10): calls, instantiations, bare calls, static
//! member reads, nested named functions + structural types, and the
//! value-reference pass. Extraction NEVER resolves cross-file — it emits
//! `UnresolvedReference`s and the resolver makes edges (the one exception:
//! same-file value-reference `references` edges, below).
//!
//! Wave-2 branches (VB.NET call ambiguity, Erlang remote calls/records,
//! Ruby receiver/method fields, ArkTS UI chains, Dart selectors, C++
//! fn-pointer bindings and stack construction, Rust route macros) land with
//! their language tasks — marked inline.

use std::collections::{HashMap, HashSet};

use selene_core::{Edge, EdgeKind, NodeKind, Provenance};
use tree_sitter::{Node, Tree};

use crate::fnref::{CaptureMode, QUALIFIED_IMPORT, SIMPLE_NAME};
use crate::helpers::{get_child_by_field, get_node_text};
use crate::is_generated_file;
use crate::rules::ClassKind;
use crate::walker::Session;
use crate::{Language, UnresolvedReference};

/// The INTERNAL reference kind for function-as-value candidates (#756). It is
/// never an [`EdgeKind`]: Phase 3's resolver maps a resolved `function_ref` to
/// a `references` edge carrying `metadata.fnRef = true` (map §Wire —
/// "`function_ref` never persists as an edge kind").
pub(crate) const FUNCTION_REF_KIND: &str = "function_ref";

/// Constructor-invocation node kinds → `instantiates` refs (§10).
pub(super) const INSTANTIATION_KINDS: [&str; 6] = [
    "new_expression",               // typescript / javascript / tsx / jsx
    "object_creation_expression",   // java / c#
    "instance_creation_expression", // some grammars
    "composite_literal",            // go — `Widget{...}` / `pkga.Widget{...}`
    "struct_expression",            // rust — `Widget { n: 1 }`
    "instance_expression",          // scala (wave 2 — inert until its grammar lands)
];

/// Member-access node kinds whose receiver may be a static `Type.MEMBER`
/// read (§10). TS/JS/Python `member_expression`/`attribute` are DELIBERATELY
/// excluded — measured A/B in the TS lineage: imports already cover those,
/// adding them was pure noise.
const MEMBER_ACCESS_TYPES: [&str; 7] = [
    "field_access",                      // java (`Foo.BAR`)
    "member_access_expression",          // c#
    "navigation_expression",             // kotlin / swift
    "field_expression",                  // scala
    "class_constant_access_expression",  // php (`Foo::CONST`)
    "scoped_property_access_expression", // php (`Foo::$bar`)
    "qualified_identifier",              // c++ (`Foo::bar`)
];

/// The static-member pass language gate — `STATIC_MEMBER_LANGS ∩ v0`
/// (swift/scala/dart are wave 2).
fn is_static_member_lang(l: Language) -> bool {
    matches!(
        l,
        Language::Java | Language::CSharp | Language::Kotlin | Language::Php | Language::Cpp
    )
}

/// Value-reference pass language gate (`VALUE_REF_LANGS`, ported whole —
/// wave-2 members are inert until their grammars land).
fn is_value_ref_lang(l: Language) -> bool {
    matches!(
        l,
        Language::Typescript
            | Language::Javascript
            | Language::Tsx
            | Language::Arkts
            | Language::Go
            | Language::Python
            | Language::Rust
            | Language::Ruby
            | Language::C
            | Language::Java
            | Language::CSharp
            | Language::Php
            | Language::Scala
            | Language::Kotlin
            | Language::Swift
            | Language::Dart
            | Language::Pascal
    )
}

/// Visit cap for both value-reference scans (Global Constraints).
pub(crate) const MAX_VALUE_REF_NODES: usize = 20_000;

/// Receivers that don't aid resolution — bare method name instead
/// (the GENERIC function-field branch's set; TS keeps this at 4 entries).
const SKIP_RECEIVERS: [&str; 4] = ["self", "this", "cls", "super"];

/// The object+name branch's skip set — TS defines it separately with SIX
/// entries: PHP `parent::m()` / `static::m()` must emit the bare method
/// name, never a `parent.m` / `static.m` qualified ref (unresolvable).
const OBJECT_NAME_SKIP_RECEIVERS: [&str; 6] = ["self", "this", "cls", "super", "parent", "static"];

impl Session<'_> {
    /// Walk a function/method body (§10): calls, instantiations, bare
    /// calls, static member reads, nested NAMED functions (anonymous
    /// wrappers get no node but their body is still walked — #528; handled
    /// by the callers), and structural types defined inside bodies.
    pub fn visit_function_body(&mut self, body: Node<'_>, _fn_id: &str) {
        self.visit_for_calls_and_structure(body);
    }

    fn visit_for_calls_and_structure(&mut self, node: Node<'_>) {
        let rules = self.rules();
        let t = rules.tables();
        let node_type = node.kind();

        // Function-as-value capture (#756) — function bodies are walked HERE,
        // not by `walker::visit`, so the capture hook must fire in both
        // walkers (a node is only ever visited by one of them).
        self.maybe_capture_fn_refs(node, node_type);

        // INSERTION POINT (Task 9): Rust route-registration macros.

        if t.call_types.contains(&node_type) {
            self.extract_call(node);
        } else if INSTANTIATION_KINDS.contains(&node_type) {
            self.extract_instantiation(node);
            // Java/C# `new T(...) { … }` — anonymous class with body
            // (Task 10): extract as a class so interface-impl synthesis
            // (Phase 5.5) can bridge T's methods to the overrides.
            if let Some(anon_body) = find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                return;
            }
        } else if let Some(callee) = rules.extract_bare_call(node, self.source()) {
            // Ruby/Dart bare-call hook (Task 14 / wave 2).
            if let Some(caller_id) = self.node_stack().last().cloned() {
                self.push_call_ref(&caller_id, callee, node);
            }
        }

        // INSERTION POINT (Task 13): C++ stack construction + local
        // function-pointer bindings.

        // Static-member / value-read: `Enum.value`, `Type.CONST`, `Foo::BAR`.
        self.extract_static_member_ref(node);

        // Local variable type annotations (Task 8): locals get NO nodes, but
        // the TYPE a local is annotated with is a real dependency of the
        // enclosing function — attribute a `references` ref to it. Falls
        // through to the recursion so initializer calls still walk.
        if node_type == "variable_declarator"
            && super::ts_core::is_type_annotation_language(self.language())
            && let Some(owner_id) = self.node_stack().last().cloned()
        {
            self.extract_variable_type_annotation(node, &owner_id);
        }

        // Nested NAMED functions become their own nodes; anonymous ones fall
        // through to the recursion so their calls attribute to the encloser.
        if t.function_types.contains(&node_type) {
            let name = super::extract_name(rules, node, self.source());
            if !name.is_empty() && name != "<anonymous>" {
                super::extract_function(rules, self, node);
                return;
            }
        }

        // Structural types inside bodies — each extractor walks its own
        // children.
        if t.class_types.contains(&node_type) {
            match rules.classify_class_node(node, self.source()) {
                Some(ClassKind::Struct) => super::extract_struct(rules, self, node),
                Some(ClassKind::Enum) => super::extract_enum(rules, self, node),
                Some(ClassKind::Interface) => super::extract_interface(rules, self, node),
                Some(ClassKind::Trait) => super::extract_class(rules, self, node, NodeKind::Trait),
                _ => super::extract_class(rules, self, node, NodeKind::Class),
            }
            return;
        }
        if t.struct_types.contains(&node_type) {
            super::extract_struct(rules, self, node);
            return;
        }
        if t.enum_types.contains(&node_type) {
            super::extract_enum(rules, self, node);
            return;
        }
        if t.interface_types.contains(&node_type) {
            super::extract_interface(rules, self, node);
            return;
        }

        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            self.visit_for_calls_and_structure(child);
        }
    }

    /// Callee-name extraction (§10 generic path). Wave-2 special branches
    /// (VB.NET, Erlang, Ruby, ArkTS, CFML) land with their language tasks.
    pub(crate) fn extract_call(&mut self, node: Node<'_>) {
        let Some(caller_id) = self.node_stack().last().cloned() else {
            return;
        };
        let source = self.source();
        let language = self.language();

        // Ruby `call` nodes use `receiver` + `method` fields (tree-sitter-ruby),
        // NOT the `object`/`name`/`function` fields every branch below expects —
        // so without this they fell through to the generic path, which took the
        // RECEIVER as the callee and DROPPED the method name: `@db.query(id)`
        // produced a `calls` ref to `@db` (unresolvable) and no method edge was
        // ever recorded, so a Ruby method's callers/impact were invisible
        // (tree-sitter.ts:3843-3898, #1108 follow-up).
        if language == Language::Ruby && matches!(node.kind(), "call" | "method_call") {
            self.extract_ruby_call(node, &caller_id);
            return;
        }

        let mut callee_name = String::new();

        // Java/Kotlin `method_invocation`, PHP `member_call_expression` /
        // `scoped_call_expression`: `object`/`scope` + `name` fields. The
        // chained-receiver re-encodes for these shapes land with Tasks
        // 10/14 (INSERTION POINT).
        let name_field = get_child_by_field(node, "name");
        let object_field =
            get_child_by_field(node, "object").or_else(|| get_child_by_field(node, "scope"));
        if let (Some(name_node), Some(object)) = (name_field, object_field)
            && matches!(
                node.kind(),
                "method_invocation" | "member_call_expression" | "scoped_call_expression"
            )
        {
            let method_name = get_node_text(name_node, source).to_string();

            // PHP static-factory fluent chain (#608): `Cls::for($x)->method()`
            // — the receiver is itself a static call; encode
            // `Cls::factory().method` so the resolver infers the class from
            // the factory's declared return (the `().` marker splits it).
            if !method_name.is_empty()
                && language == Language::Php
                && object.kind() == "scoped_call_expression"
            {
                let inner_scope = get_child_by_field(object, "scope");
                let inner_name = get_child_by_field(object, "name");
                let callee = match (inner_scope, inner_name) {
                    (Some(sc), Some(nm)) => format!(
                        "{}::{}().{method_name}",
                        get_node_text(sc, source),
                        get_node_text(nm, source)
                    ),
                    _ => method_name,
                };
                self.push_call_ref(&caller_id, callee, node);
                return;
            }

            // Java static-factory / fluent chain (#645/#608):
            // `Foo.getInstance().bar()` — encode `Foo.getInstance().bar`
            // (empty parens normalized: factory args would break the split).
            if !method_name.is_empty()
                && language == Language::Java
                && object.kind() == "method_invocation"
            {
                let inner_obj = get_child_by_field(object, "object");
                let inner_name = get_child_by_field(object, "name");
                if let (Some(io), Some(inm)) = (inner_obj, inner_name) {
                    let callee = format!(
                        "{}.{}().{method_name}",
                        get_node_text(io, source),
                        get_node_text(inm, source)
                    );
                    self.push_call_ref(&caller_id, callee, node);
                    return;
                }
            }

            // Java `this.userbo.toLogin2()` parses as
            // method_invocation(object = field_access(this, userbo)) —
            // unwrap to the FIELD name so the resolver's single-dot
            // receiver matching can look it up in the enclosing class's
            // field declarations (`this.userbo` matches nothing).
            let raw_receiver = if object.kind() == "field_access" {
                let inner = get_child_by_field(object, "object");
                let fld = get_child_by_field(object, "field");
                match (inner, fld) {
                    (Some(i), Some(f)) if i.kind() == "this" || i.kind() == "this_expression" => {
                        get_node_text(f, source).to_string()
                    }
                    _ => get_node_text(object, source).to_string(),
                }
            } else {
                get_node_text(object, source).to_string()
            };
            // PHP `$receiver` → `receiver`.
            let receiver_name = raw_receiver.trim_start_matches('$').to_string();
            callee_name = if OBJECT_NAME_SKIP_RECEIVERS.contains(&receiver_name.as_str()) {
                method_name
            } else {
                format!("{receiver_name}.{method_name}")
            };
            self.push_call_ref(&caller_id, callee_name, node);
            return;
        }

        let func = get_child_by_field(node, "function").or_else(|| node.named_child(0));
        if let Some(func) = func {
            match func.kind() {
                // Method call: `obj.method()` / `obj.field.method()`.
                "member_expression"
                | "attribute"
                | "selector_expression"
                | "navigation_expression"
                | "field_expression" => {
                    let mut property = get_child_by_field(func, "property")
                        .or_else(|| get_child_by_field(func, "field"));
                    if property.is_none() {
                        let child1 = func.named_child(1);
                        // Kotlin: navigation_suffix wraps the method name.
                        property = match child1 {
                            Some(c) if c.kind() == "navigation_suffix" => {
                                let mut cur = c.walk();
                                c.named_children(&mut cur)
                                    .find(|n| {
                                        n.kind() == "simple_identifier" || n.kind() == "identifier"
                                    })
                                    .or(Some(c))
                            }
                            other => other,
                        };
                    }
                    if let Some(property) = property {
                        let method_name = get_node_text(property, source).to_string();
                        let receiver = get_child_by_field(func, "object")
                            .or_else(|| get_child_by_field(func, "operand"))
                            .or_else(|| get_child_by_field(func, "argument"))
                            .or_else(|| func.named_child(0));
                        callee_name = match receiver {
                            Some(recv)
                                if matches!(
                                    recv.kind(),
                                    "identifier" | "simple_identifier" | "field_identifier"
                                ) =>
                            {
                                let receiver_name = get_node_text(recv, source);
                                if SKIP_RECEIVERS.contains(&receiver_name) {
                                    method_name
                                } else {
                                    format!("{receiver_name}.{method_name}")
                                }
                            }
                            // Chained factory: receiver is itself a call —
                            // re-encode `inner().method` (the `().` marker is
                            // a resolver contract) with per-language guards.
                            Some(recv)
                                if recv.kind() == "call_expression"
                                    && matches!(
                                        language,
                                        Language::C
                                            | Language::Cpp
                                            | Language::Kotlin
                                            | Language::Rust
                                            | Language::Go
                                    ) =>
                            {
                                let (inner_callee, reencode) = if language == Language::Kotlin {
                                    // Inner callee = the call's first named
                                    // child; only re-encode capitalized
                                    // (class/companion-factory) chains.
                                    let inner = recv.named_child(0);
                                    let text = inner
                                        .map(|n| {
                                            get_node_text(n, source)
                                                .split_whitespace()
                                                .collect::<String>()
                                        })
                                        .unwrap_or_default();
                                    let re =
                                        text.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                                    (text, re)
                                } else {
                                    let inner_fn = get_child_by_field(recv, "function");
                                    let text = inner_fn
                                        .map(|n| {
                                            get_node_text(n, source)
                                                .replace("->", ".")
                                                .split_whitespace()
                                                .collect::<String>()
                                        })
                                        .unwrap_or_default();
                                    let re = match language {
                                        // Rust: only associated-fn chains.
                                        Language::Rust => inner_fn
                                            .is_some_and(|n| n.kind() == "scoped_identifier"),
                                        // Go: only bare package-level factories.
                                        Language::Go => {
                                            inner_fn.is_some_and(|n| n.kind() == "identifier")
                                        }
                                        // C/C++: any inner callee.
                                        _ => !text.is_empty(),
                                    };
                                    (text, re)
                                };
                                if reencode && !inner_callee.is_empty() {
                                    format!("{inner_callee}().{method_name}")
                                } else {
                                    method_name
                                }
                            }
                            _ => method_name,
                        };
                    }
                }
                // Scoped call: `Module::function()` keeps the full text.
                "scoped_identifier" | "scoped_call_expression" => {
                    callee_name = get_node_text(func, source).to_string();
                }
                // C# member call `recv.Method(...)` (+ chained-factory
                // re-encode when the receiver is an invocation).
                "member_access_expression" if language == Language::CSharp => {
                    let recv = get_child_by_field(func, "expression");
                    let name_node = get_child_by_field(func, "name");
                    let method_name = name_node
                        .map(|n| get_node_text(n, source).to_string())
                        .unwrap_or_default();
                    callee_name = match recv {
                        Some(r)
                            if r.kind() == "invocation_expression" && !method_name.is_empty() =>
                        {
                            let inner = get_child_by_field(r, "function")
                                .map(|n| {
                                    get_node_text(n, source)
                                        .split_whitespace()
                                        .collect::<String>()
                                })
                                .unwrap_or_default();
                            if inner.is_empty() {
                                method_name
                            } else {
                                format!("{inner}().{method_name}")
                            }
                        }
                        _ => get_node_text(func, source).to_string(),
                    };
                }
                _ => {
                    callee_name = get_node_text(func, source).to_string();
                }
            }
        }

        // Parenthesized type conversions — Go `(*T)(x)` / `(T)(x)`:
        // normalize to the inner name (regex-free port of
        // /^\(\s*\*?\s*([A-Za-z_][\w.]*)\s*\)$/).
        if let Some(inner) = parse_paren_conversion(&callee_name) {
            callee_name = inner;
        }

        // C/C++ templated callees (Task 13): `fn<T, 256>(args)` /
        // `ns::fn<T>(args)` carry template args that can never match the
        // bare defined name — strip them (the CUDA kernel-launch shape once
        // `<<<…>>>` is blanked). `operator<`/`operator<<` callees excluded:
        // their `<` IS the operator.
        if !callee_name.is_empty()
            && callee_name.contains('<')
            && matches!(language, Language::Cpp | Language::C)
            && !callee_name.contains("operator")
        {
            callee_name =
                crate::rules::cpp_preparse::strip_cpp_template_args(&callee_name).into_owned();
        }
        // INSERTION POINT (Task 13b/wave 2): C++ local fn-pointer call
        // rewrite (`auto kernel = &fn<…>; kernel<<<…>>>(…)`).

        if !callee_name.is_empty() {
            self.push_call_ref(&caller_id, callee_name, node);
        }
    }

    /// Ruby `call` / `method_call` — `receiver` + `method` fields.
    ///
    /// Builds `receiver.method` so the resolver (and local-variable type
    /// inference) can link the hop; `Foo.new` stays an instantiation; `self`/
    /// `super` receivers collapse to the bare method name. A CONSTANT receiver
    /// (`JSON.parse`) additionally emits a `references` ref — a class used only
    /// via its class methods still records a dependent, which is the edge the old
    /// receiver-as-callee behavior happened to provide and which making the callee
    /// correct would otherwise have silently dropped.
    ///
    /// Verbatim port of tree-sitter.ts:3843-3898.
    fn extract_ruby_call(&mut self, node: Node<'_>, caller_id: &str) {
        let source = self.source();
        let Some(method_node) = get_child_by_field(node, "method") else {
            return; // operator / element-reference call with no method name
        };
        let method_name = get_node_text(method_node, source).to_string();
        if method_name.is_empty() {
            return;
        }

        let Some(receiver_node) = get_child_by_field(node, "receiver") else {
            // Bare `foo(...)` — just the method name (ts:3860-3862).
            self.push_call_ref(caller_id, method_name, node);
            return;
        };
        let receiver_name = get_node_text(receiver_node, source).to_string();

        // `Foo.new` / `Foo::Bar.new` is construction — emit `instantiates` against
        // the class (last `::` segment), preserving the "what creates X" edge
        // (ts:3866-3874).
        if method_name == "new" {
            let class_name = match receiver_name.rfind("::") {
                Some(i) => receiver_name[i + 2..].to_string(),
                None => receiver_name.clone(),
            };
            if class_name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            {
                self.add_unresolved(UnresolvedReference {
                    from_node_id: caller_id.to_string(),
                    reference_name: class_name,
                    reference_kind: EdgeKind::Instantiates.as_str().to_string(),
                    line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
                    column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
                    file_path: None,
                    language: None,
                });
                return;
            }
        }

        // `self`/`super` add nothing to the callee's identity (ts:3876-3877).
        let skip = matches!(receiver_name.as_str(), "self" | "super");
        let callee = if skip {
            method_name
        } else {
            format!("{receiver_name}.{method_name}")
        };
        self.push_call_ref(caller_id, callee, node);

        // A capitalized (`constant`) receiver is itself a dependency on that
        // constant (ts:3885-3897).
        if !skip && receiver_node.kind() == "constant" {
            self.add_unresolved(UnresolvedReference {
                from_node_id: caller_id.to_string(),
                reference_name: receiver_name,
                reference_kind: EdgeKind::References.as_str().to_string(),
                line: Some(u32::try_from(receiver_node.start_position().row).unwrap_or(0) + 1),
                column: Some(u32::try_from(receiver_node.start_position().column).unwrap_or(0)),
                file_path: None,
                language: None,
            });
        }
    }

    fn push_call_ref(&mut self, caller_id: &str, callee: String, node: Node<'_>) {
        self.add_unresolved(UnresolvedReference {
            from_node_id: caller_id.to_string(),
            reference_name: callee,
            reference_kind: EdgeKind::Calls.as_str().to_string(),
            line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }

    /// `new Foo()` and friends → `instantiates` ref to the class name (§10).
    /// Children still recurse (callers walk them), so nested ctor-arg calls
    /// keep their own `calls` refs.
    pub(crate) fn extract_instantiation(&mut self, node: Node<'_>) {
        let Some(from_id) = self.node_stack().last().cloned() else {
            return;
        };
        let source = self.source();
        let ctor = get_child_by_field(node, "constructor")
            .or_else(|| get_child_by_field(node, "type"))
            .or_else(|| get_child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };

        // Go composite literals: only directly-named struct types; KEEP the
        // package qualifier (`pkga.Widget`) for the cross-package resolver.
        if node.kind() == "composite_literal" {
            if ctor.kind() != "type_identifier" && ctor.kind() != "qualified_type" {
                return;
            }
            let mut go_type = get_node_text(ctor, source).trim().to_string();
            if let Some(br) = go_type.find('[')
                && br > 0
            {
                go_type.truncate(br);
                go_type = go_type.trim().to_string();
            }
            if !go_type.is_empty() {
                self.push_instantiates_ref(&from_id, go_type, node);
            }
            return;
        }
        // (Scala instance_expression unwrap — wave 2.)

        let mut class_name = get_node_text(ctor, source).to_string();
        // `new Map<K, V>()` → `Map`.
        if let Some(lt) = class_name.find('<')
            && lt > 0
        {
            class_name.truncate(lt);
        }
        // `new ns.Foo()` / `ns::Foo()` → `Foo`.
        let last_dot = class_name.rfind('.').map(|i| i + 1);
        let last_colons = class_name.rfind("::").map(|i| i + 2);
        if let Some(cut) = last_dot.max(last_colons) {
            class_name = class_name[cut..].trim_start_matches([':', '.']).to_string();
        }
        let class_name = class_name.trim().to_string();
        if !class_name.is_empty() {
            self.push_instantiates_ref(&from_id, class_name, node);
        }
    }

    fn push_instantiates_ref(&mut self, from_id: &str, class_name: String, node: Node<'_>) {
        self.add_unresolved(UnresolvedReference {
            from_node_id: from_id.to_string(),
            reference_name: class_name,
            reference_kind: EdgeKind::Instantiates.as_str().to_string(),
            line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }

    /// Static-member / value-read (`Type.CONST`, `Foo::BAR`) → `references`
    /// ref, gated to the capitalized-type-convention languages (§10; the
    /// Dart selector shape is wave 2).
    fn extract_static_member_ref(&mut self, node: Node<'_>) {
        if !is_static_member_lang(self.language()) {
            return;
        }
        let Some(owner_id) = self.node_stack().last().cloned() else {
            return;
        };
        if !MEMBER_ACCESS_TYPES.contains(&node.kind()) {
            return;
        }
        let source = self.source();

        // Skip `Type.method()` — the access is a call's callee, already
        // linked by extract_call.
        if let Some(parent) = node.parent()
            && self.rules().tables().call_types.contains(&parent.kind())
        {
            let callee = get_child_by_field(parent, "function")
                .or_else(|| get_child_by_field(parent, "method"))
                .or_else(|| parent.named_child(0));
            if let Some(c) = callee
                && c.start_byte() == node.start_byte()
            {
                return;
            }
        }

        // Receiver must be a SIMPLE capitalized identifier.
        let recv = get_child_by_field(node, "object")
            .or_else(|| get_child_by_field(node, "expression"))
            .or_else(|| get_child_by_field(node, "scope"))
            .or_else(|| node.named_child(0));
        let Some(recv) = recv else { return };
        if matches!(
            recv.kind(),
            "identifier"
                | "type_identifier"
                | "simple_identifier"
                | "name"
                | "scoped_type_identifier"
        ) {
            let text = get_node_text(recv, source);
            let simple_capitalized = text.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if simple_capitalized {
                self.add_unresolved(UnresolvedReference {
                    from_node_id: owner_id,
                    reference_name: text.to_string(),
                    reference_kind: EdgeKind::References.as_str().to_string(),
                    line: Some(u32::try_from(recv.start_position().row).unwrap_or(0) + 1),
                    column: Some(u32::try_from(recv.start_position().column).unwrap_or(0)),
                    file_path: None,
                    language: None,
                });
            }
        }
    }
}

impl Session<'_> {
    /// A Java/C# anonymous class — `new T() { …members }` (Task 10, the
    /// `extractAnonymousClass` port): a `class` node named `<T$anon@line>`,
    /// an `extends` ref to T (extraction can't tell class from interface;
    /// resolution binds T to whatever it is and Phase 5.5 handles both),
    /// and the body walked so `method_declaration` members become method
    /// nodes under the anon class. Without this, the overrides inside a
    /// lambda-returned `new T() { @Override … }` are not nodes and a call
    /// through T's abstract method has no static target.
    pub(crate) fn extract_anonymous_class(&mut self, node: Node<'_>, body: Node<'_>) {
        // Same type lookup as extract_instantiation, so the anon class's
        // `extends` target matches the `instantiates` edge.
        let type_node = get_child_by_field(node, "constructor")
            .or_else(|| get_child_by_field(node, "type"))
            .or_else(|| get_child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let mut type_name = type_node
            .map(|t| get_node_text(t, self.source()).to_string())
            .unwrap_or_else(|| "Object".to_string());
        if let Some(lt) = type_name.find('<')
            && lt > 0
        {
            type_name.truncate(lt);
        }
        let last_dot = type_name.rfind('.').map(|i| i + 1);
        let last_colons = type_name.rfind("::").map(|i| i + 2);
        if let Some(cut) = last_dot.max(last_colons) {
            type_name = type_name[cut..].trim_start_matches([':', '.']).to_string();
        }
        let type_name = {
            let t = type_name.trim();
            if t.is_empty() { "Object" } else { t }.to_string()
        };

        let anon_name = format!(
            "<{type_name}$anon@{}>",
            u32::try_from(node.start_position().row).unwrap_or(0) + 1
        );
        let Some(idx) = self.create_node(NodeKind::Class, &anon_name, node, Default::default())
        else {
            return;
        };
        let Some(anon_id) = self.nodes().get(idx).map(|n| n.id.clone()) else {
            return;
        };

        // TS quirk ported verbatim (contract, don't fix silently): this one
        // ref's `line` is the RAW 0-based row — the TS source omits the +1.
        let pos = type_node.unwrap_or(node);
        self.add_unresolved(UnresolvedReference {
            from_node_id: anon_id.clone(),
            reference_name: type_name,
            reference_kind: EdgeKind::Extends.as_str().to_string(),
            line: Some(u32::try_from(pos.start_position().row).unwrap_or(0)),
            column: Some(u32::try_from(pos.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });

        self.push_scope(anon_id);
        let mut cursor = body.walk();
        let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
        for child in children {
            self.visit(child);
        }
        self.pop_scope();
    }
}

/// A Java `class_body` / C# `declaration_list` directly under an
/// instantiation node — the anonymous-class body (`findAnonymousClassBody`).
pub(super) fn find_anonymous_class_body<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "class_body" | "declaration_list"))
}

/// `^\(\s*\*?\s*([A-Za-z_][\w.]*)\s*\)$` without a regex: the parenthesized
/// type-conversion normalization.
fn parse_paren_conversion(callee: &str) -> Option<String> {
    let inner = callee.strip_prefix('(')?.strip_suffix(')')?.trim();
    let inner = inner.strip_prefix('*').unwrap_or(inner).trim();
    let mut chars = inner.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return None;
    }
    Some(inner.to_string())
}

/// Candidates-only scan of a subtree the main walkers won't traverse
/// (`tree-sitter.ts:567` `scanFnRefSubtree`): top-level variable initializers,
/// class field/property initializers, and subtrees a custom `visit_node` hook
/// consumed. NO extraction side effects.
///
/// Halts at nested function boundaries: their bodies are walked — and their
/// candidates attributed — by `extract_function`'s own body walk, so scanning
/// into them would attribute a callback to the WRONG scope.
pub(super) fn scan_fn_ref_subtree(s: &mut Session<'_>, node: Node<'_>, depth: u32) {
    if s.fn_ref_spec().is_none() || depth > 12 {
        return;
    }
    let node_type = node.kind();
    if depth > 0
        && (s.rules().tables().function_types.contains(&node_type)
            || matches!(
                node_type,
                "arrow_function" | "function_expression" | "lambda_literal" | "lambda_expression"
            ))
    {
        return;
    }
    s.maybe_capture_fn_refs(node, node_type);
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        scan_fn_ref_subtree(s, child, depth + 1);
    }
}

/// The function-as-value GATE (`tree-sitter.ts:603` `flushFnRefCandidates`):
/// gate the captured candidates and push the survivors as `function_ref`
/// [`UnresolvedReference`]s.
///
/// The gate bounds volume and protects precision (design doc rule 1): a
/// candidate survives only if its name matches a function/method DEFINED IN
/// THIS FILE or a name this file IMPORTS. Everything else (locals, params,
/// fields passed as arguments) is dropped before it ever reaches the database.
/// Phase 3's resolver then matches survivors against function/method nodes
/// only and emits `references` edges — which callers/impact already traverse.
///
/// Exceptions, each bought by a real-repo false positive:
/// - **C-family FILE-scope initializers** skip the gate (design doc rule 2) —
///   a constant-expression context, and C has no symbol imports.
/// - **`this.<member>` and `Scope::member`** always flush — the gate cannot see
///   their scope (inherited members, same-package JVM types), and resolution is
///   class-scoped / suffix-anchored + unique-or-drop.
/// - **C++ bare identifiers** outside file-scope initializer tables are dropped
///   (`address_of_only`, design doc rule 4).
///
/// Known v1 limit, deliberate: a C/C++ callback registered in a DIFFERENT
/// translation unit than its definition, in a GATED position (assignment), is
/// only captured when the name is repo-unique at resolution.
pub(super) fn flush_fn_ref_candidates(s: &mut Session<'_>) {
    let candidates = s.take_fn_ref_candidates();
    if candidates.is_empty() {
        return;
    }
    // Generated/minified files (vendored `jquery.min.js` and friends): their
    // function-as-value edges are noise — single-letter minified symbols
    // resolve everywhere (design doc rule 8; Alamofire).
    if is_generated_file(s.file_path()) {
        return;
    }
    let Some(spec) = s.fn_ref_spec() else { return };

    let defined_here: HashSet<&str> = s
        .nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Function || n.kind == NodeKind::Method)
        .map(|n| n.name.as_str())
        .collect();

    // Import-binding names ONLY (every binding emitter pushes kind `imports`).
    // Deliberately NOT `references`: those carry type-annotation and
    // interface-member names, which let local variables that share a type
    // member's name slip through the gate (excalidraw A/B finding). A dotted
    // import (JVM `import com.example.OtherClass`) or backslashed PHP `use`
    // also contributes its LAST segment — the simple name code references.
    let mut imported_names: HashSet<&str> = HashSet::new();
    for r in s.unresolved() {
        if r.reference_kind != EdgeKind::Imports.as_str() {
            continue;
        }
        if SIMPLE_NAME.is_match(&r.reference_name) {
            imported_names.insert(&r.reference_name);
        } else if let Some(last) = QUALIFIED_IMPORT
            .captures(&r.reference_name)
            .and_then(|c| c.get(1))
        {
            imported_names.insert(last.as_str());
        }
    }

    let mut survivors: Vec<UnresolvedReference> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (c, from_node_id) in &candidates {
        let at_file_scope = from_node_id.starts_with("file:");

        // C++ (`address_of_only`): a BARE identifier qualifies only inside a
        // file-scope initializer table. Everywhere else — args, assignments,
        // local braced-init lists like `{begin, size}` — only explicit `&`
        // forms count (fmt A/B finding: generic names `begin`/`out`/`size`
        // collide with locals and members).
        if spec.address_of_only
            && !c.explicit_ref
            && !(at_file_scope && matches!(c.mode, CaptureMode::Value | CaptureMode::List))
        {
            continue;
        }

        // Gate policy by candidate shape (see the doc comment above):
        //  - `this.<member>` / `Scope::member`: ALWAYS flush.
        //  - C-family file-scope initializers: skip the gate entirely.
        //  - everything else: name ∈ same-file functions/methods ∪ imports.
        if !c.name.starts_with("this.") && !c.name.contains("::") {
            let skip_gate = (spec.is_ungated_mode(c.mode) && at_file_scope) || c.skip_gate;
            if !skip_gate
                && !defined_here.contains(c.name.as_str())
                && !imported_names.contains(c.name.as_str())
            {
                continue;
            }
        }

        let key = (from_node_id.clone(), c.name.clone());
        if !seen.insert(key) {
            continue;
        }
        survivors.push(UnresolvedReference {
            from_node_id: from_node_id.clone(),
            reference_name: c.name.clone(),
            // The INTERNAL reference kind — resolution maps it to a
            // `references` edge (`metadata.fnRef`); it NEVER persists as an
            // edge kind (map §Wire).
            reference_kind: FUNCTION_REF_KIND.to_string(),
            line: Some(c.line),
            column: Some(c.column),
            file_path: None,
            language: None,
        });
    }
    for r in survivors {
        s.add_unresolved(r);
    }
}

/// The value-reference flush (§10): same-file `references` EDGES (metadata
/// `{"valueRef": true}`, provenance TreeSitter) from reader scopes to
/// distinctive file/class-scope constants, shadow-pruned by comparing
/// declarator counts against file-scope definition counts, both scans
/// capped at [`MAX_VALUE_REF_NODES`]. Wave-2 declarator arms (Scala, Dart,
/// Pascal, Swift) and the Dart/Pascal sibling-body pull land with their
/// languages.
pub(super) fn flush_value_refs(s: &mut Session<'_>, tree: &Tree) {
    let (scopes, mut targets, file_scope_counts) = s.take_value_ref_state();
    if !s.value_refs_enabled() || !is_value_ref_lang(s.language()) {
        return;
    }
    if targets.is_empty() || scopes.is_empty() || is_generated_file(s.file_path()) {
        return;
    }
    let source = s.source();

    // Shadow prune: count every declarator of a target name across the tree;
    // more declarators than file-scope defs ⇒ a local shadow exists ⇒ drop
    // the target (a conditional module-level re-def keeps them EQUAL).
    {
        let mut decl_counts: HashMap<String, usize> = HashMap::new();
        let mut bump = |name_node: Option<Node<'_>>| {
            if let Some(n) = name_node
                && (n.kind() == "identifier" || n.kind() == "simple_identifier")
            {
                let nm = get_node_text(n, source);
                if targets.contains_key(nm) {
                    *decl_counts.entry(nm.to_string()).or_insert(0) += 1;
                }
            }
        };
        let mut stack = vec![tree.root_node()];
        let mut visited = 0usize;
        while let Some(n) = stack.pop() {
            if visited >= MAX_VALUE_REF_NODES {
                break;
            }
            visited += 1;
            match n.kind() {
                // TS/JS declarators; Go specs.
                "variable_declarator" | "const_spec" | "var_spec" => bump(n.named_child(0)),
                // Rust consts/statics.
                "const_item" | "static_item" => bump(get_child_by_field(n, "name")),
                // Rust locals / Go `:=` / Python assignments (incl. tuple
                // destructuring).
                "let_declaration" | "short_var_declaration" | "assignment" => {
                    let left = get_child_by_field(n, "left")
                        .or_else(|| get_child_by_field(n, "pattern"))
                        .or_else(|| n.named_child(0));
                    match left {
                        Some(l) if l.kind() == "identifier" => bump(Some(l)),
                        Some(l) => {
                            let mut cur = l.walk();
                            let kids: Vec<Node<'_>> = l.named_children(&mut cur).collect();
                            for c in kids {
                                bump(Some(c));
                            }
                        }
                        None => {}
                    }
                }
                // C file-scope consts AND shadowing locals.
                "init_declarator" => {
                    let d = get_child_by_field(n, "declarator");
                    if let Some(d) = d
                        && d.kind() == "identifier"
                    {
                        bump(Some(d));
                    }
                }
                // Kotlin `val`/`var` (kotlin-ng names are `identifier`).
                "property_declaration" => {
                    let mut cur = n.walk();
                    let vd = n
                        .named_children(&mut cur)
                        .find(|c| c.kind() == "variable_declaration");
                    if let Some(vd) = vd {
                        let mut c2 = vd.walk();
                        let id = vd
                            .named_children(&mut c2)
                            .find(|c| c.kind() == "identifier" || c.kind() == "simple_identifier");
                        bump(id);
                    }
                }
                _ => {}
            }
            let mut cur = n.walk();
            for c in n.named_children(&mut cur) {
                stack.push(c);
            }
        }
        for (nm, c) in decl_counts {
            if c > file_scope_counts.get(&nm).copied().unwrap_or(1) {
                targets.remove(&nm);
            }
        }
        if targets.is_empty() {
            return;
        }
    }

    // Reader scan: for each scope, walk its subtree for identifier-like
    // reads of a target name; dedup per (reader, target); skip self and
    // same-name targets.
    let mut new_edges: Vec<Edge> = Vec::new();
    for scope in &scopes {
        let Some(scope_node) = tree
            .root_node()
            .named_descendant_for_byte_range(scope.start_byte, scope.end_byte)
        else {
            continue;
        };
        // (Dart/Pascal sibling-body pull — wave 2.)
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack = vec![scope_node];
        let mut visited = 0usize;
        while let Some(n) = stack.pop() {
            if visited >= MAX_VALUE_REF_NODES {
                break;
            }
            visited += 1;
            // `constant` covers Ruby, `name` PHP, `simple_identifier`
            // legacy-Kotlin shapes — a file only holds its own grammar's
            // kinds, so the union is safe.
            if matches!(
                n.kind(),
                "identifier" | "constant" | "name" | "simple_identifier"
            ) {
                let ref_name = get_node_text(n, source);
                if let Some(target_id) = targets.get(ref_name)
                    && target_id != &scope.id
                    && ref_name != scope.name
                    && !seen.contains(target_id.as_str())
                {
                    seen.insert(target_id.as_str());
                    new_edges.push(Edge {
                        source: scope.id.clone(),
                        target: target_id.clone(),
                        kind: EdgeKind::References,
                        metadata: Some(serde_json::json!({ "valueRef": true })),
                        line: None,
                        column: None,
                        provenance: Some(Provenance::TreeSitter),
                    });
                }
            }
            let mut cur = n.walk();
            for c in n.named_children(&mut cur) {
                stack.push(c);
            }
        }
    }
    for e in new_edges {
        s.add_edge(e);
    }
}
