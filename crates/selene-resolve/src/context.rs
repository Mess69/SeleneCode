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

/// How many blocking store reads the ladder made, and how long it sat in them.
///
/// This exists because the module docs *claimed* resolution was "CPU-bound over a warm cache, not
/// I/O-bound" and it was not: on django the lazy path made **32 524 blocking reads and spent
/// 4 810 ms — 69% of the ladder — waiting on them**. With the eager index it makes **48**. Keep the
/// counter: it is the difference between an assertion and a measurement, and it costs two atomics.
pub static BLOCKING_CALLS: AtomicU64 = AtomicU64::new(0);
pub static BLOCKING_NANOS: AtomicU64 = AtomicU64::new(0);
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use selene_core::{EdgeKind, Language, Node, NodeKind};
use selene_db::GraphStore;

use crate::cache::{SyncLru, cache_limit, content_cache_limit};
use crate::error::{ResolveError, Result};
use crate::imports::aliases::load_project_aliases;
use crate::imports::cpp_includes::load_cpp_include_dirs;
use crate::imports::go_module::load_go_module;
use crate::imports::mappings::{extract_import_mappings, extract_re_exports};
use crate::imports::workspace::load_workspace_packages;
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
    ///
    /// Returns `Arc<Node>`, not `Node`: the ladder calls these accessors millions of times and a
    /// deep `Vec<Node>` clone per call (each `Node` is ~half a dozen heap `String`s) is the hot cost
    /// — `resolve/3` was 189 s on VS Code. An `Arc` clone is a refcount bump, the same cheap
    /// reference-passing the TS original got free from its GC (~15× on a 10k-element group). The
    /// eager index stores each node once behind an `Arc`.
    fn nodes_in_file(&self, path: &str) -> Vec<Arc<Node>>;

    /// Every node whose `name` matches exactly (case-sensitive).
    fn nodes_by_name(&self, name: &str) -> Vec<Arc<Node>>;

    /// Every node whose lower-cased `name` equals `lower` (fuzzy matching,
    /// Task 7). `lower` is expected **pre-lowercased** by the caller.
    fn nodes_by_lower_name(&self, lower: &str) -> Vec<Arc<Node>>;

    /// Every node whose `qualified_name` matches exactly (overloads can share
    /// one, so this is a `Vec`, not an `Option`).
    fn nodes_by_qualified_name(&self, qn: &str) -> Vec<Arc<Node>>;

    /// Every node of exactly `kind`.
    fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Arc<Node>>;

    /// Point lookup by id.
    fn node_by_id(&self, id: &str) -> Option<Arc<Node>>;

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
    fn method_matches(&self, language: Language, ty: &str, method: &str) -> Vec<Arc<Node>>;

    /// The supertypes of `node_id`: the targets of its outgoing
    /// `implements`/`extends` edges.
    ///
    /// **Node-anchored, never name-keyed.** A name-keyed `get_supertypes("Engine")`
    /// unioned every rails `Engine`'s parents and produced a cross-class wrong
    /// edge (`design/function-ref-capture.md` §Known limits); the node walk
    /// eliminated it.
    fn supertypes(&self, node_id: &str) -> Vec<Arc<Node>>;

    /// The `contains` children of `node_id` — a class's members.
    fn members_of(&self, node_id: &str) -> Vec<Arc<Node>>;

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

    // ---- cache lifecycle ----------------------------------------------------

    /// Drop every cached read.
    ///
    /// The conformance passes (`passes.rs`) call this **before** they retry a
    /// deferred reference: the first pass created the `implements`/`extends` edges
    /// those retries depend on, and a stale cache would hide them — making the
    /// pass a silent no-op that looks like it ran. A framework's `post_extract`
    /// (Part B) mutates nodes and needs the same.
    ///
    /// Default: a no-op (an in-memory context has nothing to invalidate).
    fn clear_caches(&self) {}

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
    node_cache: SyncLru<String, Vec<Arc<Node>>>, // file → nodes
    name_cache: SyncLru<String, Vec<Arc<Node>>>, // name → nodes
    lower_name_cache: SyncLru<String, Vec<Arc<Node>>>, // lower(name) → nodes
    qualified_name_cache: SyncLru<String, Vec<Arc<Node>>>,
    method_match_cache: SyncLru<String, Vec<Arc<Node>>>, // "{lang} {ty}::{method}"
    node_by_id_cache: SyncLru<String, Option<Arc<Node>>>,

    /// **Every node, indexed in memory.** `None` on a repo too large to hold (see `EAGER_MAX`),
    /// where the lazy LRU path below stays in charge.
    ///
    /// The lazy path is what the module docs describe as "CPU-bound over a warm cache, not
    /// I/O-bound". It is not. Measured on django: the ladder made **32 524 blocking store reads
    /// and spent 4 810 ms — 69% of its 6 839 ms — waiting on them**, on one thread, to interrogate
    /// a table of 19 061 rows. `get_node`, a point lookup by primary key, fired **14 674 times**.
    ///
    /// The LRU cannot fix this and its size is not the problem: raising the limit from 5 000 to
    /// 200 000 removed only 8% of the reads. They are **cold misses** — 12 279 distinct names, each
    /// fetched exactly once. A lazy cache pays one round trip per distinct key, forever.
    ///
    /// One scan replaces all of them.
    eager: Option<EagerIndex>,
    count_cache: SyncLru<String, u64>,
    node_count_cache: SyncLru<String, u64>,
    supertype_cache: SyncLru<String, Vec<Arc<Node>>>,
    member_cache: SyncLru<String, Vec<Arc<Node>>>,
    file_cache: SyncLru<String, Option<String>>, // content-bearing
    file_lines_cache: SyncLru<String, Option<Arc<Vec<String>>>>, // content-bearing
    import_mapping_cache: SyncLru<String, Arc<Vec<ImportMapping>>>,
    re_export_cache: SyncLru<String, Arc<Vec<ReExport>>>,

    /// ~24 kinds, never evicted — a plain map, not an LRU (`#1180`).
    kind_cache: std::sync::Mutex<HashMap<NodeKind, Vec<Arc<Node>>>>,

    /// Store reads that FAILED. See [`ResolutionContext::store_read_errors`] —
    /// a swallowed store error must never be indistinguishable from a clean miss.
    store_read_errors: AtomicU64,

    // Project singletons. `None` = absent (Task 4 populates them at construction).
    aliases: Option<AliasMap>,
    go_module: Option<GoModule>,
    workspace_packages: Option<WorkspacePackages>,
    cpp_include_dirs: Vec<String>,
}

