//! [`ResolutionContext`] — the read-only project view every strategy sees —
//! and [`StoreContext`], its `GraphStore`-backed implementation.
//!
//! This is the port of CodeGraph's `ResolutionContext` (an interface of ~20
//! data-access closures, `maps/resolution.md` §types.ts). It is **the only
//! module in this crate that touches `selene-db` or `std::fs`**: every strategy
//! function takes `&C: ResolutionContext`, so the whole matcher is unit-testable
//! against an in-memory fake with no database at all (see
//! `tests/common/mod.rs`).
//!
//! # The sync/async seam (decided once, here)
//!
//! `ResolutionContext`'s methods are **synchronous**. The matcher is a fixed,
//! ordered, single-threaded pipeline whose behavior *is* its order; making it
//! async would infect every strategy function with `.await`, and buy nothing —
//! resolution is CPU-bound over a warm cache, not I/O-bound.
//!
//! [`StoreContext`] therefore bridges to the async `GraphStore` by holding a
//! `tokio::runtime::Handle` and calling `Handle::block_on` for the reads the
//! warm cache misses. **That is only legal off a runtime worker thread**, so the
//! resolver must run inside `spawn_blocking` — exactly as Phase 2's extraction
//! orchestrator already does at its own DB seam. Part C's driver (Task 27) owns
//! that wrapper. Calling a `StoreContext` method directly from an async task
//! panics inside tokio, loudly and immediately; [`StoreContext::new`] documents
//! it, and the alternative (a silently-deadlocking hand-rolled executor) is
//! strictly worse.
//!
//! # Warm caches
//!
//! `known_files` / `known_names` / `files_with_language` / `languages` are
//! computed **once**, at construction, in one query each. `known_names` backs
//! the `has_any_possible_match` pre-filter that runs on *every* reference, so it
//! must be a hash lookup, not a query. The spike (F4) measured the cost: this
//! repo's 2 482 nodes yield 1 767 distinct names for ~0.08 MB, and a repo 100x
//! larger still warms a ~10 MB name set — so the TS streaming `iterateNodeNames`
//! is **dropped**, not ported.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use selene_core::{EdgeKind, Language, Node, NodeKind};
use selene_db::GraphStore;

use crate::cache::{SyncLru, cache_limit, content_cache_limit};
use crate::error::{ResolveError, Result};
use crate::types::{AliasMap, GoModule, ImportMapping, ReExport, WorkspacePackages};

// =============================================================================
// The trait
// =============================================================================

/// The read-only project view a strategy, a framework resolver, or a
/// synthesizer sees.
///
/// `Send + Sync`: Part B's framework registry is a `&'static` table and the
/// synth passes hold `&dyn ResolutionContext` across an await.
pub trait ResolutionContext: Send + Sync {
    // ---- graph reads --------------------------------------------------------

    /// Every node attributed to `path`.
    fn nodes_in_file(&self, path: &str) -> Vec<Node>;

    /// Every node whose `name` matches exactly (case-sensitive).
    fn nodes_by_name(&self, name: &str) -> Vec<Node>;

    /// Every node whose lower-cased `name` equals `lower` (fuzzy matching,
    /// Task 7). `lower` is expected **pre-lowercased** by the caller.
    fn nodes_by_lower_name(&self, lower: &str) -> Vec<Node>;

    /// Every node whose `qualified_name` matches exactly (overloads can share
    /// one, so this is a `Vec`, not an `Option`).
    fn nodes_by_qualified_name(&self, qn: &str) -> Vec<Node>;

    /// Every node of exactly `kind`.
    fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Node>;

    /// Point lookup by id.
    fn node_by_id(&self, id: &str) -> Option<Node>;

