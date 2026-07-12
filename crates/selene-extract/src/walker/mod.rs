//! The generic tree-sitter walker — the Rust port of `tree-sitter.ts`'s
//! `TreeSitterExtractor` (extraction-core.md §8). Task 5 lands the
//! DECLARATION ladder + [`Session`] (the `ExtractorContext` port); later
//! tasks fill the named insertion points below — coordinate via the plan's
//! sequencing note (Tasks 7, 8, 13, 15a all edit this file, strictly
//! sequentially):
//!
//! - `INSERTION POINT (Task 6)`  — call/instantiation branches at the
//!   ladder tail + the real [`Session::visit_function_body`] body walker
//!   (`src/walker/body.rs`).
//! - `INSERTION POINT (Task 7/8)` — TS/JS branches (arrow-const naming, HOC
//!   components, re-export/import-binding refs, store collections,
//!   type-annotation refs, inheritance).
//! - `INSERTION POINT (Task 13)` — C++ `namespace_definition` prefix branch
//!   (pushes [`Session::namespace_prefix`], mints no node).
//! - `INSERTION POINT (Task 15a)` — function-as-value candidate capture.
//!
//! ## Column convention note (vs the TS build)
//!
//! web-tree-sitter (the TS build) counted column positions in UTF-16 code
//! units; native tree-sitter counts UTF-8 **bytes** — column values on
//! non-ASCII lines may differ from the TS build. Node ids
//! (`path:kind:name:line`) embed the 1-based START LINE only, so ids and
//! the Task 19 count gate are unaffected.
//!
//! ## Determinism
//!
//! Walk order is the grammar's named-child order (deterministic); the ONE
//! wall-clock read is `updated_at`, captured once per extraction and
//! stamped on every node (TS read `Date.now()` per node; a single capture
//! is within the Global Constraints and steadier).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use selene_core::{Edge, EdgeKind, Node as CoreNode, NodeKind, Provenance, file_node_id, node_id};
use tree_sitter::{Node, Parser};

use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::{ClassKind, LanguageRules, MethodClass, rules_for};
use crate::{
    ErrorCode, ExtractionError, ExtractionResult, Language, Severity, UnresolvedReference,
    grammars::grammar_for, is_file_level_only,
};

/// The extraction session — the `ExtractorContext` port. Owns the output
/// buffers, the scope stack, and an id→(kind, name) map that makes
/// qualified-name building and class-like checks O(1) (the TS
/// `nodes.find()` per lookup was O(n²) over a file; same observable
/// behavior).
pub struct Session<'s> {
    file_path: &'s str,
    source: &'s str,
    language: Language,
    nodes: Vec<CoreNode>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
    /// Scope stack of node IDS (file node at the bottom).
    node_stack: Vec<String>,
    /// id → (kind, name) for every created node.
    id_index: HashMap<String, (NodeKind, String)>,
    /// C++ namespace prefix stack — INSERTION POINT (Task 13) pushes here.
    namespace_prefix: Vec<String>,
    /// The one wall-clock read (ms), captured at session start.
    updated_at: i64,
}

impl<'s> Session<'s> {
    fn new(file_path: &'s str, source: &'s str, language: Language) -> Self {
        let updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Session {
            file_path,
            source,
            language,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved: Vec::new(),
            errors: Vec::new(),
            node_stack: Vec::new(),
            id_index: HashMap::new(),
            namespace_prefix: Vec::new(),
            updated_at,
        }
    }

    pub fn file_path(&self) -> &str {
        self.file_path
    }
    pub fn source(&self) -> &str {
        self.source
    }
    pub fn node_stack(&self) -> &[String] {
        &self.node_stack
    }
    pub fn nodes(&self) -> &[CoreNode] {
        &self.nodes
    }
    /// Mutable access to a prior node — the Lombok taken-name scan and
    /// Erlang-style endLine mutation (wave 2) need read+mutate on already
    /// created nodes.
    pub fn node_mut(&mut self, idx: usize) -> Option<&mut CoreNode> {
        self.nodes.get_mut(idx)
    }
    pub fn push_scope(&mut self, node_id: String) {
        self.node_stack.push(node_id);
    }
    pub fn pop_scope(&mut self) {
        self.node_stack.pop();
    }
    pub fn add_unresolved(&mut self, r: UnresolvedReference) {
        self.unresolved.push(r);
    }
    pub fn add_edge(&mut self, e: Edge) {
        self.edges.push(e);
    }

