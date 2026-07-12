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

mod body;
mod ts_core;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use selene_core::{Edge, EdgeKind, Node as CoreNode, NodeKind, Provenance, file_node_id, node_id};
use tree_sitter::{Node, Parser};

use crate::fnref::{FnRefCandidate, FnRefSpec, fn_ref_spec};
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
    /// The language's rules — a `&'static` copy so hooks receiving
    /// `&mut Session` never conflict with rule dispatch.
    rules: &'static dyn LanguageRules,
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
    /// Value-reference pass state (Task 6, `src/walker/body.rs`):
    /// `SELENE_VALUE_REFS=0` disables; file/class-scope constant targets by
    /// name → id (+ per-name file-scope definition counts for the
    /// conditional-def vs shadow distinction); reader scopes as byte ranges
    /// (re-located against the tree at flush time — `Session` stays free of
    /// the tree lifetime).
    value_refs_enabled: bool,
    file_scope_values: HashMap<String, String>,
    file_scope_value_counts: HashMap<String, usize>,
    value_ref_scopes: Vec<ValueRefScope>,
    /// Function-as-value capture state (#756, Task 15a — `src/fnref.rs`): the
    /// language's spec (`None` = the language captures nothing) plus the
    /// candidates collected during the walk, each paired with the scope they
    /// were captured in. Gated + flushed into `unresolved` at end-of-file
    /// (`body::flush_fn_ref_candidates`).
    fn_ref_spec: Option<&'static FnRefSpec>,
    fn_ref_candidates: Vec<(FnRefCandidate, String)>,
    /// Per-file Vue-store heuristic cache (`src/walker/ts_core.rs`).
    vue_store_file: Option<bool>,
}

/// One value-reference reader scope: the symbol's id/name plus the byte
/// range of its declaration node (re-located at flush time).
pub(crate) struct ValueRefScope {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

impl<'s> Session<'s> {
    fn new(
        file_path: &'s str,
        source: &'s str,
        language: Language,
        rules: &'static dyn LanguageRules,
    ) -> Self {
        let updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let value_refs_enabled = std::env::var("SELENE_VALUE_REFS")
            .map(|v| v != "0")
            .unwrap_or(true);
        Session {
            file_path,
            source,
            language,
            rules,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved: Vec::new(),
            errors: Vec::new(),
            node_stack: Vec::new(),
            id_index: HashMap::new(),
            namespace_prefix: Vec::new(),
            updated_at,
            value_refs_enabled,
            file_scope_values: HashMap::new(),
            file_scope_value_counts: HashMap::new(),
            value_ref_scopes: Vec::new(),
            fn_ref_spec: fn_ref_spec(language),
            fn_ref_candidates: Vec::new(),
            vue_store_file: None,
        }
    }

    /// Re-entrant ladder dispatch — the `ctx.visitNode` of the TS
    /// `ExtractorContext`; `visit_node` hooks call it to hand a subtree
    /// back to the generic walker.
    pub fn visit(&mut self, node: Node<'_>) {
        let rules = self.rules;
        visit(rules, self, node);
    }

    pub(crate) fn vue_store_file_cache(&self) -> Option<bool> {
        self.vue_store_file
    }
    pub(crate) fn set_vue_store_file_cache(&mut self, v: bool) {
        self.vue_store_file = Some(v);
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
        kind: NodeKind,
        name: &str,
        node: Node<'_>,
        extra: NodeExtra,
    ) -> Option<usize> {
        let rules = self.rules;
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

        self.capture_value_ref_scope(kind, name, &id, node);

        self.id_index.insert(id, (kind, name.to_string()));
        self.nodes.push(core);
        Some(self.nodes.len() - 1)
    }

    /// The id of the node at `idx`.
    fn id_of(&self, idx: usize) -> Option<String> {
        self.nodes.get(idx).map(|n| n.id.clone())
    }