    /// How many **files** contain a node named exactly `name`.
    ///
    /// ⚠ **This is a FILE count, not a node count** — it is the store's
    /// `count_nodes_matching_name_in_files`, whose SurrealQL is
    /// `SELECT filePath … GROUP BY filePath` (`selene-db/src/nodes.rs`). The
    /// spike (F2) measured it: 2 001 nodes named `get` over 201 files answers
    /// **201**. So it CANNOT back the `AMBIGUOUS_NAME_CEILING` (#999) guard,
    /// which compares 500 against a **candidate-node** count — wiring the
    /// ceiling to this would mean the guard never fires. Task 7 is blocked on
    /// the maintainer decision (Open Coordination Point 1b); this method keeps
    /// the store's real semantics and an honest name until then.
    fn count_files_with_name(&self, name: &str) -> u64;

    /// How many **nodes** are named exactly `name`, across every file.
    ///
    /// This is what the `AMBIGUOUS_NAME_CEILING` (#999) guard compares against
    /// (Task 7): above the ceiling a name is *ubiquitous*, and the matcher
    /// **declines rather than guesses**. The whole point of a counting primitive
    /// here is to decline **without materializing** the ten thousand candidates —
    /// so this must never be implemented as `nodes_by_name(name).len()`.
    ///
    /// Distinct from [`Self::count_files_with_name`] on purpose: the spike (F2)
    /// found the store's original primitive counted distinct FILES, which would
    /// have compared 500 against a file count and left the guard silently never
    /// firing. `selene-db` now carries both, with honest names.
    fn count_nodes_named(&self, name: &str) -> u64;

    /// Method nodes of `language` on type `ty` named `method`: the nodes whose
    /// `qualified_name` is `"{ty}::{method}"` or ends with `"::{ty}::{method}"`.
    /// Memoized (key `"{language} {ty}::{method}"`, `maps/resolution.md`
    /// §Caches) — the store has no suffix query, so this is a
    /// fetch-by-name-then-filter (spike F2).
    fn method_matches(&self, language: Language, ty: &str, method: &str) -> Vec<Node>;

    /// The supertypes of `node_id`: the targets of its outgoing
    /// `implements`/`extends` edges.
    ///
    /// **Node-anchored, never name-keyed.** A name-keyed `get_supertypes("Engine")`
    /// unioned every rails `Engine`'s parents and produced a cross-class wrong
    /// edge (`design/function-ref-capture.md` §Known limits); the node walk
    /// eliminated it.
    fn supertypes(&self, node_id: &str) -> Vec<Node>;

    /// The `contains` children of `node_id` — a class's members.
    fn members_of(&self, node_id: &str) -> Vec<Node>;

    // ---- filesystem reads ---------------------------------------------------

    /// The project root (absolute).
    fn project_root(&self) -> &Path;

    /// Does this repo-relative path exist in the index?
    fn file_exists(&self, path: &str) -> bool;

    /// The text of a repo-relative file, cached. `None` when unreadable — a
    /// miss, never an error.
    fn read_file(&self, path: &str) -> Option<String>;

    /// A file's lines, cached and shared (`Arc`) — the receiver-inference
    /// scans walk them backwards, per reference (`#1122`).
    fn file_lines(&self, path: &str) -> Option<Arc<Vec<String>>>;

    /// Every indexed file path, sorted.
    fn all_files(&self) -> &[String];

    /// Every indexed file path **with its language**, sorted by path.
    ///
    /// Part B's framework `extract()` sweep and every language-gated pass need
    /// the language; `all_files()` alone would force a re-detect per file.
    fn files_with_language(&self) -> &[(String, Language)];

    /// The distinct languages present in the index.
    ///
    /// A `BTreeSet` (not a `HashSet`) because it gates framework applicability
    /// and synth-pass execution, and iteration order there leaks into output
    /// order.
    fn languages(&self) -> &BTreeSet<Language>;

    /// Immediate sub-directories of a repo-relative directory (the cargo
    /// workspace glob walk, Task 18).
    fn list_directories(&self, path: &str) -> Vec<String>;

    // ---- import-side singletons (loaders land in Tasks 4/5) -----------------

    /// The import bindings a file introduces (Task 4 wires the extractor).
    fn import_mappings(&self, path: &str) -> Arc<Vec<ImportMapping>>;

