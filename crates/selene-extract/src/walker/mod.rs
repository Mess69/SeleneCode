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
mod decls;
mod imports;
mod ladder;
mod ts_core;
mod types;
mod vars;

pub(crate) use self::imports::push_php_use_ref;
pub use self::ladder::extract_from_source;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use selene_core::{Edge, EdgeKind, Node as CoreNode, NodeKind, Provenance, node_id};
use tree_sitter::Node;

use crate::fnref::{FnRefCandidate, FnRefSpec, fn_ref_spec};
use crate::rules::LanguageRules;
use crate::{ExtractionError, Language, UnresolvedReference};

use self::decls::{
    extract_class, extract_decorators_for, extract_enum, extract_enum_members, extract_function,
    extract_function_named, extract_interface, extract_method, extract_struct,
};
use self::imports::extract_import;
use self::ladder::{extract_name, is_ts_js_language, resolve_body, visit};
use self::types::{extract_inheritance, extract_type_alias};
use self::vars::{extract_field, extract_property, extract_variable};

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
            language: self.language,
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
            // Route fields are never set by a language extractor: only the
            // framework registry (`selene-resolve`) emits `NodeKind::Route`.
            route_method: None,
            route_path: None,
            framework: None,
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

    /// The id of the node at `idx` (the index [`Session::create_node`] returns).
    /// `pub(crate)` so a language rules hook that creates its own nodes can
    /// attach refs to them — C# fields do (`rules/csharp.rs`).
    pub(crate) fn id_of(&self, idx: usize) -> Option<String> {
        self.nodes.get(idx).map(|n| n.id.clone())
    }

    /// `extends`/`implements` refs from `node`'s inheritance clauses.
    ///
    /// Exposed for the language hooks that own their type extraction outright and
    /// so never reach the walker's class/struct path — Go's interface `type_spec`
    /// (`rules/go.rs`) is the one such case.
    pub(crate) fn extract_inheritance(&mut self, node: Node<'_>, owner_id: &str) {
        extract_inheritance(self, node, owner_id);
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