    /// The current scope's node id (stack top).
    fn scope_id(&self) -> Option<&String> {
        self.node_stack.last()
    }

    /// `qualifiedName` = C++ namespace prefix, then names of non-file stack
    /// nodes, then `name`, joined `::` — never a file-path component.
    fn build_qualified_name(&self, name: &str) -> String {
        let mut parts: Vec<&str> = self.namespace_prefix.iter().map(String::as_str).collect();
        for id in &self.node_stack {
            if let Some((kind, n)) = self.id_index.get(id)
                && *kind != NodeKind::File
            {
                parts.push(n);
            }
        }
        parts.push(name);
        parts.join("::")
    }

    /// Whether the current scope is class-like (class/struct/interface/
    /// trait/enum/module). File nodes are not class-like.
    fn is_inside_class_like(&self) -> bool {
        let Some(id) = self.scope_id() else {
            return false;
        };
        matches!(
            self.id_index.get(id).map(|(k, _)| *k),
            Some(
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Interface
                    | NodeKind::Trait
                    | NodeKind::Enum
                    | NodeKind::Module
            )
        )
    }

    /// Create a node: skip empty names (`None` — #42, an empty-named node
    /// would take FK-violating edges); id from `selene_core::ids::node_id`
    /// with the **1-based** start line (`row + 1` — the 0→1 boundary, pinned
    /// by `first_line_symbol_gets_line_1_in_its_id`); qualified name from
    /// the scope stack; `contains` edge from the stack top (provenance
    /// TreeSitter); `extract_modifiers` merged into `decorators`.
    /// Returns the created node's index into [`Session::nodes`].
    pub fn create_node(
        &mut self,
        rules: &'static dyn LanguageRules,
        kind: NodeKind,
        name: &str,
        node: Node<'_>,
        extra: NodeExtra,
    ) -> Option<usize> {
        if name.is_empty() {
            return None;
        }
        let start_line = u32::try_from(node.start_position().row).unwrap_or(u32::MAX - 1) + 1;
        let id = node_id(self.file_path, kind, name, start_line);

        // Sibling-body grammars (Dart): extend endLine to the resolved body
        // when it sits beyond the node. Only ever extends.
        let mut end_line = u32::try_from(node.end_position().row).unwrap_or(u32::MAX - 1) + 1;
        if (kind == NodeKind::Function || kind == NodeKind::Method)
            && let Some(body) = rules.resolve_body(node, rules.tables().body_field)
        {
            let body_end = u32::try_from(body.end_position().row).unwrap_or(u32::MAX - 1) + 1;
            if body_end > end_line {
                end_line = body_end;
            }
        }

        let qualified_name = extra
            .qualified_name
            .unwrap_or_else(|| self.build_qualified_name(name));

        let mut decorators = extra.decorators;
        let mods = rules.extract_modifiers(node, self.source);
        decorators.extend(mods);

        let core = CoreNode {
            id: id.clone(),
            kind,
            name: name.to_string(),
            qualified_name,
            file_path: self.file_path.to_string(),
            language: self.language.as_str().to_string(),
            start_line,
            end_line,
            start_column: u32::try_from(node.start_position().column).unwrap_or(u32::MAX),
            end_column: u32::try_from(node.end_position().column).unwrap_or(u32::MAX),
            docstring: extra.docstring,
            signature: extra.signature,
            visibility: extra.visibility,
            is_exported: extra.is_exported,
            is_async: extra.is_async,
            is_static: extra.is_static,
            is_abstract: None,
            decorators,
            type_parameters: Vec::new(),
            return_type: extra.return_type,
            updated_at: self.updated_at,
        };

        if let Some(parent_id) = self.scope_id().cloned() {
            self.edges.push(Edge {
                source: parent_id,
                target: id.clone(),
                kind: EdgeKind::Contains,
                metadata: None,
                line: None,
                column: None,
                provenance: Some(Provenance::TreeSitter),
            });
        }

        self.id_index.insert(id, (kind, name.to_string()));
        self.nodes.push(core);
        Some(self.nodes.len() - 1)
    }