    /// The session's language rules (a `&'static` copy — free to hold
    /// across `&mut self` calls).
    pub(crate) fn rules(&self) -> &'static dyn LanguageRules {
        self.rules
    }
    pub(crate) fn language(&self) -> Language {
        self.language
    }
    pub(crate) fn value_refs_enabled(&self) -> bool {
        self.value_refs_enabled
    }
    /// The refs emitted so far — the fn-ref gate reads the `imports` ones to
    /// build its imported-binding name set (`body::flush_fn_ref_candidates`).
    pub(crate) fn unresolved(&self) -> &[UnresolvedReference] {
        &self.unresolved
    }
    /// The language's function-as-value capture spec (`None` = captures
    /// nothing).
    pub(crate) fn fn_ref_spec(&self) -> Option<&'static FnRefSpec> {
        self.fn_ref_spec
    }
    /// Drain the captured fn-ref candidates for the gate.
    pub(crate) fn take_fn_ref_candidates(&mut self) -> Vec<(FnRefCandidate, String)> {
        std::mem::take(&mut self.fn_ref_candidates)
    }

    /// Function-as-value capture (#756, `tree-sitter.ts:549`
    /// `maybeCaptureFnRefs`): if this node is one of the language's
    /// value-position containers (call arguments, assignment RHS,
    /// struct/object initializer, array/table literal), collect candidate
    /// function names from it, attributed to the current scope. Candidates are
    /// gated + flushed at end-of-file.
    ///
    /// Fires from BOTH walkers ([`visit`] and
    /// [`Session::visit_function_body`]) — a node is only ever visited by one
    /// of them — plus [`body::scan_fn_ref_subtree`] for subtrees the walkers
    /// consume without descending.
    pub(crate) fn maybe_capture_fn_refs(&mut self, node: Node<'_>, node_type: &str) {
        let Some(spec) = self.fn_ref_spec else { return };
        let Some(rule) = spec.dispatch_for(node_type) else {
            return;
        };
        let Some(from_node_id) = self.node_stack.last().cloned() else {
            return;
        };
        let captured = crate::fnref::capture_fn_ref_candidates(node, rule, spec, self.source);
        self.fn_ref_candidates
            .extend(captured.into_iter().map(|c| (c, from_node_id.clone())));
    }
    /// Drain the value-reference state for the flush pass (scopes, targets,
    /// per-name file-scope definition counts).
    #[allow(clippy::type_complexity)] // the three-part flush handoff
    pub(crate) fn take_value_ref_state(
        &mut self,
    ) -> (
        Vec<ValueRefScope>,
        HashMap<String, String>,
        HashMap<String, usize>,
    ) {
        (
            std::mem::take(&mut self.value_ref_scopes),
            std::mem::take(&mut self.file_scope_values),
            std::mem::take(&mut self.file_scope_value_counts),
        )
    }

    /// Value-reference capture (runs on every created node): distinctive
    /// file/class-scope const/var names become TARGETS (`name.len() >= 3`
    /// and contains `[A-Z_]`; scope decided by the parent id's kind PREFIX —
    /// the load-bearing id-prefix contract); function/method/const/var
    /// declarations become reader SCOPES (byte ranges, re-located at flush).
    fn capture_value_ref_scope(&mut self, kind: NodeKind, name: &str, id: &str, node: Node<'_>) {
        if !self.value_refs_enabled {
            return;
        }
        let target_kind_ok = kind == NodeKind::Constant || kind == NodeKind::Variable;
        if target_kind_ok
            && name.len() >= 3
            && name.chars().any(|c| c.is_ascii_uppercase() || c == '_')
            && let Some(parent_id) = self.scope_id()
            && (parent_id.starts_with("file:")
                || parent_id.starts_with("class:")
                || parent_id.starts_with("module:")
                || parent_id.starts_with("struct:")
                || parent_id.starts_with("enum:"))
        {
            self.file_scope_values
                .insert(name.to_string(), id.to_string());
            *self
                .file_scope_value_counts
                .entry(name.to_string())
                .or_insert(0) += 1;
        }
        if matches!(
            kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constant | NodeKind::Variable
        ) {
            self.value_ref_scopes.push(ValueRefScope {
                id: id.to_string(),
                name: name.to_string(),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
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

    let mut s = Session::new(file_path, source, language, rules);

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

    // Gate + flush the function-as-value candidates (#756) while the file's
    // nodes and import refs are complete and the file node is still pushed.
    body::flush_fn_ref_candidates(&mut s);
    body::flush_value_refs(&mut s, &tree);

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
    s.create_node(NodeKind::Namespace, &name, pkg, NodeExtra::default())
}

/// `extractName`: resolve_name hook, else the `name_field` child (C/C++
/// declarator unwrapping arrives with Task 13), else `<anonymous>`; passed
/// through `recover_mangled_name` (identity by default).
fn extract_name(rules: &'static dyn LanguageRules, node: Node<'_>, source: &str) -> String {
    let raw = rules
        .resolve_name(node, source)
        .or_else(|| {
            let name_node = get_child_by_field(node, rules.tables().name_field)?;
            Some(resolve_declarator_name(name_node, source))
        })
        .or_else(|| {
            // Arrow/function expressions never name themselves from body
            // identifiers — the parent declarator names them (or nothing).
            if node.kind() == "arrow_function" || node.kind() == "function_expression" {
                return None;
            }
            // Fall back to the first identifier-like child (how a C
            // `struct point` gets its name despite name_field being
            // `declarator` — struct_specifier has no such field).
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| {
                    matches!(
                        c.kind(),
                        "identifier" | "type_identifier" | "simple_identifier" | "constant"
                    )
                })
                .map(|c| get_node_text(c, source).to_string())
        })
        .unwrap_or_else(|| "<anonymous>".to_string());
    rules.recover_mangled_name(raw)
}

/// The C/C++ declarator-unwrapping tail of `extractNameRaw` (Task 13):
/// pointer/reference declarators unwrap to their inner (`int* f()` names
/// `f`, not `* f(...)`); a user-defined conversion operator names
/// `operator <type>`; a `function_declarator`/`declarator` yields its inner
/// declarator (the identifier). Inert for grammars whose name field is
/// already an identifier. (Lua dot/method index shapes — wave 2.)
fn resolve_declarator_name(name_node: Node<'_>, source: &str) -> String {
    let mut resolved = name_node;
    while resolved.kind() == "pointer_declarator" || resolved.kind() == "reference_declarator" {
        let inner = get_child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0));
        match inner {
            Some(i) => resolved = i,
            None => break,
        }
    }
    if resolved.kind() == "operator_cast" {
        return match resolved.named_child(0) {
            Some(type_node) => format!("operator {}", get_node_text(type_node, source).trim()),
            None => get_node_text(resolved, source).to_string(),
        };
    }
    if resolved.kind() == "function_declarator" || resolved.kind() == "declarator" {
        let inner = get_child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0));
        return match inner {
            Some(i) => get_node_text(i, source).to_string(),
            None => get_node_text(resolved, source).to_string(),
        };
    }
    get_node_text(resolved, source).to_string()
}

