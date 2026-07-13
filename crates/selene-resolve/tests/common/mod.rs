#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]
//! `FakeContext` — the in-memory [`ResolutionContext`] every strategy test in
//! this crate is written against.
//!
//! **A matcher test must never need a database.** The strategies (Tasks 3–10)
//! take `&C: ResolutionContext`, so they can be exercised over a handful of
//! nodes and a `HashMap` of file contents — which is what makes it practical to
//! port the TS contract suite's hundreds of assertions. `StoreContext` (the real
//! one) is exercised separately, in `tests/store_context_test.rs`, against a
//! real `SurrealStore`.
//!
//! Later tasks extend this with a builder method per input they need
//! (`with_alias_map`, `with_go_module`, …); the shape is deliberately
//! append-only.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use selene_core::{Language, Node, NodeKind};
use selene_resolve::{
    AliasMap, GoModule, ImportMapping, ReExport, ResolutionContext, WorkspacePackages,
};

/// An in-memory [`ResolutionContext`] built from nodes, files, and edges.
#[derive(Default)]
pub struct FakeContext {
    root: PathBuf,
    nodes: Vec<Node>,
    files: HashMap<String, String>,
    /// `(source_id, target_id)` pairs of `implements`/`extends` edges.
    ///
    /// Behind a `Mutex` so a test can ADD one after the context is already inside a
    /// resolver — which is exactly what the first resolution pass does in reality
    /// (it persists the `implements`/`extends` edges the conformance pass then
    /// needs). Without that, a conformance test would have to build a second
    /// resolver over a pre-populated graph, and would then be testing nothing: the
    /// first pass would simply resolve the reference outright and never defer it.
    supertype_edges: Mutex<Vec<(String, String)>>,
    /// `(container_id, member_id)` pairs of `contains` edges.
    contains_edges: Vec<(String, String)>,
    /// file path → the import mappings that file declares (Task 3's pre-filter
    /// escape, Task 6's `resolve_via_import`).
    import_mappings: HashMap<String, Vec<ImportMapping>>,
    /// file path → the re-exports that barrel declares (Task 6).
    re_exports: HashMap<String, Vec<ReExport>>,
    aliases: Option<AliasMap>,
    go_module: Option<GoModule>,
    workspace: Option<WorkspacePackages>,
    cpp_include_dirs: Vec<String>,

    // Derived at build time.
    all_files: Vec<String>,
    files_with_language: Vec<(String, Language)>,
    languages: BTreeSet<Language>,
    known_files: HashSet<String>,
    known_names: HashSet<String>,

    /// Counts every graph read, so a test can prove a pre-filter short-circuited
    /// BEFORE any strategy ran (Task 3 asserts exactly that).
    pub reads: Arc<Mutex<usize>>,
}