    /// The id of the node at `idx`.
    fn id_of(&self, idx: usize) -> Option<String> {
        self.nodes.get(idx).map(|n| n.id.clone())
    }

    /// Body walker — INSERTION POINT (Task 6): calls, instantiations, bare
    /// calls, static member reads, nested named functions and structural
    /// types, value-ref capture (`src/walker/body.rs`). Task 5: no-op —
    /// declaration extraction never descends into function bodies, exactly
    /// the subset of TS behavior this task ports.
    pub fn visit_function_body(&mut self, _body: Node<'_>, _fn_id: &str) {
        // Task 6 lands the real walker here.
    }
}

/// The optional per-node attributes `createNode` merges (`extra` in TS).
#[derive(Default)]
pub struct NodeExtra {
    pub docstring: Option<String>,
    pub signature: Option<String>,
    pub visibility: Option<selene_core::Visibility>,
    pub is_exported: Option<bool>,
    pub is_async: Option<bool>,
    pub is_static: Option<bool>,
    pub return_type: Option<String>,
    pub qualified_name: Option<String>,
    pub decorators: Vec<String>,
}

/// One extraction pass over `source` (the `extractFromSource` port —
/// declarations subset, Task 5). Errors are collected, never thrown; a
/// language without v0 rules yields an `unsupported_language` result
/// (Warning for known wave-2 languages, Error for [`Language::Unknown`] —
/// the TS severity split).
pub fn extract_from_source(file_path: &str, source: &str, language: Language) -> ExtractionResult {
    let started = std::time::Instant::now();
    let mut result = ExtractionResult::default();

    // File-level-only languages: indexed as files, no symbol extraction.
    if is_file_level_only(language) {
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    }

    let (Some(rules), Some(grammar)) = (rules_for(language), grammar_for(language)) else {
        result.errors.push(ExtractionError {
            message: format!("Unsupported language: {}", language.as_str()),
            severity: if language == Language::Unknown {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: ErrorCode::UnsupportedLanguage,
            file_path: Some(file_path.to_string()),
        });
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    };

    // Optional byte-offset-preserving pre-parse transform; downstream text
    // reads use the same bytes the parser saw.
    let transformed = rules.pre_parse(source, file_path);
    let source: &str = transformed.as_deref().unwrap_or(source);

    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        result.errors.push(ExtractionError {
            message: format!("Failed to build parser for language: {}", language.as_str()),
            severity: Severity::Error,
            code: ErrorCode::ParserError,
            file_path: Some(file_path.to_string()),
        });
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    }
    let Some(tree) = parser.parse(source, None) else {
        result.errors.push(ExtractionError {
            message: "Parse error: parser returned no tree".to_string(),
            severity: Severity::Error,
            code: ErrorCode::ParseError,
            file_path: Some(file_path.to_string()),
        });
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    };

    let mut s = Session::new(file_path, source, language);

    // File node: unhashed literal id, name = basename, qualifiedName = the
    // file path (the one deliberate path-valued qualifiedName), endLine =
    // line count (split('\n') semantics: trailing newline adds a line).
    let basename = file_path.rsplit('/').next().unwrap_or(file_path);
    let file_node = CoreNode {
        id: file_node_id(file_path),
        kind: NodeKind::File,
        name: basename.to_string(),
        qualified_name: file_path.to_string(),
        file_path: file_path.to_string(),
        language: language.as_str().to_string(),
        start_line: 1,
        end_line: u32::try_from(source.split('\n').count()).unwrap_or(u32::MAX),
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: Some(false),
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        updated_at: s.updated_at,
    };
    s.id_index
        .insert(file_node.id.clone(), (NodeKind::File, basename.to_string()));
    s.node_stack.push(file_node.id.clone());
    s.nodes.push(file_node);

    // Package header (Java/Kotlin/Erlang) → implicit `namespace` node
    // wrapping every top-level declaration.
    let package_idx = extract_file_package(rules, &mut s, tree.root_node());
    if let Some(idx) = package_idx
        && let Some(id) = s.id_of(idx)
    {
        s.node_stack.push(id);
    }

    visit(rules, &mut s, tree.root_node());

    // INSERTION POINT (Task 15a): flush function-as-value candidates.
    // INSERTION POINT (Task 6): flush value refs.

    if package_idx.is_some() {
        s.node_stack.pop();
    }
    s.node_stack.pop();

    result.nodes = s.nodes;
    result.edges = s.edges;
    result.unresolved = s.unresolved;
    result.errors = s.errors;
    result.duration_ms = started.elapsed().as_millis() as u64;
    result
}