    /// The re-exports a barrel file declares (Task 4 wires the extractor).
    fn re_exports(&self, path: &str) -> Arc<Vec<ReExport>>;

    /// `tsconfig`/`jsconfig` path aliases, loaded once (Task 4).
    fn project_aliases(&self) -> Option<&AliasMap>;

    /// The `go.mod` module directive, loaded once (Task 4).
    fn go_module(&self) -> Option<&GoModule>;

    /// npm/yarn/bun/pnpm workspace members, loaded once (Task 4).
    fn workspace_packages(&self) -> Option<&WorkspacePackages>;

    /// C/C++ `-I` include directories, loaded once (Task 5).
    fn cpp_include_dirs(&self) -> &[String];

    // ---- warm caches --------------------------------------------------------

    /// Every indexed file path, as a set (the `matchByFilePath` existence probe).
    fn known_files(&self) -> &HashSet<String>;

    /// Every distinct node name in the graph — the `has_any_possible_match`
    /// pre-filter's hash lookup, hit once per reference.
    fn known_names(&self) -> &HashSet<String>;

    // ---- health -------------------------------------------------------------

    /// How many store reads have **failed** over this context's life.
    ///
    /// # Why this exists, and why it is on the trait
    ///
    /// Every read below degrades a store malfunction to an empty result (errors
    /// are collected, never thrown — a reference that cannot be looked up is a
    /// reference that does not resolve, and unwinding through a hundred strategy
    /// frames would take down an index of a million over one bad row). The
    /// hazard is that this makes a **store outage byte-identical to a clean
    /// no-match**: nothing resolves, and nothing says why — which is exactly the
    /// *vacuous resolution* the Phase 3 gate exists to catch, arriving through
    /// the back door.
    ///
    /// So the degradation is counted, not silent. Part C's batch driver reports
    /// this in its stats, and the gate's non-vacuity assertion fails the run when
    /// it is non-zero: **a resolution pass that swallowed store errors is not a
    /// resolution pass, it is a lie about one.**
    ///
    /// The default is `0` — an in-memory context has no store to fail.
    fn store_read_errors(&self) -> u64 {
        0
    }
}

// =============================================================================
// StoreContext
// =============================================================================

/// The `GraphStore`-backed [`ResolutionContext`].
///
/// Generic over `S: GraphStore` — **never** tied to `SurrealStore` (a Phase 3
/// Global Constraint: the store is a seam, and the resolver's tests mock it).
pub struct StoreContext<S: GraphStore> {
    store: S,
    root: PathBuf,
    /// The runtime the (sync) strategies drive the (async) store through. See
    /// the module docs: this only works off a runtime worker thread.
    handle: tokio::runtime::Handle,

    // Warm caches — computed once, at construction.
    all_files: Vec<String>,
    files_with_language: Vec<(String, Language)>,
    languages: BTreeSet<Language>,
    known_files: HashSet<String>,
    known_names: HashSet<String>,

    // LRU caches (maps/resolution.md §Caches).
    node_cache: SyncLru<String, Vec<Node>>, // file → nodes
    name_cache: SyncLru<String, Vec<Node>>, // name → nodes
    lower_name_cache: SyncLru<String, Vec<Node>>, // lower(name) → nodes
    qualified_name_cache: SyncLru<String, Vec<Node>>,
    method_match_cache: SyncLru<String, Vec<Node>>, // "{lang} {ty}::{method}"
    node_by_id_cache: SyncLru<String, Option<Node>>,
    count_cache: SyncLru<String, u64>,
    node_count_cache: SyncLru<String, u64>,
    supertype_cache: SyncLru<String, Vec<Node>>,
    member_cache: SyncLru<String, Vec<Node>>,
    file_cache: SyncLru<String, Option<String>>, // content-bearing
    file_lines_cache: SyncLru<String, Option<Arc<Vec<String>>>>, // content-bearing
    import_mapping_cache: SyncLru<String, Arc<Vec<ImportMapping>>>,
    re_export_cache: SyncLru<String, Arc<Vec<ReExport>>>,