/// Class/module-scope `CONST = …`: an `assignment` whose LHS is a
/// `constant` node (the TS `isClassScopeConstantAssignment` — see the
/// variable branch of the dispatch ladder).
fn is_class_scope_constant_assignment(node: Node<'_>) -> bool {
    if node.kind() != "assignment" {
        return false;
    }
    get_child_by_field(node, "left")
        .or_else(|| node.named_child(0))
        .is_some_and(|left| left.kind() == "constant")
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
        // The hook consumed this subtree, so the walkers below never descend
        // into it — scan it for function-as-value candidates (#756). The scan
        // is capture-only and halts at nested function boundaries.
        body::scan_fn_ref_subtree(s, node, 0);
        return;
    }

    // C++ namespace blocks (Task 13): carry the namespace name as a
    // qualifiedName prefix while walking the body — NO node is minted, so
    // `namespace flash { void compute(); }` indexes `flash::compute` and a
    // namespace-qualified call resolves by exact qualified match (#387).
    // C++17 nested forms (`namespace a::b {`) prefix as written; an
    // ANONYMOUS namespace falls through to the generic walk — its contents
    // stay bare, matching how call sites spell them.
    if s.language() == Language::Cpp && node_type == "namespace_definition" {
        let ns_name = get_child_by_field(node, "name")
            .map(|n| get_node_text(n, s.source()).to_string())
            .unwrap_or_default();
        if !ns_name.is_empty() {
            s.namespace_prefix.push(ns_name);
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            for child in children {
                visit(rules, s, child);
            }
            s.namespace_prefix.pop();
            return;
        }
    }

    // Function-as-value capture (#756, Task 15a) — deliberately INDEPENDENT of
    // the dispatch ladder below (the captured container types have no other
    // handler there), so it can never shadow or be shadowed by an extraction
    // branch. Subtrees a matched branch consumes without descending get
    // `scan_fn_ref_subtree` instead (below).
    s.maybe_capture_fn_refs(node, node_type);

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
            // A field initializer can register callbacks
            // (`static handlers = { click: onClick }`) — the property branch
            // consumes the subtree, so scan it for fn-ref candidates.
            body::scan_fn_ref_subtree(s, node, 0);
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
        // TS semantics: a plain alias does NOT skip children (the walker
        // recurses into the alias value); a reclassified one does.
        matched = extract_type_alias(rules, s, node);
    } else if t.property_types.contains(&node_type) && s.is_inside_class_like() {
        extract_property(rules, s, node);
        // Property initializers aren't walked — scan for fn-ref candidates
        // (Kotlin `val cb = ::handler` class properties — Task 15b).
        body::scan_fn_ref_subtree(s, node, 0);
    } else if t.field_types.contains(&node_type) && s.is_inside_class_like() {
        extract_field(rules, s, node);
        // Field initializers aren't walked — scan for fn-ref candidates (Java
        // `List<IntConsumer> table = List.of(Main::cb)`, C#
        // `List<Action<int>> table = new() { TargetCb }` — Task 15b).
        body::scan_fn_ref_subtree(s, node, 0);
    } else if t.variable_types.contains(&node_type)
        && (!s.is_inside_class_like() || is_class_scope_constant_assignment(node))
    {
        // Top-level variables — plus class/module-scope CONSTANTS (Task 14):
        // a Ruby `CONST = …` has a `constant`-typed LHS; no other grammar
        // puts one here, so the gate is effectively Ruby-only and never
        // disturbs other languages' class-internal locals.
        extract_variable(rules, s, node);
        // `extract_variable` doesn't walk every initializer shape (object
        // literals are deliberately skipped; Python/C don't walk at all), so
        // scan the declaration subtree for fn-ref candidates — `const routes =
        // { home: renderHome }`, `handlers = {"recv": target_cb}`, `static
        // cb_t table[] = { cb_a, cb_b }`. The scan halts at nested function
        // definitions (their bodies are walked — and attributed — separately)
        // and flush-time dedup absorbs any overlap with the initializers
        // `extract_variable` DOES walk.
        body::scan_fn_ref_subtree(s, node, 0);
    } else if t.import_types.contains(&node_type) {
        extract_import(rules, s, node);
    }
    // TS/JS re-export refs: `export { A, B as C } from './y'` — barrels
    // record a dependency on their source module (Task 8).
    else if node_type == "export_statement"
        && is_ts_js_language(s.language())
        && get_child_by_field(node, "source").is_some()
    {
        if let Some(parent_id) = s.scope_id().cloned() {
            s.emit_re_export_refs(node, &parent_id);
        }
        matched = false; // children still recurse (a re-export can't nest, but parity)
    }
    // Vuex MODULE default export: `export default { namespaced, actions:
    // {…} }` — store-file gated; the collection methods become nodes and
    // the subtree is consumed (Task 8).
    else if node_type == "export_statement"
        && is_ts_js_language(s.language())
        && s.looks_like_vue_store_file()
        && let Some(exported) = get_child_by_field(node, "value")
        && (exported.kind() == "object" || exported.kind() == "object_expression")
    {
        s.extract_store_collection_methods(rules, exported);
    } else if t.call_types.contains(&node_type) {
        // Top-level calls (IIFE module wrappers #528, side-effect calls)
        // attribute to the stack top; children STILL recurse so nested
        // arrows/calls extract (TS: skipChildren stays false here).
        s.extract_call(node);
        matched = false;
    } else if body::INSTANTIATION_KINDS.contains(&node_type) {
        // Children still walked so ctor-arg calls get their own refs.
        s.extract_instantiation(node);
        // Java/C# `new T(...) { … }` — anonymous class with body (Task 10):
        // consumed whole (TS skipChildren = true); plain instantiations
        // keep recursing.
        if let Some(anon_body) = body::find_anonymous_class_body(node) {
            s.extract_anonymous_class(node, anon_body);
        } else {
            matched = false;
        }
    }
    // INSERTION POINT (Task 9): Rust `impl_item` implements refs.
    // TS interface members: property_signature / method_signature carry
    // type annotations the interface walker would otherwise drop (Task 8).
    else if (node_type == "property_signature" || node_type == "method_signature")
        && s.is_inside_class_like()
        && ts_core::is_type_annotation_language(s.language())
    {
        if let Some(parent_id) = s.scope_id().cloned() {
            s.extract_type_annotations(rules, node, &parent_id);
        }
        matched = false; // nested signatures still need traversal
    } else {
        matched = false;
    }

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
    extract_function_named(rules, s, node, None);
}

