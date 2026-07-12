//! The Rust shape of TS's `LanguageExtractor` (extraction-langs.md §Public
//! interface — field-for-field): a data struct for the node-type tables
//! ([`NodeTypeTables`]) plus a trait of hooks with inert defaults
//! ([`LanguageRules`]). Every capability the TS interface had exists here so
//! later language tasks only *fill in*, never reshape.
//!
//! Registry: [`rules_for`] — v0 wires Python (Task 5) and Go/Rust (Task 9);
//! TS/JS (Task 7/8), Java (10), Kotlin (11), C/C++ (13), C#/PHP/Ruby (14)
//! land per plan.

pub(crate) mod go;
pub(crate) mod java;
pub(crate) mod kotlin;
pub(crate) mod python;
pub(crate) mod rust_lang;

use selene_core::{NodeKind, Visibility};
use tree_sitter::Node;

use crate::Language;
use crate::walker::Session;

/// The `*Types` string tables + scalar knobs of a language config. All node
/// type names are grammar node kinds (`&'static` — configs are compile-time
/// data; Task 5 keeps string comparison, kind-id precompute is a profiled
/// later optimization per the Task 1 spike note).
#[derive(Debug, Clone, Copy)]
pub struct NodeTypeTables {
    pub function_types: &'static [&'static str],
    pub class_types: &'static [&'static str],
    pub method_types: &'static [&'static str],
    pub interface_types: &'static [&'static str],
    pub struct_types: &'static [&'static str],
    pub enum_types: &'static [&'static str],
    pub enum_member_types: &'static [&'static str],
    pub type_alias_types: &'static [&'static str],
    pub import_types: &'static [&'static str],
    pub call_types: &'static [&'static str],
    pub variable_types: &'static [&'static str],
    pub field_types: &'static [&'static str],
    pub property_types: &'static [&'static str],
    pub extra_class_node_types: &'static [&'static str],
    pub package_types: &'static [&'static str],
    pub name_field: &'static str,
    pub body_field: &'static str,
    pub params_field: &'static str,
    pub return_field: Option<&'static str>,
    /// Go: `func (r T) M()` is top-level, still a method.
    pub methods_are_top_level: bool,
    /// C++ (#1093): a bodiless class is a forward declaration, not a
    /// definition — skip it.
    pub skip_bodiless_class: bool,
    /// Rust/Scala: `Trait`; ObjC: `Protocol`. `None` = `Interface`.
    pub interface_kind: Option<NodeKind>,
}

impl NodeTypeTables {
    /// All-empty tables to spread language configs from (`..EMPTY`), so a
    /// config states only what it uses — the TS configs' implicit-undefined
    /// fields, made explicit.
    pub const EMPTY: NodeTypeTables = NodeTypeTables {
        function_types: &[],
        class_types: &[],
        method_types: &[],
        interface_types: &[],
        struct_types: &[],
        enum_types: &[],
        enum_member_types: &[],
        type_alias_types: &[],
        import_types: &[],
        call_types: &[],
        variable_types: &[],
        field_types: &[],
        property_types: &[],
        extra_class_node_types: &[],
        package_types: &[],
        name_field: "name",
        body_field: "body",
        params_field: "parameters",
        return_field: None,
        methods_are_top_level: false,
        skip_bodiless_class: false,
        interface_kind: None,
    };
}

/// `classifyClassNode` outcome — which structural kind a `classTypes` node
/// actually is (Swift reuses `class_declaration` for structs/enums, Kotlin
/// for interfaces/enums).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
}

/// `classifyMethodNode` outcome (#808): TS/JS class fields parse as a
/// method-type node; only function-valued fields are methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodClass {
    Method,
    Property,
}

/// `extractImport` hook result: `{moduleName, signature, handledRefs}`.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    pub module_name: String,
    pub signature: String,
    /// When true the hook already emitted the refs — the core skips its
    /// default `imports` UnresolvedReference.
    pub handled_refs: bool,
}

/// A language's extraction rules: the type tables plus every optional hook
/// of the TS `LanguageExtractor`, defaulted inert (return `None`/`false`/
/// empty — exactly the absent-closure behavior in TS).
#[allow(unused_variables)] // default impls deliberately ignore their inputs
pub trait LanguageRules: Sync {
    fn tables(&self) -> &'static NodeTypeTables;