impl FakeContext {
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/fake"),
            ..Default::default()
        }
    }

    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    pub fn with_node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self.rebuild();
        self
    }

    pub fn with_file(mut self, path: &str, content: &str) -> Self {
        self.files.insert(path.to_string(), content.to_string());
        self.rebuild();
        self
    }

    /// An import binding declared by `path`.
    pub fn with_import_mapping(mut self, path: &str, m: ImportMapping) -> Self {
        self.import_mappings
            .entry(path.to_string())
            .or_default()
            .push(m);
        self
    }

    /// A re-export declared by the barrel at `path`.
    pub fn with_re_export(mut self, path: &str, e: ReExport) -> Self {
        self.re_exports.entry(path.to_string()).or_default().push(e);
        self
    }

    /// A loaded tsconfig alias map.
    pub fn with_aliases(mut self, m: AliasMap) -> Self {
        self.aliases = Some(m);
        self
    }

    /// A loaded `go.mod`.
    pub fn with_go_module(mut self, m: GoModule) -> Self {
        self.go_module = Some(m);
        self
    }

    /// Loaded workspace packages.
    pub fn with_workspace(mut self, w: WorkspacePackages) -> Self {
        self.workspace = Some(w);
        self
    }

    /// C/C++ `-I` include directories (Task 5).
    pub fn with_cpp_include_dirs(mut self, dirs: Vec<String>) -> Self {
        self.cpp_include_dirs = dirs;
        self
    }

    /// An `implements`/`extends` edge, by node id.
    pub fn with_supertype(self, child_id: &str, parent_id: &str) -> Self {
        self.add_supertype_edge(child_id, parent_id);
        self
    }

    /// Add an `implements`/`extends` edge to a context that is ALREADY inside a
    /// resolver — modelling the first pass persisting the type graph that the
    /// conformance pass (#750) then walks.
    pub fn add_supertype_edge(&self, child_id: &str, parent_id: &str) {
        if let Ok(mut edges) = self.supertype_edges.lock() {
            edges.push((child_id.to_string(), parent_id.to_string()));
        }
    }

    /// A `contains` edge, by node id.
    pub fn with_member(mut self, container_id: &str, member_id: &str) -> Self {
        self.contains_edges
            .push((container_id.to_string(), member_id.to_string()));
        self
    }

    /// Recompute the warm caches (mirrors what `StoreContext::new` does once).
    fn rebuild(&mut self) {
        let mut paths: HashSet<String> = self.nodes.iter().map(|n| n.file_path.clone()).collect();
        paths.extend(self.files.keys().cloned());
        self.all_files = paths.iter().cloned().collect();
        self.all_files.sort();
        self.known_files = paths;

        let mut by_path: HashMap<String, Language> = HashMap::new();
        for n in &self.nodes {
            if let Some(l) = Language::from_wire(&n.language) {
                by_path.insert(n.file_path.clone(), l);
            }
        }
        self.files_with_language = by_path.into_iter().collect();
        self.files_with_language.sort_by(|a, b| a.0.cmp(&b.0));
        self.languages = self.files_with_language.iter().map(|(_, l)| *l).collect();
        self.known_names = self.nodes.iter().map(|n| n.name.clone()).collect();
    }

    fn tick(&self) {
        if let Ok(mut n) = self.reads.lock() {
            *n += 1;
        }
    }

    /// The RAW node count for a name — what the store's `count_nodes_named`
    /// answers. Exposed for the test that proves the #999 ceiling is compared
    /// against the FILTERED candidate count, not this.
    pub fn count_nodes_named_for_test(&self, name: &str) -> u64 {
        self.nodes.iter().filter(|n| n.name == name).count() as u64
    }

    /// How many graph reads this context has served.
    pub fn read_count(&self) -> usize {
        self.reads.lock().map(|n| *n).unwrap_or(0)
    }

    fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

impl ResolutionContext for FakeContext {
    fn nodes_in_file(&self, path: &str) -> Vec<Node> {
        self.tick();
        self.nodes
            .iter()
            .filter(|n| n.file_path == path)
            .cloned()
            .collect()
    }