/// A repo above this many nodes keeps the lazy LRU path. The eager index stores each node ONCE
/// behind an `Arc` (see `EagerIndex`/`deref_clone`); the four lookup groups + by_id hold 8-byte
/// pointer-clones, so it is ~one node's bytes per symbol plus pointers — NOT the 4-5 deep copies it
/// held before, which were the bulk of the indexer's peak RSS at VS Code scale.
const EAGER_MAX: usize = 2_000_000;

/// Every node, grouped the four ways the ladder asks for them.
///
/// ⚠ **Insertion order inside each group is the scan order, and it is LOAD-BEARING.**
/// `select_nodes_where` has no `ORDER BY`, so the store returns whatever its index scan yields, and
/// `best_candidate` breaks ties by taking the earlier candidate (there is a test named
/// `best_candidate_keeps_the_earlier_on_a_tie`). Rebuilding these groups from a full scan must
/// therefore reproduce the same order — which is exactly what the graph diff at the end of this
/// change verifies, rather than assumes.
struct EagerIndex {
    by_id: HashMap<String, Arc<Node>>,
    by_name: HashMap<String, Vec<Arc<Node>>>,
    by_lower: HashMap<String, Vec<Arc<Node>>>,
    by_qname: HashMap<String, Vec<Arc<Node>>>,
    by_file: HashMap<String, Vec<Arc<Node>>>,
}

/// Deref-clone a group slice into the owned `Vec<Node>` the `ResolutionContext` trait returns.
/// The eager index stores each node ONCE behind an `Arc` (the four groups hold cheap 8-byte
/// pointer-clones, not 4-5 deep copies of every node — that duplication was ~5× the node bytes
/// resident and the bulk of the indexer's peak RSS on a large repo). The deep copy is paid only
/// here, on the SMALL per-query result set, and freed as soon as the caller drops it.
fn arc_vec(v: Option<Vec<Node>>) -> Vec<Arc<Node>> {
    v.unwrap_or_default().into_iter().map(Arc::new).collect()
}

/// Deref-clone an `Arc<Node>` group back into owned `Node`s — the CONTAINMENT boundary for the cold
/// resolvers (framework/synth passes) that still work in owned `Node`s. The hot ladder path works in
/// `Arc<Node>` end-to-end (no clone); these rarely-hit paths pay the deep copy here to stay simple.
pub(crate) fn owned(v: Vec<Arc<Node>>) -> Vec<Node> {
    v.into_iter().map(|a| a.as_ref().clone()).collect()
}