    /// ~24 kinds, never evicted — a plain map, not an LRU (`#1180`).
    kind_cache: std::sync::Mutex<HashMap<NodeKind, Vec<Node>>>,

    /// Store reads that FAILED. See [`ResolutionContext::store_read_errors`] —
    /// a swallowed store error must never be indistinguishable from a clean miss.
    store_read_errors: AtomicU64,

    // Project singletons. `None` = absent (Task 4 populates them at construction).
    aliases: Option<AliasMap>,
    go_module: Option<GoModule>,
    workspace_packages: Option<WorkspacePackages>,
    cpp_include_dirs: Vec<String>,
}

impl<S: GraphStore> StoreContext<S> {
    /// Build a context over `store`, warming the caches every reference hits.
    ///
    /// `async` because warming is four store queries; **the resulting context's
    /// own methods are sync**, and calling them from inside an async task will
    /// panic in `Handle::block_on` — run the resolver under `spawn_blocking`
    /// (see the module docs).
    ///
    /// # Errors
    /// Only a genuine store malfunction, or being constructed with no tokio
    /// runtime at all (a caller wiring bug).
    pub async fn new(store: S, root: PathBuf) -> Result<Self> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|e| ResolveError::NoRuntime(e.to_string()))?;

        // --- the warm caches: one query each -------------------------------
        let files = store.all_files().await?;
        let mut files_with_language: Vec<(String, Language)> = files
            .iter()
            .filter_map(|f| Language::from_wire(&f.language).map(|l| (f.path.clone(), l)))
            .collect();
        files_with_language.sort_by(|a, b| a.0.cmp(&b.0));

        let mut all_files: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        all_files.sort();
        let known_files: HashSet<String> = all_files.iter().cloned().collect();

        // A file whose stored language string is not a known wire value is
        // dropped from `files_with_language` (and so from `languages`) rather
        // than guessed at — see `Language::from_wire`'s docs. It stays in
        // `all_files`/`known_files`: it IS an indexed file, we just cannot type it.
        let languages: BTreeSet<Language> = files_with_language.iter().map(|(_, l)| *l).collect();

        let known_names: HashSet<String> = store.all_node_names().await?.into_iter().collect();

        let limit = cache_limit();
        let content_limit = content_cache_limit(limit);

        Ok(Self {
            store,
            root,
            handle,
            all_files,
            files_with_language,
            languages,
            known_files,
            known_names,
            node_cache: SyncLru::new(limit),
            name_cache: SyncLru::new(limit),
            lower_name_cache: SyncLru::new(limit),
            qualified_name_cache: SyncLru::new(limit),
            method_match_cache: SyncLru::new(limit),
            node_by_id_cache: SyncLru::new(limit),
            count_cache: SyncLru::new(limit),
            node_count_cache: SyncLru::new(limit),
            supertype_cache: SyncLru::new(limit),
            member_cache: SyncLru::new(limit),
            file_cache: SyncLru::new(content_limit),
            file_lines_cache: SyncLru::new(content_limit),
            import_mapping_cache: SyncLru::new(limit),
            re_export_cache: SyncLru::new(limit),
            kind_cache: std::sync::Mutex::new(HashMap::new()),
            store_read_errors: AtomicU64::new(0),
            aliases: None,
            go_module: None,
            workspace_packages: None,
            cpp_include_dirs: Vec::new(),
        })
    }

    /// The store, for the passes that write (Part C's batch driver).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Drive one async store read from the sync strategy layer.
    ///
    /// A store malfunction **degrades to an empty result** rather than unwinding
    /// through a hundred strategy frames: errors are collected, never thrown, and
    /// a reference that cannot be looked up is a reference that does not resolve.
    ///
    /// But it is **counted and logged**, never silent. A swallowed store error
    /// that left no trace would make an outage look byte-identical to a repo with
    /// nothing to resolve — the vacuous-resolution failure mode, arriving through
    /// the back door. [`Self::store_read_errors`] is what the batch driver's stats
    /// and the gate's non-vacuity assertion read.
    fn blocking<T, F>(&self, fut: F) -> Option<T>
    where
        F: Future<Output = std::result::Result<T, selene_db::Error>>,
    {
        match self.handle.block_on(fut) {
            Ok(value) => Some(value),
            Err(e) => {
                let count = self.store_read_errors.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(
                    error = %e,
                    store_read_errors = count,
                    "graph-store read failed during resolution; degrading this lookup to \
                     an empty result. References that needed it will not resolve."
                );
                None
            }
        }
    }

    /// Store reads that have failed over this context's life (see the trait's
    /// [`ResolutionContext::store_read_errors`] for why this is not optional).
    pub fn store_read_error_count(&self) -> u64 {
        self.store_read_errors.load(Ordering::Relaxed)
    }

    /// Drop every cached read (`clear_caches()`): the node caches go stale the
    /// moment a framework's `post_extract` mutates a node.
    pub fn clear_caches(&self) {
        self.node_cache.clear();
        self.name_cache.clear();
        self.lower_name_cache.clear();
        self.qualified_name_cache.clear();
        self.method_match_cache.clear();
        self.node_by_id_cache.clear();
        self.count_cache.clear();
        self.node_count_cache.clear();
        self.supertype_cache.clear();
        self.member_cache.clear();
        self.file_cache.clear();
        self.file_lines_cache.clear();
        self.import_mapping_cache.clear();
        self.re_export_cache.clear();
        if let Ok(mut kinds) = self.kind_cache.lock() {
            kinds.clear();
        }
    }
}

