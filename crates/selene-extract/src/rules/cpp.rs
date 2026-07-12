//! C++ rules — verbatim port of `c-cpp.ts`'s `cppExtractor` (map §11 C++
//! row): the five-blanker pre-parse chain in EXACT order (+ Metal/CUDA
//! gates), qualified-method name/receiver extraction via the declarator
//! BFS, macro-defined-name recovery (`DEFINE_KERNEL(real_name, …)`),
//! misparse guards (#946/#1061/#1093), and typedef/using aliases.

use std::collections::VecDeque;

use selene_core::{NodeKind, Visibility};
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::c::{extract_c_include, resolve_c_typedef_kind};
use crate::rules::cpp_preparse::{
    blank_cpp_annotation_macro_calls, blank_cpp_api_prefix_macros, blank_cpp_export_macros,
    blank_cpp_inline_annotation_macros, blank_cpp_inline_macros, blank_cuda_constructs,
    blank_metal_attributes, looks_like_cuda_source, normalize_cpp_return_type,
    recover_mangled_cpp_name,
};
use crate::rules::{ImportInfo, LanguageRules, NodeTypeTables};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["function_definition"],
    class_types: &["class_specifier"],
    // A bodiless class_specifier is a forward declaration / elaborated type
    // reference — dozens of forward decls must not mint phantom classes
    // that crowd out the one real definition (#1093).
    skip_bodiless_class: true,
    method_types: &["function_definition"],
    struct_types: &["struct_specifier"],
    enum_types: &["enum_specifier"],
    enum_member_types: &["enumerator"],
    type_alias_types: &["type_definition", "alias_declaration"], // typedef + using
    import_types: &["preproc_include"],
    call_types: &["call_expression"],
    variable_types: &["declaration"],
    name_field: "declarator",
    body_field: "body",
    params_field: "parameters",
    return_field: None,
    ..NodeTypeTables::EMPTY
};

/// BFS for the `qualified_identifier` under a declarator, skipping
/// `parameter_list`/`trailing_return_type` subtrees (their types are not
/// the function name).
fn find_declarator_qualified_id<'t>(declarator: Node<'t>) -> Option<Node<'t>> {
    let mut queue: VecDeque<Node<'t>> = VecDeque::from([declarator]);
    while let Some(current) = queue.pop_front() {
        if current.kind() == "qualified_identifier" {
            return Some(current);
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            if child.kind() != "parameter_list" && child.kind() != "trailing_return_type" {
                queue.push_back(child);
            }
        }
    }
    None
}

/// `MACRO_NAME(real_name, typed args…) { body }` recovery — deliberately
/// narrow (ALL of): macro-shaped parsed name (ALL-CAPS, ≥1 underscore);
/// first param a LONE type_identifier containing a lowercase letter; ≥2
/// params and NO other lone-ident param (gtest `TEST_F(Fixture, Name)`,
/// `PYBIND11_MODULE(ext, m)` all bail).
fn recover_cpp_macro_defined_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "function_definition" {
        return None;
    }
    let declarator = get_child_by_field(node, "declarator")?;
    if declarator.kind() != "function_declarator" {
        return None;
    }
    let inner = get_child_by_field(declarator, "declarator")?;
    if inner.kind() != "identifier" {
        return None;
    }
    let macro_name = get_node_text(inner, source);
    if !is_macro_shaped(macro_name) {
        return None;
    }
    let params = get_child_by_field(declarator, "parameters")?;
    if params.named_child_count() < 2 {
        return None;
    }
    let lone_ident_text = |p: Node<'_>| -> Option<&str> {
        if p.kind() == "parameter_declaration"
            && p.named_child_count() == 1
            && p.named_child(0)
                .is_some_and(|c| c.kind() == "type_identifier")
        {
            p.named_child(0).map(|c| get_node_text(c, source))
        } else {
            None
        }
    };
    let first = params.named_child(0)?;
    let name = lone_ident_text(first)?;
    if !name.chars().any(|c| c.is_ascii_lowercase()) {
        return None;
    }
    for i in 1..u32::try_from(params.named_child_count()).unwrap_or(0) {
        if let Some(p) = params.named_child(i)
            && lone_ident_text(p).is_some()
        {
            return None; // a second bare arg means the first isn't the name
        }
    }
    Some(name.to_string())
}

/// `/^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$/` without a regex.
fn is_macro_shaped(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && name.contains('_')
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with('_')
        && !name.ends_with('_')
        && !name.contains("__")
}