impl EagerIndex {
    fn build(nodes: Vec<Node>) -> Self {
        let mut ix = EagerIndex {
            by_id: HashMap::with_capacity(nodes.len()),
            by_name: HashMap::new(),
            by_lower: HashMap::new(),
            by_qname: HashMap::new(),
            by_file: HashMap::new(),
        };
        for n in nodes {
            // One heap Node per symbol; the four groups + by_id share it by refcount.
            let n = Arc::new(n);
            ix.by_name
                .entry(n.name.clone())
                .or_default()
                .push(Arc::clone(&n));
            ix.by_lower
                .entry(n.name.to_lowercase())
                .or_default()
                .push(Arc::clone(&n));
            if !n.qualified_name.is_empty() {
                ix.by_qname
                    .entry(n.qualified_name.clone())
                    .or_default()
                    .push(Arc::clone(&n));
            }
            ix.by_file
                .entry(n.file_path.clone())
                .or_default()
                .push(Arc::clone(&n));
            ix.by_id.insert(n.id.clone(), n);
        }
        ix
    }
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

        // --- the eager node index: ONE scan, replacing 32 524 lazy point lookups ----
        let stats = store.stats().await?;
        let eager = if (stats.nodes as usize) <= EAGER_MAX {
            let t = std::time::Instant::now();
            let nodes = store.all_nodes().await?;
            let n = nodes.len();
            let ix = EagerIndex::build(nodes);
            tracing::info!(
                target: "selene::index",
                nodes = n,
                ms = t.elapsed().as_millis(),
                "resolve/0: eager node index (one scan, replaces the ladder's blocking point lookups)"
            );
            Some(ix)
        } else {
            tracing::warn!(
                target: "selene::index",
                nodes = stats.nodes,
                cap = EAGER_MAX,
                "repo too large for the eager node index — falling back to the lazy LRU path, \
                 which pays one BLOCKING store read per distinct key"
            );
            None
        };

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