    /// Byte-offset-preserving source transform (blank with spaces, keep
    /// newlines — positions feed node ids). `None` = no transform.
    fn pre_parse(&self, source: &str, file_path: &str) -> Option<String> {
        None
    }
    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
    /// C/C++ only: recover an identifier from a macro-mangled name. Identity
    /// by default (a clean name is never altered).
    fn recover_mangled_name(&self, name: String) -> String {
        name
    }
    fn extract_property_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
    // NOTE (Rust-shape deviation, uniform): the TS hooks read `node.text`
    // (WASM nodes own their text); native tree-sitter nodes don't, so every
    // node-inspecting hook takes `source` too.
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        None
    }
    fn is_exported(&self, node: Node<'_>, source: &str) -> Option<bool> {
        None
    }
    fn is_async(&self, node: Node<'_>, source: &str) -> Option<bool> {
        None
    }
    fn is_static(&self, node: Node<'_>, source: &str) -> Option<bool> {
        None
    }
    fn is_const(&self, node: Node<'_>, source: &str) -> Option<bool> {
        None
    }
    /// Extra symbol-level modifiers merged into `Node.decorators`
    /// (Kotlin `expect`/`actual`).
    fn extract_modifiers(&self, node: Node<'_>, source: &str) -> Vec<String> {
        Vec::new()
    }
    /// Custom visitor — runs FIRST in the dispatch ladder; `true` = this
    /// subtree is fully handled, the ladder never sees it.
    fn visit_node(&self, node: Node<'_>, session: &mut Session<'_>) -> bool {
        false
    }
    /// Compile-time member synthesis (Java Lombok). Runs after the class
    /// body walk, class still on the scope stack.
    fn synthesize_members(&self, class_node: Node<'_>, session: &mut Session<'_>) {}
    fn classify_class_node(&self, node: Node<'_>, source: &str) -> Option<ClassKind> {
        None
    }
    fn classify_method_node(&self, node: Node<'_>, source: &str) -> Option<MethodClass> {
        None
    }
    /// Some grammars (Dart) model the body as a *sibling* of the signature.
    fn resolve_body<'t>(&self, node: Node<'t>, body_field: &str) -> Option<Node<'t>> {
        None
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        None
    }
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
    fn resolve_type_alias_kind(&self, node: Node<'_>, source: &str) -> Option<NodeKind> {
        None
    }
    fn is_misparsed_function(&self, name: &str, node: Node<'_>) -> bool {
        false
    }
    /// Ruby/Dart: statement-level bare-call extraction (Task 14/wave 2).
    fn extract_bare_call(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        None
    }
}

/// The v0 rules registry. `None` = language detects but has no extraction
/// rules yet (wave-2, or Task 6+ for the remaining v0 languages) —
/// extraction skips with an `unsupported_language` warning.
pub fn rules_for(l: Language) -> Option<&'static dyn LanguageRules> {
    match l {
        Language::Python => Some(&python::PythonRules),
        Language::Go => Some(&go::GoRules),
        Language::Rust => Some(&rust_lang::RustRules),
        Language::Java => Some(&java::JavaRules),
        Language::Kotlin => Some(&kotlin::KotlinRules),
        _ => None,
    }
}

/// Whether the session's current scope (stack top) is a class-like node —
/// mirrors the walker's private ladder gate, resolved through the public
/// [`Session::nodes`] surface. Shared by hook-hosted branches (Rust
/// const/static fallback, Java field extraction, Kotlin property scoping)
/// that must replicate the ladder's `is_inside_class_like` gating from
/// inside `visit_node`. (A utility over existing hooks, not a new hook —
/// flagged in the Task 9–11 report for the core chain's awareness.)
pub(crate) fn scope_is_class_like(s: &Session<'_>) -> bool {
    let Some(top) = s.node_stack().last() else {
        return false;
    };
    s.nodes()
        .iter()
        .rev()
        .find(|n| &n.id == top)
        .is_some_and(|n| {
            matches!(
                n.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Interface
                    | NodeKind::Trait
                    | NodeKind::Enum
                    | NodeKind::Module
            )
        })
}