    fn nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.tick();
        self.nodes
            .iter()
            .filter(|n| n.name == name)
            .cloned()
            .collect()
    }

    fn nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
        self.tick();
        self.nodes
            .iter()
            .filter(|n| n.name.to_lowercase() == lower)
            .cloned()
            .collect()
    }

    fn nodes_by_qualified_name(&self, qn: &str) -> Vec<Node> {
        self.tick();
        self.nodes
            .iter()
            .filter(|n| n.qualified_name == qn)
            .cloned()
            .collect()
    }

    fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.tick();
        self.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .cloned()
            .collect()
    }

    fn node_by_id(&self, id: &str) -> Option<Node> {
        self.tick();
        self.node(id).cloned()
    }

    fn count_files_with_name(&self, name: &str) -> u64 {
        self.tick();
        // The store's semantics, faithfully: DISTINCT FILES, not nodes (spike F2).
        let files: HashSet<&str> = self
            .nodes
            .iter()
            .filter(|n| n.name == name)
            .map(|n| n.file_path.as_str())
            .collect();
        files.len() as u64
    }

    fn count_nodes_named(&self, name: &str) -> u64 {
        self.tick();
        // NODES, not files — the distinction the spike's F2 finding turns on.
        self.nodes.iter().filter(|n| n.name == name).count() as u64
    }

    fn method_matches(&self, language: Language, ty: &str, method: &str) -> Vec<Node> {
        self.tick();
        let exact = format!("{ty}::{method}");
        let suffix = format!("::{ty}::{method}");
        self.nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::Method
                    && n.name == method
                    && Language::from_wire(&n.language) == Some(language)
                    && (n.qualified_name == exact || n.qualified_name.ends_with(&suffix))
            })
            .cloned()
            .collect()
    }

    fn supertypes(&self, node_id: &str) -> Vec<Node> {
        self.tick();
        let Ok(edges) = self.supertype_edges.lock() else {
            return Vec::new();
        };
        edges
            .iter()
            .filter(|(child, _)| child == node_id)
            .filter_map(|(_, parent)| self.node(parent).cloned())
            .collect()
    }

    fn members_of(&self, node_id: &str) -> Vec<Node> {
        self.tick();
        self.contains_edges
            .iter()
            .filter(|(container, _)| container == node_id)
            .filter_map(|(_, member)| self.node(member).cloned())
            .collect()
    }

    fn project_root(&self) -> &Path {
        &self.root
    }

    fn file_exists(&self, path: &str) -> bool {
        self.known_files.contains(path)
    }

    fn read_file(&self, path: &str) -> Option<String> {
        self.files.get(path).cloned()
    }

    fn file_lines(&self, path: &str) -> Option<Arc<Vec<String>>> {
        self.read_file(path)
            .map(|src| Arc::new(src.lines().map(str::to_string).collect()))
    }

    fn all_files(&self) -> &[String] {
        &self.all_files
    }

    fn files_with_language(&self) -> &[(String, Language)] {
        &self.files_with_language
    }

    fn languages(&self) -> &BTreeSet<Language> {
        &self.languages
    }

    fn list_directories(&self, _path: &str) -> Vec<String> {
        Vec::new()
    }

    fn import_mappings(&self, path: &str) -> Arc<Vec<ImportMapping>> {
        Arc::new(self.import_mappings.get(path).cloned().unwrap_or_default())
    }

    fn re_exports(&self, path: &str) -> Arc<Vec<ReExport>> {
        Arc::new(self.re_exports.get(path).cloned().unwrap_or_default())
    }

    fn project_aliases(&self) -> Option<&AliasMap> {
        self.aliases.as_ref()
    }

    fn go_module(&self) -> Option<&GoModule> {
        self.go_module.as_ref()
    }

    fn workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace.as_ref()
    }

    fn cpp_include_dirs(&self) -> &[String] {
        &self.cpp_include_dirs
    }

    fn known_files(&self) -> &HashSet<String> {
        &self.known_files
    }

    fn known_names(&self) -> &HashSet<String> {
        &self.known_names
    }
}

// =============================================================================
// Node builders (shared by every strategy test)
// =============================================================================

/// A node with the given kind/name/qualified-name/file/language.
pub fn node(id: &str, kind: NodeKind, name: &str, qn: &str, file: &str, lang: Language) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qn.to_string(),
        file_path: file.to_string(),
        language: lang.as_str().to_string(),
        start_line: 1,
        end_line: 10,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: vec![],
        type_parameters: vec![],
        return_type: None,
        // Route fields: only the framework registry sets these (Task 11).
        route_method: None,
        route_path: None,
        framework: None,
        updated_at: 0,
    }
}

/// A TypeScript function node.
pub fn ts_fn(id: &str, name: &str, file: &str) -> Node {
    node(
        id,
        NodeKind::Function,
        name,
        name,
        file,
        Language::Typescript,
    )
}

/// A TypeScript method node (`qualified_name` = `Type::name`).
pub fn ts_method(id: &str, ty: &str, name: &str, file: &str) -> Node {
    node(
        id,
        NodeKind::Method,
        name,
        &format!("{ty}::{name}"),
        file,
        Language::Typescript,
    )
}