/// `extractFilePackage`: first `package_types` child under the root → a
/// `namespace` node; caller scopes top-level declarations underneath.
fn extract_file_package(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    root: Node<'_>,
) -> Option<usize> {
    let types = rules.tables().package_types;
    if types.is_empty() {
        return None;
    }
    let mut cursor = root.walk();
    let pkg = root
        .named_children(&mut cursor)
        .find(|c| types.contains(&c.kind()))?;
    let name = rules.extract_package(pkg, s.source())?;
    s.create_node(rules, NodeKind::Namespace, &name, pkg, NodeExtra::default())
}

/// `extractName`: resolve_name hook, else the `name_field` child (C/C++
/// declarator unwrapping arrives with Task 13), else `<anonymous>`; passed
/// through `recover_mangled_name` (identity by default).
fn extract_name(rules: &'static dyn LanguageRules, node: Node<'_>, source: &str) -> String {
    let raw = rules
        .resolve_name(node, source)
        .or_else(|| {
            get_child_by_field(node, rules.tables().name_field)
                .map(|n| get_node_text(n, source).to_string())
        })
        .unwrap_or_else(|| "<anonymous>".to_string());
    rules.recover_mangled_name(raw)
}

/// `resolveBody` hook, else the `body_field` child.
fn resolve_body<'t>(rules: &'static dyn LanguageRules, node: Node<'t>) -> Option<Node<'t>> {
    rules
        .resolve_body(node, rules.tables().body_field)
        .or_else(|| get_child_by_field(node, rules.tables().body_field))
}

/// The dispatch ladder (map §8, exact order; first match wins; matched
/// branches handle their own descent).
fn visit(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let t = rules.tables();
    let node_type = node.kind();

    // 1. Custom visit_node hook — short-circuits the whole ladder.
    if rules.visit_node(node, s) {
        return;
    }

    // INSERTION POINT (Task 13): C++ `namespace_definition` prefix branch.
    // INSERTION POINT (Task 15a): function-as-value candidate capture.

    let mut matched = true;
    if t.function_types.contains(&node_type) {
        if s.is_inside_class_like() && t.method_types.contains(&node_type) {
            extract_method(rules, s, node);
        } else {
            extract_function(rules, s, node);
        }
    } else if t.class_types.contains(&node_type) {
        match rules.classify_class_node(node, s.source()) {
            Some(ClassKind::Struct) => extract_struct(rules, s, node),
            Some(ClassKind::Enum) => extract_enum(rules, s, node),
            Some(ClassKind::Interface) => extract_interface(rules, s, node),
            Some(ClassKind::Trait) => extract_class(rules, s, node, NodeKind::Trait),
            _ => extract_class(rules, s, node, NodeKind::Class),
        }
    } else if t.extra_class_node_types.contains(&node_type) {
        extract_class(rules, s, node, NodeKind::Class);
    } else if t.method_types.contains(&node_type) {
        // TS/JS #808: a field-shaped method node may be a plain property.
        if rules.classify_method_node(node, s.source()) == Some(MethodClass::Property) {
            extract_property(rules, s, node);
        } else {
            extract_method(rules, s, node);
        }
    } else if t.interface_types.contains(&node_type) {
        extract_interface(rules, s, node);
    } else if t.struct_types.contains(&node_type) {
        extract_struct(rules, s, node);
    } else if t.enum_types.contains(&node_type) {
        extract_enum(rules, s, node);
    } else if t.type_alias_types.contains(&node_type) {
        extract_type_alias(rules, s, node);
    } else if t.property_types.contains(&node_type) && s.is_inside_class_like() {
        extract_property(rules, s, node);
    } else if t.field_types.contains(&node_type) && s.is_inside_class_like() {
        extract_field(rules, s, node);
    } else if t.variable_types.contains(&node_type) && !s.is_inside_class_like() {
        // (Ruby class-scope `CONST =` gate arrives with Task 14.)
        extract_variable(rules, s, node);
    } else if t.import_types.contains(&node_type) {
        extract_import(rules, s, node);
    } else {
        matched = false;
    }
    // INSERTION POINT (Task 7/8): TS/JS branches (re-export, store
    // collections, interface member type refs) slot in above this line.
    // INSERTION POINT (Task 6): call_types + INSTANTIATION_KINDS branches.

    if matched {
        return; // matched branches walked (or deliberately skipped) children
    }

    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
}