/// The blanker chain, EXACT TS order: export-macros → inline-macros →
/// api-prefix → inline-annotation → annotation-calls; then the `.metal`
/// blanker, or the CUDA blanker for `.cu`/`.cuh`/content-sniffed CUDA
/// (the sniff runs on the ORIGINAL source, as in TS).
fn pre_parse_cpp(source: &str, file_path: &str) -> Option<String> {
    let mut cur: String = blank_cpp_export_macros(source).into_owned();
    cur = blank_cpp_inline_macros(&cur).into_owned();
    cur = blank_cpp_api_prefix_macros(&cur).into_owned();
    cur = blank_cpp_inline_annotation_macros(&cur).into_owned();
    cur = blank_cpp_annotation_macro_calls(&cur).into_owned();

    let lower = file_path.to_lowercase();
    if lower.ends_with(".metal") {
        cur = blank_metal_attributes(&cur).into_owned();
    } else if lower.ends_with(".cu") || lower.ends_with(".cuh") || looks_like_cuda_source(source) {
        cur = blank_cuda_constructs(&cur).into_owned();
    }

    if cur == source { None } else { Some(cur) }
}

pub(crate) struct CppRules;

impl LanguageRules for CppRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    fn pre_parse(&self, source: &str, file_path: &str) -> Option<String> {
        pre_parse_cpp(source, file_path)
    }

    fn recover_mangled_name(&self, name: String) -> String {
        recover_mangled_cpp_name(name)
    }

    /// Macro-defined-name recovery first, else the qualified-id BFS's last
    /// `::` segment (`int Widget::size() const` names `size`).
    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if let Some(macro_defined) = recover_cpp_macro_defined_name(node, source) {
            return Some(macro_defined);
        }
        let declarator = get_child_by_field(node, "declarator")?;
        let qid = find_declarator_qualified_id(declarator)?;
        let parts: Vec<&str> = get_node_text(qid, source)
            .trim()
            .split("::")
            .filter(|p| !p.is_empty())
            .collect();
        parts.last().map(|p| (*p).to_string())
    }

    /// Out-of-line method receiver: the qualifier segments joined `::`
    /// (`int a::b::C::f()` → receiver `a::b::C`).
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let declarator = get_child_by_field(node, "declarator")?;
        let qid = find_declarator_qualified_id(declarator)?;
        let parts: Vec<&str> = get_node_text(qid, source)
            .trim()
            .split("::")
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() > 1 {
            Some(parts[..parts.len() - 1].join("::"))
        } else {
            None
        }
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let type_node = get_child_by_field(node, "type")?;
        normalize_cpp_return_type(get_node_text(type_node, source))
    }

    /// Access specifier scanned from the PARENT's children. TS-parity note:
    /// the FIRST `access_specifier` in the parent wins regardless of its
    /// position relative to the member (TS returns on first match) — a
    /// known coarse approximation, ported as-is rather than "fixed".
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let parent = node.parent()?;
        for i in 0..u32::try_from(parent.child_count()).unwrap_or(0) {
            let Some(child) = parent.child(i) else {
                continue;
            };
            if child.kind() == "access_specifier" {
                let text = get_node_text(child, source);
                if text.contains("public") {
                    return Some(Visibility::Public);
                }
                if text.contains("private") {
                    return Some(Visibility::Private);
                }
                if text.contains("protected") {
                    return Some(Visibility::Protected);
                }
            }
        }
        None
    }

    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        resolve_c_typedef_kind(node)
    }

    /// Misparse guards: `namespace`-prefixed names (macro-confused
    /// namespace blocks), bare C++ keywords, and the macro-misparsed type
    /// declaration shape (#946/#1061 fallback).
    fn is_misparsed_function(&self, name: &str, node: Node<'_>) -> bool {
        if name.starts_with("namespace") {
            return true;
        }
        if matches!(
            name,
            "switch" | "if" | "for" | "while" | "do" | "case" | "return"
        ) {
            return true;
        }
        is_macro_misparsed_type_decl(node)
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        extract_c_include(node, source)
    }
}

/// `class MACRO Name { … }` misparse (#946): the `type` field is a BODILESS
/// class/struct specifier (an elaborated type, not a real inline-defined
/// return type) AND the declarator is not a function_declarator (a real
/// definition always has one). The body is unrecoverable — drop the node.
fn is_macro_misparsed_type_decl(node: Node<'_>) -> bool {
    let Some(type_node) = get_child_by_field(node, "type") else {
        return false;
    };
    if type_node.kind() != "class_specifier" && type_node.kind() != "struct_specifier" {
        return false;
    }
    let mut cursor = type_node.walk();
    if type_node
        .named_children(&mut cursor)
        .any(|c| c.kind() == "field_declaration_list")
    {
        return false;
    }
    let declarator = get_child_by_field(node, "declarator");
    !declarator.is_some_and(|d| d.kind() == "function_declarator")
}