/// [`extract_function`] with an optional explicit name — supplied only for
/// explicitly-named anonymous functions the caller resolved itself (object-
/// literal function members, RTK endpoints — `src/walker/ts_core.rs`).
fn extract_function_named(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
    name_override: Option<&str>,
) {
    // Receiver-typed functions (Rust impl fns, Task 9) route to method.
    if rules.get_receiver_type(node, s.source()).is_some() {
        extract_method(rules, s, node);
        return;
    }

    let mut name = name_override
        .map(str::to_string)
        .unwrap_or_else(|| extract_name(rules, node, s.source()));
    // TS/JS arrow-const naming: `const useAuth = () => {…}` — the arrow
    // node has no `name` field; the name lives on the parent
    // `variable_declarator` (Task 7).
    if name_override.is_none()
        && name == "<anonymous>"
        && (node.kind() == "arrow_function" || node.kind() == "function_expression")
        && let Some(parent) = node.parent()
        && parent.kind() == "variable_declarator"
        && let Some(var_name) = get_child_by_field(parent, "name")
    {
        name = get_node_text(var_name, s.source()).to_string();
    }
    if name == "<anonymous>" || rules.is_misparsed_function(&name, node) {
        // No node, but the body is still walked — AMD/CommonJS module
        // wrappers hold named inner functions and calls (#528).
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
    let Some(idx) = s.create_node(NodeKind::Function, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // Type refs from parameter/return annotations (Task 8).
    s.extract_type_annotations(rules, node, &id);
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
    let Some(idx) = s.create_node(NodeKind::Method, &name, node, extra) else {
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

    // Type refs from parameter/return annotations (Task 8).
    s.extract_type_annotations(rules, node, &id);
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
    let Some(idx) = s.create_node(kind, &name, node, extra) else {
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
    let Some(idx) = s.create_node(kind, &name, node, extra) else {
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
    let Some(idx) = s.create_node(NodeKind::Struct, &name, node, extra) else {
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
    let Some(idx) = s.create_node(NodeKind::Enum, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    s.push_scope(id);
    let member_types = rules.tables().enum_member_types;
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        if member_types.contains(&child.kind()) {
            extract_enum_members(s, child);
        } else {
            visit(rules, s, child);
        }
    }
    s.pop_scope();
}

/// Enum member names: `name` field first (Rust enum_variant), else
/// identifier-like children (multi-case declarations), else the node itself
/// when it is a bare identifier.
fn extract_enum_members(s: &mut Session<'_>, node: Node<'_>) {
    if let Some(name_node) = get_child_by_field(node, "name") {
        let name = get_node_text(name_node, s.source()).to_string();
        s.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
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
            s.create_node(NodeKind::EnumMember, &name, child, NodeExtra::default());
            found = true;
        }
    }
    if !found && node.named_child_count() == 0 {
        let name = get_node_text(node, s.source()).to_string();
        s.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
    }
}

/// Type alias (`type X = ...`). Returns the TS `skipChildren` bool: `true`
/// when the alias was reclassified and its body walked; `false` for a plain
/// alias (the walker then recurses into the alias value, matching TS).
/// `resolve_type_alias_kind` reclassification bodies (Go `type_spec`
/// struct/interface, C typedefs) land with Tasks 9/13 — INSERTION POINT.
fn extract_type_alias(
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
    // INSERTION POINT (Task 9/13): struct/enum/interface reclassification
    // with body walks (returns true).
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
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
    let created = s.create_node(NodeKind::Property, &name, node, extra);
    // `@Inject() private svc: Foo` — decorator + type-annotation refs on
    // class properties too (Task 8).
    if let Some(id) = created.and_then(|idx| s.id_of(idx)) {
        extract_decorators_for(s, node, &id);
        s.extract_type_annotations(rules, node, &id);
    }
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
        s.create_node(NodeKind::Field, &name, d, extra);
    }
}

/// Vue store collection key names (`ts_core.rs` owns the sets; this thin
/// check keeps the variable branch readable).
fn ts_core_is_store_collection_name(name: &str) -> bool {
    matches!(name, "actions" | "mutations" | "getters")
}

/// The TS/JS language family gate for the ts_core ladder branches.
fn is_ts_js_language(l: Language) -> bool {
    matches!(
        l,
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx
    )
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
fn extract_variable(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
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

/// Imports: hook first (single-module languages); Python inline
/// multi-import + from-import per-name refs are core machinery (map §11).
fn extract_import(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let import_text = get_node_text(node, s.source()).trim().to_string();

    if let Some(info) = rules.extract_import(node, s.source()) {
        let extra = NodeExtra {
            signature: Some(info.signature.clone()),
            ..NodeExtra::default()
        };
        s.create_node(NodeKind::Import, &info.module_name, node, extra);
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
        // TS/JS import-binding refs: each imported LOCAL binding records a
        // dependency (Task 8).
        if is_ts_js_language(s.language())
            && let Some(parent_id) = s.scope_id().cloned()
        {
            s.emit_import_binding_refs(node, &parent_id);
        }
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
            s.create_node(NodeKind::Import, &module, node, extra);
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