impl<S: GraphStore> ResolutionContext for StoreContext<S> {
    fn nodes_in_file(&self, path: &str) -> Vec<Node> {
        self.node_cache.get_or_insert_with(path.to_string(), || {
            self.blocking(self.store.get_nodes_by_file(path))
                .unwrap_or_default()
        })
    }

    fn nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.name_cache.get_or_insert_with(name.to_string(), || {
            self.blocking(self.store.get_nodes_by_name(name))
                .unwrap_or_default()
        })
    }

    fn nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
        self.lower_name_cache
            .get_or_insert_with(lower.to_string(), || {
                self.blocking(self.store.get_nodes_by_name_ci(lower))
                    .unwrap_or_default()
            })
    }

    fn nodes_by_qualified_name(&self, qn: &str) -> Vec<Node> {
        self.qualified_name_cache
            .get_or_insert_with(qn.to_string(), || {
                self.blocking(self.store.get_nodes_by_qualified_name(qn))
                    .unwrap_or_default()
            })
    }

    fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        if let Ok(cache) = self.kind_cache.lock()
            && let Some(hit) = cache.get(&kind)
        {
            return hit.clone();
        }
        let fetched = self
            .blocking(self.store.get_nodes_by_kind(kind))
            .unwrap_or_default();
        if let Ok(mut cache) = self.kind_cache.lock() {
            cache.insert(kind, fetched.clone());
        }
        fetched
    }

    fn node_by_id(&self, id: &str) -> Option<Node> {
        self.node_by_id_cache
            .get_or_insert_with(id.to_string(), || {
                self.blocking(self.store.get_node(id)).flatten()
            })
    }

    fn count_files_with_name(&self, name: &str) -> u64 {
        self.count_cache.get_or_insert_with(name.to_string(), || {
            self.blocking(self.store.count_nodes_matching_name_in_files(name))
                .unwrap_or(0)
        })
    }

    fn count_nodes_named(&self, name: &str) -> u64 {
        self.node_count_cache
            .get_or_insert_with(name.to_string(), || {
                self.blocking(self.store.count_nodes_named(name))
                    .unwrap_or(0)
            })
    }

    fn method_matches(&self, language: Language, ty: &str, method: &str) -> Vec<Node> {
        // The memo key is the TS one, verbatim: `${language} ${type}::${method}`.
        let key = format!("{} {ty}::{method}", language.as_str());
        self.method_match_cache.get_or_insert_with(key, || {
            // No suffix query exists on the store (spike F2): fetch by method
            // name, filter in-resolver. The filter is the SAFETY mechanism —
            // an absent method yields no match, hence no edge, never a wrong one.
            let exact = format!("{ty}::{method}");
            let suffix = format!("::{ty}::{method}");
            self.nodes_by_name(method)
                .into_iter()
                .filter(|n| {
                    n.kind == NodeKind::Method
                        && Language::from_wire(&n.language) == Some(language)
                        && (n.qualified_name == exact || n.qualified_name.ends_with(&suffix))
                })
                .collect()
        })
    }

    fn supertypes(&self, node_id: &str) -> Vec<Node> {
        self.supertype_cache
            .get_or_insert_with(node_id.to_string(), || {
                self.blocking(self.store.outgoing(
                    node_id,
                    &[EdgeKind::Implements, EdgeKind::Extends],
                    None,
                ))
                .unwrap_or_default()
                .into_iter()
                .map(|n| n.node)
                .collect()
            })
    }

    fn members_of(&self, node_id: &str) -> Vec<Node> {
        self.member_cache
            .get_or_insert_with(node_id.to_string(), || {
                self.blocking(self.store.children(node_id))
                    .unwrap_or_default()
            })
    }

    fn project_root(&self) -> &Path {
        &self.root
    }

    fn file_exists(&self, path: &str) -> bool {
        self.known_files.contains(path)
    }

    fn read_file(&self, path: &str) -> Option<String> {
        self.file_cache.get_or_insert_with(path.to_string(), || {
            // Path traversal: a reference-derived path must not escape the root.
            // (`join` on an absolute path would replace the root outright.)
            let rel = Path::new(path);
            if rel.is_absolute() || rel.components().any(|c| c.as_os_str() == "..") {
                return None;
            }
            std::fs::read_to_string(self.root.join(rel)).ok()
        })
    }

    fn file_lines(&self, path: &str) -> Option<Arc<Vec<String>>> {
        self.file_lines_cache
            .get_or_insert_with(path.to_string(), || {
                self.read_file(path)
                    .map(|src| Arc::new(src.lines().map(str::to_string).collect()))
            })
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

    fn list_directories(&self, path: &str) -> Vec<String> {
        let rel = Path::new(path);
        if rel.is_absolute() || rel.components().any(|c| c.as_os_str() == "..") {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(self.root.join(rel)) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        out.sort(); // determinism: read_dir order is filesystem-defined
        out
    }

    fn import_mappings(&self, path: &str) -> Arc<Vec<ImportMapping>> {
        // Task 4 wires `imports::mappings::extract_import_mappings` in here; the
        // cache and the seam exist now so the trait is complete for Parts B/C.
        self.import_mapping_cache
            .get_or_insert_with(path.to_string(), || Arc::new(Vec::new()))
    }

    fn re_exports(&self, path: &str) -> Arc<Vec<ReExport>> {
        // Task 4 wires `imports::mappings::extract_re_exports` in here.
        self.re_export_cache
            .get_or_insert_with(path.to_string(), || Arc::new(Vec::new()))
    }

    fn project_aliases(&self) -> Option<&AliasMap> {
        self.aliases.as_ref() // Task 4 loads it at construction.
    }

    fn go_module(&self) -> Option<&GoModule> {
        self.go_module.as_ref() // Task 4.
    }

    fn workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace_packages.as_ref() // Task 4.
    }

    fn cpp_include_dirs(&self) -> &[String] {
        &self.cpp_include_dirs // Task 5.
    }

    fn known_files(&self) -> &HashSet<String> {
        &self.known_files
    }

    fn known_names(&self) -> &HashSet<String> {
        &self.known_names
    }

    fn store_read_errors(&self) -> u64 {
        self.store_read_error_count()
    }
}