fn extract_function(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    // Receiver-typed functions (Rust impl fns, Task 9) route to method.
    if rules.get_receiver_type(node, s.source()).is_some() {
        extract_method(rules, s, node);
        return;
    }

    let name = extract_name(rules, node, s.source());
    // (TS/JS arrow/function-expression declarator naming — Task 7.)
    if name == "<anonymous>" || rules.is_misparsed_function(&name, node) {
        // No node, but the body is still walked (module wrappers #528) —
        // body walking lands in Task 6.
        if let Some(body) = resolve_body(rules, node) {
            s.visit_function_body(body, "");
        }
        return;
    }

    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        signature: rules.get_signature(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        is_async: rules.is_async(node, s.source()),
        is_static: rules.is_static(node, s.source()),
        return_type: rules.get_return_type(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(rules, NodeKind::Function, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    extract_decorators_for(s, node, &id);

    s.push_scope(id.clone());
    if let Some(body) = resolve_body(rules, node) {
        s.visit_function_body(body, &id);
    }
    s.pop_scope();
}

fn extract_method(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let receiver = rules.get_receiver_type(node, s.source());

    if !s.is_inside_class_like() && !rules.tables().methods_are_top_level && receiver.is_none() {
        // (Object-literal method skip — TS/JS, Task 7.) Not in a class and
        // no receiver: it's a function.
        extract_function(rules, s, node);
        return;
    }

    let name = extract_name(rules, node, s.source());
    if rules.is_misparsed_function(&name, node) {
        if let Some(body) = resolve_body(rules, node) {
            s.visit_function_body(body, "");
        }
        return;
    }

    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        signature: rules.get_signature(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_async: rules.is_async(node, s.source()),
        is_static: rules.is_static(node, s.source()),
        return_type: rules.get_return_type(node, s.source()),
        // Receiver methods: `Receiver::name` qualified-name override.
        qualified_name: receiver.as_ref().map(|r| format!("{r}::{name}")),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(rules, NodeKind::Method, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // Receiver method with no class-like parent (Rust impl blocks, Task 9):
    // contains edge from the same-file owner node found by name.
    if let Some(recv) = receiver
        && !s.is_inside_class_like()
    {
        let owner = s.nodes.iter().find(|n| {
            n.name == recv
                && matches!(
                    n.kind,
                    NodeKind::Struct | NodeKind::Class | NodeKind::Enum | NodeKind::Trait
                )
        });
        if let Some(owner_id) = owner.map(|o| o.id.clone()) {
            s.add_edge(Edge {
                source: owner_id,
                target: id.clone(),
                kind: EdgeKind::Contains,
                metadata: None,
                line: None,
                column: None,
                provenance: Some(Provenance::TreeSitter),
            });
        }
    }

    extract_decorators_for(s, node, &id);

    s.push_scope(id.clone());
    if let Some(body) = resolve_body(rules, node) {
        s.visit_function_body(body, &id);
    }
    s.pop_scope();
}

fn extract_class(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
    kind: NodeKind,
) {
    let body = resolve_body(rules, node);
    if rules.tables().skip_bodiless_class && body.is_none() {
        return; // forward declaration (#1093)
    }

    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(rules, kind, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // INSERTION POINT (Task 7): extract_inheritance (extends/implements refs).
    extract_decorators_for(s, node, &id);

    s.push_scope(id);
    let body = body.unwrap_or(node);
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
    // Lombok synthesis (Task 10) runs here, class still on the stack.
    rules.synthesize_members(node, s);
    s.pop_scope();
}

fn extract_interface(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let name = extract_name(rules, node, s.source());
    let kind = rules.tables().interface_kind.unwrap_or(NodeKind::Interface);
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(rules, kind, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // INSERTION POINT (Task 7): interface inheritance refs.

    s.push_scope(id);
    let body = resolve_body(rules, node).unwrap_or(node);
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
    s.pop_scope();
}

fn extract_struct(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    // No body = forward declaration / type reference, not a definition.
    let Some(body) = resolve_body(rules, node) else {
        return;
    };
    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(rules, NodeKind::Struct, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    s.push_scope(id);
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
    s.pop_scope();
}

fn extract_enum(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let Some(body) = resolve_body(rules, node) else {
        return;
    };
    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(rules, NodeKind::Enum, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    s.push_scope(id);
    let member_types = rules.tables().enum_member_types;
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        if member_types.contains(&child.kind()) {
            extract_enum_members(rules, s, child);
        } else {
            visit(rules, s, child);
        }
    }
    s.pop_scope();
}

/// Enum member names: `name` field first (Rust enum_variant), else
/// identifier-like children (multi-case declarations), else the node itself
/// when it is a bare identifier.
fn extract_enum_members(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    if let Some(name_node) = get_child_by_field(node, "name") {
        let name = get_node_text(name_node, s.source()).to_string();
        s.create_node(
            rules,
            NodeKind::EnumMember,
            &name,
            node,
            NodeExtra::default(),
        );
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let mut found = false;
    for child in children {
        if matches!(
            child.kind(),
            "simple_identifier" | "identifier" | "property_identifier"
        ) {
            let name = get_node_text(child, s.source()).to_string();
            s.create_node(
                rules,
                NodeKind::EnumMember,
                &name,
                child,
                NodeExtra::default(),
            );
            found = true;
        }
    }
    if !found && node.named_child_count() == 0 {
        let name = get_node_text(node, s.source()).to_string();
        s.create_node(
            rules,
            NodeKind::EnumMember,
            &name,
            node,
            NodeExtra::default(),
        );
    }
}

/// Type alias (`type X = ...`) — `resolve_type_alias_kind` may reclassify
/// (Go `type_spec` wrapping struct/interface, Task 9); the struct/interface
/// re-dispatch arrives with that task.
fn extract_type_alias(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let kind = rules
        .resolve_type_alias_kind(node, s.source())
        .unwrap_or(NodeKind::TypeAlias);
    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    s.create_node(rules, kind, &name, node, extra);
}

/// Class property (C# property_declaration and TS/JS #808 demotions).
fn extract_property(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
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
    let Some(name) = name else { return };

    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_static: rules.is_static(node, s.source()),
        ..NodeExtra::default()
    };
    s.create_node(rules, NodeKind::Property, &name, node, extra);
    // (Type-annotation refs + decorator capture on properties — Task 7.)
}

/// Class field declarations (Java/C#/PHP shapes — Task 10/14). The generic
/// declarator scan only; language-specific wrappers land with their tasks.
fn extract_field(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let docstring = get_preceding_docstring(node, s.source());
    let visibility = rules.get_visibility(node, s.source());
    let is_static = rules.is_static(node, s.source());

    let mut cursor = node.walk();
    let declarators: Vec<Node<'_>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .collect();
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
        s.create_node(rules, NodeKind::Field, &name, d, extra);
    }
}

/// Top-level variable declarations. Task 5 ports the Python/Ruby shape
/// (`assignment`: left identifier/constant, `= <first 100 chars>`
/// signature); TS/JS declarators and Go specs land with Tasks 7/9.
fn extract_variable(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let kind = if rules.is_const(node, s.source()).unwrap_or(false) {
        NodeKind::Constant
    } else {
        NodeKind::Variable
    };
    let docstring = get_preceding_docstring(node, s.source());

    let left = get_child_by_field(node, "left").or_else(|| node.named_child(0));
    let right = get_child_by_field(node, "right").or_else(|| node.named_child(1));
    let Some(left) = left else { return };
    if left.kind() != "identifier" && left.kind() != "constant" {
        return;
    }
    let name = get_node_text(left, s.source()).to_string();
    let signature = right.map(|r| {
        let init: String = get_node_text(r, s.source()).chars().take(100).collect();
        let ellipsis = if init.chars().count() >= 100 {
            "..."
        } else {
            ""
        };
        format!("= {init}{ellipsis}")
    });
    let extra = NodeExtra {
        docstring,
        signature,
        ..NodeExtra::default()
    };
    s.create_node(rules, kind, &name, node, extra);
}

/// Imports: hook first (single-module languages); Python inline
/// multi-import + from-import per-name refs are core machinery (map §11).
fn extract_import(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let import_text = get_node_text(node, s.source()).trim().to_string();

    if let Some(info) = rules.extract_import(node, s.source()) {
        let extra = NodeExtra {
            signature: Some(info.signature.clone()),
            ..NodeExtra::default()
        };
        s.create_node(rules, NodeKind::Import, &info.module_name, node, extra);
        if !info.handled_refs
            && !info.module_name.is_empty()
            && let Some(parent_id) = s.scope_id().cloned()
        {
            s.add_unresolved(UnresolvedReference {
                from_node_id: parent_id,
                reference_name: info.module_name.clone(),
                reference_kind: EdgeKind::Imports.as_str().to_string(),
                line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
                column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
                file_path: None,
                language: None,
            });
        }
        // INSERTION POINT (Task 7): TS/JS import-binding refs.
        // Python `from m import X, Y` per-name refs:
        if s.language == Language::Python
            && node.kind() == "import_from_statement"
            && let Some(parent_id) = s.scope_id().cloned()
        {
            emit_py_from_import_refs(s, node, &parent_id);
        }
        // INSERTION POINT (Task 9): Rust use-binding refs.
        // INSERTION POINT (Task 14): PHP use refs, Ruby require refs.
        return;
        // Hook returning None means "I didn't handle this" — fall through to
        // the inline multi-import handlers only, never a generic fallback.
    }

    // Python `import a, b` / `import numpy as np`: one import node +
    // `imports` ref per dotted_name / aliased_import.
    if s.language == Language::Python && node.kind() == "import_statement" {
        let parent_id = s.scope_id().cloned();
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            let dotted = match child.kind() {
                "dotted_name" => Some(child),
                "aliased_import" => {
                    let mut c2 = child.walk();
                    child
                        .named_children(&mut c2)
                        .find(|c| c.kind() == "dotted_name")
                }
                _ => None,
            };
            let Some(dotted) = dotted else { continue };
            let module = get_node_text(dotted, s.source()).to_string();
            let extra = NodeExtra {
                signature: Some(import_text.clone()),
                ..NodeExtra::default()
            };
            s.create_node(rules, NodeKind::Import, &module, node, extra);
            if let Some(pid) = &parent_id {
                s.add_unresolved(UnresolvedReference {
                    from_node_id: pid.clone(),
                    reference_name: module,
                    reference_kind: EdgeKind::Imports.as_str().to_string(),
                    line: Some(u32::try_from(dotted.start_position().row).unwrap_or(0) + 1),
                    column: Some(u32::try_from(dotted.start_position().column).unwrap_or(0)),
                    file_path: None,
                    language: None,
                });
            }
        }
    }
    // INSERTION POINT (Task 9): Go grouped-import specs.
}

/// `from m import X, Y as Z` → one `imports` ref per imported name (alias
/// wins; wildcard + the module part skipped; last dotted segment).
fn emit_py_from_import_refs(s: &mut Session<'_>, node: Node<'_>, from_id: &str) {
    let module = get_child_by_field(node, "module_name");
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        if let Some(m) = module
            && child.byte_range() == m.byte_range()
        {
            continue;
        }
        if child.kind() == "wildcard_import" {
            continue;
        }
        let name_node = match child.kind() {
            "aliased_import" => get_child_by_field(child, "alias")
                .or_else(|| get_child_by_field(child, "name"))
                .or_else(|| child.named_child(0)),
            "dotted_name" => Some(child),
            _ => None,
        };
        let Some(name_node) = name_node else { continue };
        let raw = get_node_text(name_node, s.source());
        let local = raw.rsplit('.').next().unwrap_or(raw);
        if local.is_empty() {
            continue;
        }
        s.add_unresolved(UnresolvedReference {
            from_node_id: from_id.to_string(),
            reference_name: local.to_string(),
            reference_kind: EdgeKind::Imports.as_str().to_string(),
            line: Some(u32::try_from(name_node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(name_node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }
}

/// Decorators/annotations on a declaration → `decorates` refs: direct
/// children (+ inside a `modifiers` wrapper), then preceding siblings
/// stopping at the first non-decorator. Callee unwrapped from invoked
/// decorators; generic args and qualifier prefixes stripped.
fn extract_decorators_for(s: &mut Session<'_>, decl: Node<'_>, decorated_id: &str) {
    fn consider(s: &mut Session<'_>, n: Node<'_>, decorated_id: &str) {
        // (Solidity modifier_invocation branch — wave 2.)
        if !matches!(
            n.kind(),
            "decorator" | "annotation" | "marker_annotation" | "attribute"
        ) {
            return;
        }
        let mut target: Option<Node<'_>> = None;
        let mut cursor = n.walk();
        let children: Vec<Node<'_>> = n.named_children(&mut cursor).collect();
        for child in children {
            if child.kind() == "call_expression" || child.kind() == "call" {
                target = get_child_by_field(child, "function").or_else(|| child.named_child(0));
                if target.is_some() {
                    break;
                }
            }
            if matches!(
                child.kind(),
                "identifier"
                    | "member_expression"
                    | "attribute" // python decorator callee `app.route` parses as attribute
                    | "scoped_identifier"
                    | "navigation_expression"
                    | "user_type"
                    | "type_identifier"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let mut name = get_node_text(target, s.source()).to_string();
        if let Some(lt) = name.find('<')
            && lt > 0
        {
            name.truncate(lt);
        }
        let last_dot = name.rfind('.').map(|i| i + 1);
        let last_colons = name.rfind("::").map(|i| i + 2);
        if let Some(cut) = last_dot.max(last_colons) {
            name = name[cut..].trim_start_matches([':', '.']).to_string();
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        s.add_unresolved(UnresolvedReference {
            from_node_id: decorated_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::Decorates.as_str().to_string(),
            line: Some(u32::try_from(n.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(n.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }

    // 1. Direct children (+ descend into a `modifiers` wrapper).
    let mut cursor = decl.walk();
    let children: Vec<Node<'_>> = decl.named_children(&mut cursor).collect();
    for child in children {
        consider(s, child, decorated_id);
        if child.kind() == "modifiers" {
            let mut c2 = child.walk();
            let inner: Vec<Node<'_>> = child.named_children(&mut c2).collect();
            for j in inner {
                consider(s, j, decorated_id);
            }
        }
    }

    // 2. Preceding siblings, walking backwards, stopping at the first
    // non-decorator (so an earlier declaration's decorators never leak in).
    let mut sib = decl.prev_named_sibling();
    while let Some(p) = sib {
        if !matches!(p.kind(), "decorator" | "annotation" | "marker_annotation") {
            break;
        }
        consider(s, p, decorated_id);
        sib = p.prev_named_sibling();
    }
}