        let aliases = load_project_aliases(&root);
        let go_module = load_go_module(&root);
        let workspace_packages = load_workspace_packages(&root);
        let cpp_include_dirs = load_cpp_include_dirs(&root);

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
            eager,
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
            // ⚠ These four were `None` / empty — the loaders existed, were tested, and
            // were never called. Same bug as `import_mappings`, same blast radius: a
            // missing `go.mod` makes `resolve_go_cross_package` (#388) return None for
            // EVERY Go cross-package call; a missing alias map makes every `@/lib/x`
            // import unresolvable; and none of it fails, it just quietly resolves
            // nothing. The resolution parity gate is what found them (TS resolved
            // gin's `service.Create()`, we did not).
            //
            // Loaded once, here, at construction — they are project singletons and a
            // per-reference read would be quadratic.
            aliases,
            go_module,
            workspace_packages,
            cpp_include_dirs,
        })
    }

    /// The store, for the passes that write (Part C's batch driver).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Take the store back, discarding the context.
    ///
    /// The context's caches (`known_names`, nodes-by-name, file lists) are
    /// **warmed once, at construction**. Any pass that WRITES nodes — the
    /// framework extract pass emits route nodes — therefore invalidates them:
    /// a context built before emission does not know the routes exist, and would
    /// pre-filter away every reference the frameworks just emitted ("that name
    /// matches no symbol"). The fix is to rebuild the context after such a pass,
    /// which means getting the store back out. Hence this.
    pub fn into_store(self) -> S {
        self.store
    }

    /// A file's language, off the warm `files_with_language` cache — a binary search,
    /// not a store read. The import-mapping loader needs it per file.
    fn language_of(&self, path: &str) -> Option<Language> {
        self.files_with_language
            .binary_search_by(|(p, _)| p.as_str().cmp(path))
            .ok()
            .map(|i| self.files_with_language[i].1)
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
    fn blocking<T, F>(&self, _label: &'static str, fut: F) -> Option<T>
    where
        F: Future<Output = std::result::Result<T, selene_db::Error>>,
    {
        // TEMP INSTRUMENTATION: how often does the "warm cache" actually miss?
        let __t = std::time::Instant::now();
        let __r = self.handle.block_on(fut);
        BLOCKING_CALLS.fetch_add(1, Ordering::Relaxed);
        BLOCKING_NANOS.fetch_add(__t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        match __r {
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
    fn nodes_in_file(&self, path: &str) -> Vec<Arc<Node>> {
        if let Some(ix) = &self.eager {
            return ix.by_file.get(path).cloned().unwrap_or_default();
        }
        self.node_cache.get_or_insert_with(path.to_string(), || {
            arc_vec(self.blocking("get_nodes_by_file", self.store.get_nodes_by_file(path)))
        })
    }

    fn nodes_by_name(&self, name: &str) -> Vec<Arc<Node>> {
        if let Some(ix) = &self.eager {
            return ix.by_name.get(name).cloned().unwrap_or_default();
        }
        self.name_cache.get_or_insert_with(name.to_string(), || {
            arc_vec(self.blocking("get_nodes_by_name", self.store.get_nodes_by_name(name)))
        })
    }

    fn nodes_by_lower_name(&self, lower: &str) -> Vec<Arc<Node>> {
        if let Some(ix) = &self.eager {
            return ix.by_lower.get(lower).cloned().unwrap_or_default();
        }
        self.lower_name_cache.get_or_insert_with(lower.to_string(), || {
            arc_vec(self.blocking("get_nodes_by_name_ci", self.store.get_nodes_by_name_ci(lower)))
        })
    }

    fn nodes_by_qualified_name(&self, qn: &str) -> Vec<Arc<Node>> {
        if let Some(ix) = &self.eager {
            return ix.by_qname.get(qn).cloned().unwrap_or_default();
        }
        self.qualified_name_cache.get_or_insert_with(qn.to_string(), || {
            arc_vec(self.blocking(
                "get_nodes_by_qualified_name",
                self.store.get_nodes_by_qualified_name(qn),
            ))
        })
    }

    fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Arc<Node>> {
        if let Ok(cache) = self.kind_cache.lock()
            && let Some(hit) = cache.get(&kind)
        {
            return hit.clone();
        }
        let fetched = arc_vec(self.blocking("get_nodes_by_kind", self.store.get_nodes_by_kind(kind)));
        if let Ok(mut cache) = self.kind_cache.lock() {
            cache.insert(kind, fetched.clone());
        }
        fetched
    }

    fn node_by_id(&self, id: &str) -> Option<Arc<Node>> {
        if let Some(ix) = &self.eager {
            return ix.by_id.get(id).cloned();
        }
        self.node_by_id_cache.get_or_insert_with(id.to_string(), || {
            self.blocking("get_node", self.store.get_node(id)).flatten().map(Arc::new)
        })
    }

    fn count_files_with_name(&self, name: &str) -> u64 {
        self.count_cache.get_or_insert_with(name.to_string(), || {
            self.blocking(
                "count_nodes_matching_name_in_files",
                self.store.count_nodes_matching_name_in_files(name),
            )
            .unwrap_or(0)
        })
    }

    fn count_nodes_named(&self, name: &str) -> u64 {
        self.node_count_cache
            .get_or_insert_with(name.to_string(), || {
                self.blocking("count_nodes_named", self.store.count_nodes_named(name))
                    .unwrap_or(0)
            })
    }

    fn method_matches(&self, language: Language, ty: &str, method: &str) -> Vec<Arc<Node>> {
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

    fn supertypes(&self, node_id: &str) -> Vec<Arc<Node>> {
        self.supertype_cache
            .get_or_insert_with(node_id.to_string(), || {
                self.blocking(
                    "outgoing",
                    self.store
                        .outgoing(node_id, &[EdgeKind::Implements, EdgeKind::Extends], None),
                )
                .unwrap_or_default()
                .into_iter()
                .map(|n| Arc::new(n.node)).collect()
            })
    }

    fn members_of(&self, node_id: &str) -> Vec<Arc<Node>> {
        self.member_cache
            .get_or_insert_with(node_id.to_string(), || {
                arc_vec(self.blocking("children", self.store.children(node_id)))
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
        // ⚠ This was a STUB returning an empty Vec, and the resolution parity gate
        // is what found it. An empty mapping list does not fail — it makes the whole
        // of ladder step 8 (`resolve_via_import`) a silent no-op: Go cross-package
        // (#388), JVM FQN imports (#314), barrel re-exports (#629), path aliases,
        // Python module members, Rust paths, C/C++ includes — every one of them reads
        // this list, and every one of them was inert in the real pipeline while the
        // strategy tests passed, because `FakeContext` injects the mappings directly.
        //
        // The lesson is the fake's: a seam that returns "nothing found" is
        // indistinguishable from a seam that works and found nothing.
        self.import_mapping_cache
            .get_or_insert_with(path.to_string(), || {
                let Some(language) = self.language_of(path) else {
                    return Arc::new(Vec::new());
                };
                let Some(source) = self.read_file(path) else {
                    return Arc::new(Vec::new());
                };
                Arc::new(extract_import_mappings(path, &source, language))
            })
    }

    fn re_exports(&self, path: &str) -> Arc<Vec<ReExport>> {
        // Same stub, same consequence: an un-wired barrel loader means a renamed
        // re-export (`export { signIn as login }`) resolves to nothing, for every
        // project, silently.
        self.re_export_cache
            .get_or_insert_with(path.to_string(), || match self.read_file(path) {
                Some(source) => Arc::new(extract_re_exports(&source, path)),
                None => Arc::new(Vec::new()),
            })
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

    fn clear_caches(&self) {
        StoreContext::clear_caches(self);
    }
}
