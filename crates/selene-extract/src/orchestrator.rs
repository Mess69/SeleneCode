//! The extraction orchestrator — the `ExtractionOrchestrator` port
//! (extraction-core.md §2/§4, minus everything the WASM layer justified):
//! scan → rayon parse fan-out → **ordered DB commit** → incremental
//! re-index. Tokio appears ONLY at the DB seam (selene-db is async);
//! extraction itself stays sync — each batch is `spawn_blocking`-bridged
//! into a dedicated rayon pool and its results are committed sequentially
//! on the async task.
//!
//! ## The #1015 ordered-commit invariant
//!
//! Batches parse in parallel (`par_iter().map(..).collect()` is
//! order-preserving), but every commit happens **sequentially in scan
//! order** (the scan list is sorted; commits follow it). Resolution
//! disambiguates same-named symbols by insertion order, so commit order is
//! part of the determinism contract: two runs over the same tree produce
//! identical ids AND identical stats.
//!
//! ## Deferred-FTS bulk mode
//!
//! `index_all` brackets its run with [`GraphStore::bulk_load_begin`] /
//! [`GraphStore::bulk_load_finish`] — the Phase 1 API built for exactly this
//! path (§5.3 remediation: inline FTS maintenance capped node ingest at 803
//! nodes/s vs 4,703 without; 2.15x total-load win). `bulk_load_begin` is
//! also the initializing entry point, so `index_all` works on a fresh
//! store. Single-file incremental re-index (`index_file`) deliberately does
//! NOT use bulk mode — dropping and rebuilding the FTS indexes for one file
//! costs more than inline maintenance.
//!
//! **The bracket is exit-safe.** Inside it the four FULLTEXT indexes are
//! DROPPED and `search_fts` is success-shaped-EMPTY (never `Err` — the
//! `isError` reservation, PRD §8.2), so a run that returns without calling
//! `bulk_load_finish` leaves the store *silently* unsearchable until some
//! later run completes. Two rules keep that impossible rather than merely
//! handled: the scan runs BEFORE `bulk_load_begin` (it touches no store
//! state, so its failure never enters the bracket), and every path after
//! `begin` — store malfunctions included, since they are collected, never
//! returned — falls through to `bulk_load_finish`.
//!
//! ## Recursion/depth guard (Task 5 review, Minor 4)
//!
//! The walker's `visit` recurses per AST level; native stack overflow in
//! Rust ABORTS the process (unlike the TS build, where the same depth threw
//! a catchable `RangeError` that became a `parse_error`). The orchestrator
//! therefore (a) runs parses on pool threads with an explicit
//! [`PARSE_STACK_SIZE`] stack, and (b) pre-rejects adversarially nested
//! sources: a file whose maximum bracket-nesting depth exceeds
//! [`MAX_NESTING_DEPTH`] is converted into a collected
//! [`ExtractionError`] (`parse_error`, severity Error) instead of being
//! parsed — errors-collected, the pipeline never crashes. The bracket scan
//! is a deliberate upper-bound heuristic: brackets inside strings/comments
//! count too, which only makes the guard MORE conservative, and the
//! threshold is far above real-world nesting (deepest observed real code is
//! well under 200).
//!
//! ## Deliberately dropped (extraction-core.md §Rust port notes) — each named:
//!
//! - **`ParseWorkerPool`** + its recycle / crash-budget / late-result
//!   policy (`WORKER_RECYCLE_INTERVAL`, `CRASH_BUDGET`,
//!   `MAX_CONCURRENT_SPAWN`, `HARD_KILL_MULTIPLIER`) — worker processes
//!   existed to contain WASM-heap corruption; rayon threads share the
//!   process and native grammars don't corrupt heaps.
//! - **`PARSER_RESET_INTERVAL`** (per-5000-parse parser resets) — WASM heap
//!   growth; native parsers are cheap and constructed per extraction.
//! - **WASM-OOM retry + comment-blank passes** and the string-matched retry
//!   pass (`'Worker exited'` / `'memory access out of bounds'` /
//!   `'timed out'`) — no WASM, no OOM class to retry.
//! - **`wasm-runtime-flags`** (V8 `--liftoff-only` re-exec) — no V8.
//! - **Grammar-bytes pre-read** (`readGrammarWasmBytes`) and **sequential
//!   grammar loading** (web-tree-sitter race, tree-sitter#2338) — grammars
//!   are statically linked; consequently the TS "needed languages =
//!   detectLanguage set (+cpp when c present)" pre-pass, whose ONLY
//!   consumer was that loading, folds away with it (per-file detection
//!   still happens at read time, content sniff included).
//! - **`FILE_IO_BATCH_SIZE = 10`** — folded into [`parse_batch`] (map §2):
//!   without worker threads a separate read-batch size buys nothing.
//! - **Per-parse timeout** — Task 1 found a workable API
//!   (`parse_with_options` progress-callback cancellation), but it guards
//!   the PARSE only, not the walk, and native parses on ≤ 1 MiB inputs are
//!   milliseconds; the nesting guard above covers the pathological class.
//!   Revisit if profiling ever disagrees.
//!
//! NOT ported here (map §4): `sync()` / `getChangedFiles()` → Phase 6
//! (`selene-sync`); framework detection → Phase 3 (an empty framework-name
//! list is threaded internally so the seam exists).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use selene_core::{EXTRACTION_VERSION, hash_content};
use selene_db::{FileRecord, GraphStore, RefStatus, ReplaceStats, UnresolvedRef};

use crate::scan::{ScanOverrides, scan_directory};
use crate::{
    ErrorCode, ExtractionError, ExtractionResult, Language, MAX_FILE_SIZE, Severity,
    UnresolvedReference, detect_language, extract_from_source,
};

/// Hard cap on parse workers (TS: `Math.min(…, 16)`).
const MAX_PARSE_WORKERS: usize = 16;

/// Explicit stack size for parse-pool threads: the walker recurses per AST
/// level, and rayon's default (2 MiB) leaves little headroom for legal deep
/// trees (expression chains nest without brackets). 16 MiB is virtual
/// reserve, not committed memory.
const PARSE_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Maximum bracket-nesting depth accepted before a source is rejected as
/// adversarial (module docs: an upper-bound proxy for AST depth; real code
/// stays orders of magnitude below, and 4096 frames fit comfortably in
/// [`PARSE_STACK_SIZE`]).
pub const MAX_NESTING_DEPTH: usize = 4096;

/// The meta key `index_all` persists [`EXTRACTION_VERSION`] under.
const EXTRACTION_VERSION_META_KEY: &str = "extraction_version";

/// Parse workers: `SELENE_PARSE_WORKERS` (capped at [`MAX_PARSE_WORKERS`])
/// or `clamp(cores-1, 1, 8)` — the TS `resolveParsePoolSize` rule with the
/// `SELENE_` env prefix.
fn parse_workers() -> usize {
    if let Ok(raw) = std::env::var("SELENE_PARSE_WORKERS")
        && let Ok(n) = raw.trim().parse::<usize>()
        && n > 0
    {
        return n.min(MAX_PARSE_WORKERS);
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    cores.saturating_sub(1).clamp(1, 8)
}

/// `PARSE_BATCH = max(4, workers * 2)` — the map §2 rolling-window size,
/// reused as the read+parse batch (FILE_IO_BATCH_SIZE folds in, module
/// docs).
fn parse_batch(workers: usize) -> usize {
    4.max(workers * 2)
}

/// Maximum cumulative `(`/`[`/`{` nesting depth of `src` (byte scan,
/// early-exits once past `limit`). Closers below zero saturate at zero so a
/// pathological closer-run can't mask a later opener-run.
fn max_bracket_nesting(src: &str, limit: usize) -> usize {
    let mut depth: usize = 0;
    let mut max = 0;
    for b in src.bytes() {
        match b {
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    max = depth;
                    if max > limit {
                        return max;
                    }
                }
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Progress phase (TS also had `'resolving'`, which extraction itself never
/// emits — map §Rust port notes — so it is not modeled here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    Parsing,
    Storing,
}

/// One progress tick, delivered to `index_all`'s callback.
#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub phase: Phase,
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
}

/// The outcome of an `index_all`/`index_files` run. Counts are disjoint:
/// a file is indexed XOR skipped (unchanged hash / oversized) XOR errored
/// (read failure, or extraction with severity-Error errors — still
/// committed with its errors when extractable).
#[derive(Debug, Clone, Default)]
pub struct IndexResult {
    pub success: bool,
    pub files_indexed: u32,
    pub files_skipped: u32,
    pub files_errored: u32,
    pub files_discovered: u32,
    pub nodes_created: u64,
    pub edges_created: u64,
    pub errors: Vec<ExtractionError>,
    /// Advisory notes ("re-index recommended", …) — never errors. An
    /// addition to the brief's pinned field list: the version-mismatch note
    /// must not masquerade as an [`ExtractionError`] (its `ErrorCode` set is
    /// a persisted wire contract with no fitting variant).
    pub notes: Vec<String>,
    pub duration_ms: u64,
}

/// Progress callback (sync — invoked inline between pipeline steps).
pub type ProgressFn<'a> = &'a (dyn Fn(&IndexProgress) + Send + Sync);

/// The dedicated parse pool: [`parse_workers`] threads with explicit
/// [`PARSE_STACK_SIZE`] stacks and named threads (module docs).
fn build_pool(workers: usize) -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .stack_size(PARSE_STACK_SIZE)
        .thread_name(|i| format!("selene-parse-{i}"))
        .build()
}

/// The indexer: a project root + a store. Generic over [`GraphStore`] — the
/// brief's concrete-store note is resolved: Phase 1 Task 10 DID lift
/// `replace_file_extraction` onto the trait, so the orchestrator codes
/// against the contract (tests still drive the in-memory `SurrealStore`).
pub struct Indexer<S: GraphStore> {
    root: PathBuf,
    store: S,
    /// The parse pool, built once on first use. The `Result` is the point: an
    /// OS refusal to spawn the pool threads stays a **collected** error
    /// (errors-collected-never-thrown, the crate's global constraint) instead
    /// of a construction-time panic. [`Indexer::try_new`] surfaces the same
    /// failure eagerly for callers that want it up front.
    pool: OnceLock<Result<Arc<rayon::ThreadPool>, String>>,
    workers: usize,
}

/// One file's read outcome, carried from the read step into parse + commit.
struct ParseInput {
    rel: String,
    content: String,
    language: Language,
    size: u64,
    modified_at: i64,
}

impl<S: GraphStore> Indexer<S> {
    /// Build an indexer over `root` and `store`. The dedicated rayon parse
    /// pool ([`parse_workers`] threads, [`PARSE_STACK_SIZE`] stacks — module
    /// docs) is built lazily, on the first indexing call.
    ///
    /// Never panics: if the OS refuses to spawn the pool threads, the failure
    /// is collected into [`IndexResult::errors`] as a `parser_error`
    /// (infrastructure, not file content) and the run reports
    /// `success: false` — the same errors-collected discipline every other
    /// failure in this crate follows. Use [`Indexer::try_new`] to build the
    /// pool eagerly and handle that failure at construction instead.
    pub fn new(root: PathBuf, store: S) -> Self {
        Indexer {
            root,
            store,
            pool: OnceLock::new(),
            workers: parse_workers(),
        }
    }

    /// [`Indexer::new`] with the parse pool built up front: `Err` iff the OS
    /// refuses to spawn the pool threads (the only failure mode of pool
    /// construction). Prefer this at a process boundary that wants to fail
    /// fast; `new` defers the same error into the run's `errors`.
    pub fn try_new(root: PathBuf, store: S) -> Result<Self, rayon::ThreadPoolBuildError> {
        let workers = parse_workers();
        let pool = Arc::new(build_pool(workers)?);
        let cell = OnceLock::new();
        let _ = cell.set(Ok(pool)); // fresh cell: `set` cannot fail
        Ok(Indexer {
            root,
            store,
            pool: cell,
            workers,
        })
    }

    /// The parse pool, built on first call and memoized (failure included, so
    /// a doomed pool is not re-attempted per batch).
    fn pool(&self) -> Result<Arc<rayon::ThreadPool>, String> {
        self.pool
            .get_or_init(|| {
                build_pool(self.workers)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            })
            .clone()
    }

    /// The wrapped store (tests and later pipeline phases query through it).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Full-project index (map §2 minus the WASM machinery — module docs):
    /// scan → batched read+parse on the rayon pool → ordered sequential
    /// commit, all inside deferred-FTS bulk mode. Never returns `Err`:
    /// problems are collected into `errors` and `success` reflects them.
    pub async fn index_all(&self, on_progress: Option<ProgressFn<'_>>) -> IndexResult {
        let started = Instant::now();
        let mut result = IndexResult::default();

        // Scan FIRST — outside the bulk-load bracket (module docs: the bracket
        // is exit-safe). The scan touches no store state, and a scan failure is
        // the one early return this function has; hoisting it above
        // `bulk_load_begin` makes "return with the FTS indexes dropped"
        // structurally impossible instead of merely remembered.
        emit(on_progress, Phase::Scanning, 0, 0, None);
        let mut files = match scan_directory(&self.root, &ScanOverrides::default()) {
            Ok(files) => files,
            Err(e) => {
                result.errors.push(ExtractionError {
                    message: format!("scan failed: {e}"),
                    severity: Severity::Error,
                    code: ErrorCode::ReadError,
                    file_path: None,
                });
                result.duration_ms = started.elapsed().as_millis() as u64;
                return result;
            }
        };
        // scan_directory already returns sorted paths; sort defensively —
        // commit order IS the determinism contract (#1015, module docs).
        files.sort_unstable();

        // Initializing entry point: applies the schema on a fresh store and
        // defers FTS maintenance for the whole run (module docs).
        if let Err(e) = self.store.bulk_load_begin().await {
            push_store_error(&mut result.errors, "bulk_load_begin", &e);
            // `begin` is schema-apply + a 4-statement index drop: a failure can
            // land with some indexes already gone. Restore them best-effort
            // rather than walk away from a half-dropped store (on a store so
            // broken that the schema never applied, `finish` fails too — a
            // second collected error on an already-failed run).
            if let Err(e) = self.store.bulk_load_finish().await {
                push_store_error(&mut result.errors, "bulk_load_finish", &e);
            }
            result.duration_ms = started.elapsed().as_millis() as u64;
            return result;
        }

        // Stored-version pre-check: stored < engine ⇒ advisory note, never
        // an error.
        match self.store.get_meta(EXTRACTION_VERSION_META_KEY).await {
            Ok(Some(stored)) => {
                if stored
                    .trim()
                    .parse::<u32>()
                    .is_ok_and(|v| v < EXTRACTION_VERSION)
                {
                    result.notes.push(format!(
                        "re-index recommended: stored extraction version {stored} is older \
                         than engine version {EXTRACTION_VERSION}"
                    ));
                }
            }
            Ok(None) => {}
            Err(e) => push_store_error(&mut result.errors, "get_meta", &e),
        }

        self.run_pipeline(&files, on_progress, &mut result).await;

        if let Err(e) = self.store.bulk_load_finish().await {
            push_store_error(&mut result.errors, "bulk_load_finish", &e);
        }

        result.success = result.files_indexed > 0
            || !result.errors.iter().any(|e| e.severity == Severity::Error);
        if result.success
            && let Err(e) = self
                .store
                .set_meta(EXTRACTION_VERSION_META_KEY, &EXTRACTION_VERSION.to_string())
                .await
        {
            push_store_error(&mut result.errors, "set_meta", &e);
        }

        result.duration_ms = started.elapsed().as_millis() as u64;
        result
    }

    /// Index an explicit set of root-relative paths through the same
    /// batched-parse + ordered-commit pipeline (no scan, no bulk mode, no
    /// version bookkeeping — the incremental multi-file entry point).
    pub async fn index_files(&self, rel_paths: &[String]) -> IndexResult {
        let started = Instant::now();
        let mut result = IndexResult::default();
        let mut files: Vec<String> = rel_paths.to_vec();
        files.sort_unstable();

        self.run_pipeline(&files, None, &mut result).await;

        result.success = result.files_indexed > 0
            || !result.errors.iter().any(|e| e.severity == Severity::Error);
        result.duration_ms = started.elapsed().as_millis() as u64;
        result
    }

    /// Single-file incremental re-index (map §4 essence): read + stat →
    /// content-hash pre-check against the stored record (same hash ⇒ no-op,
    /// an empty result) → extract → `replace_file_extraction`. File-level
    /// problems are collected INTO the returned result; `Err` is reserved
    /// for store malfunctions.
    pub async fn index_file(&self, rel_path: &str) -> Result<ExtractionResult, selene_db::Error> {
        let mut out = ExtractionResult::default();
        if is_path_traversal(rel_path) {
            out.errors.push(ExtractionError {
                message: format!("path escapes the project root: {rel_path}"),
                severity: Severity::Error,
                code: ErrorCode::PathTraversal,
                file_path: Some(rel_path.to_string()),
            });
            return Ok(out);
        }

        let Some(input) = read_input(&self.root, rel_path, &mut out.errors) else {
            return Ok(out);
        };

        // Hash pre-check BEFORE extraction (the brief's §4 ordering).
        let content_hash = hash_content(&input.content);
        if let Some(existing) = self.store.get_file(rel_path).await?
            && existing.content_hash == content_hash
        {
            return Ok(out); // unchanged — no-op
        }

        let extraction = guarded_extract(&input);
        let stats = self.commit(&input, &extraction, content_hash).await?;
        let _ = stats; // single-file callers read counts off the store
        Ok(extraction)
    }

    /// The shared read → parse → ordered-commit pipeline over `files`
    /// (already sorted). Mutates `result` counters/errors in place.
    async fn run_pipeline(
        &self,
        files: &[String],
        on_progress: Option<ProgressFn<'_>>,
        result: &mut IndexResult,
    ) {
        result.files_discovered = u32::try_from(files.len()).unwrap_or(u32::MAX);
        let total = files.len();
        let batch_size = parse_batch(self.workers);
        let mut processed = 0usize;

        // The pool is built here, not in the constructor (see [`Indexer::new`]):
        // an OS refusal to spawn threads is a collected `parser_error`, not a
        // panic. Nothing can be parsed without it — the run ends here.
        let pool = match self.pool() {
            Ok(pool) => pool,
            Err(msg) => {
                result.errors.push(ExtractionError {
                    message: format!("failed to build the parse thread pool: {msg}"),
                    severity: Severity::Error,
                    code: ErrorCode::ParserError,
                    file_path: None,
                });
                return;
            }
        };

        for batch in files.chunks(batch_size) {
            // Read step (sync std::fs — FILE_IO_BATCH_SIZE folds into the
            // parse batch, module docs). Each successfully-read file carries
            // its 1-based ordinal in scan order (`processed`) through parse
            // into commit: it is what the Storing phase reports as `current`,
            // so progress stays monotonic and correct when a batch had reads
            // that failed or were skipped (a batch-relative offset would drift
            // by the number of missing inputs).
            let mut inputs: Vec<(usize, ParseInput)> = Vec::with_capacity(batch.len());
            for rel in batch {
                processed += 1;
                emit(
                    on_progress,
                    Phase::Parsing,
                    processed,
                    total,
                    Some(rel.clone()),
                );
                if is_path_traversal(rel) {
                    result.errors.push(ExtractionError {
                        message: format!("path escapes the project root: {rel}"),
                        severity: Severity::Error,
                        code: ErrorCode::PathTraversal,
                        file_path: Some(rel.clone()),
                    });
                    result.files_errored += 1;
                    continue;
                }
                let mut errors = Vec::new();
                match read_input(&self.root, rel, &mut errors) {
                    Some(input) => inputs.push((processed, input)),
                    None => {
                        // size_exceeded is a skip (warning); read failures err.
                        if errors.iter().any(|e| e.code == ErrorCode::SizeExceeded) {
                            result.files_skipped += 1;
                        } else {
                            result.files_errored += 1;
                        }
                    }
                }
                result.errors.append(&mut errors);
            }

            // Parse step: the whole batch fans out on the dedicated pool;
            // `collect` preserves input order.
            let pool = Arc::clone(&pool);
            let extracted: Vec<(usize, ParseInput, ExtractionResult)> =
                match tokio::task::spawn_blocking(move || {
                    pool.install(|| {
                        inputs
                            .into_par_iter()
                            .map(|(ordinal, input)| {
                                let r = guarded_extract(&input);
                                (ordinal, input, r)
                            })
                            .collect()
                    })
                })
                .await
                {
                    Ok(v) => v,
                    Err(join_err) => {
                        result.errors.push(ExtractionError {
                            message: format!("parse batch panicked: {join_err}"),
                            severity: Severity::Error,
                            code: ErrorCode::ParserError,
                            file_path: None,
                        });
                        continue;
                    }
                };

            // Commit step: strictly sequential, in scan order (#1015).
            for (ordinal, input, extraction) in &extracted {
                emit(
                    on_progress,
                    Phase::Storing,
                    *ordinal,
                    total,
                    Some(input.rel.clone()),
                );

                let content_hash = hash_content(&input.content);
                match self.store.get_file(&input.rel).await {
                    Ok(Some(existing)) if existing.content_hash == content_hash => {
                        result.files_skipped += 1;
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        push_store_error(&mut result.errors, "get_file", &e);
                        result.files_errored += 1;
                        continue;
                    }
                }

                match self.commit(input, extraction, content_hash).await {
                    Ok(stats) => {
                        result.nodes_created += stats.nodes_inserted;
                        result.edges_created += stats.edges_inserted;
                        let has_error = extraction
                            .errors
                            .iter()
                            .any(|e| e.severity == Severity::Error);
                        if has_error {
                            result.files_errored += 1;
                        } else {
                            result.files_indexed += 1;
                        }
                    }
                    Err(e) => {
                        push_store_error(&mut result.errors, "replace_file_extraction", &e);
                        result.files_errored += 1;
                    }
                }
                result.errors.extend(extraction.errors.iter().cloned());
            }
        }
    }

    /// Commit one file's extraction through the single-file re-index
    /// protocol, converting the extraction shapes into the DB rows at the
    /// seam (brief: FileRecord fields + UnresolvedReference →
    /// `selene_db::UnresolvedRef`).
    async fn commit(
        &self,
        input: &ParseInput,
        extraction: &ExtractionResult,
        content_hash: String,
    ) -> Result<ReplaceStats, selene_db::Error> {
        let file_record = FileRecord {
            path: input.rel.clone(),
            content_hash,
            language: input.language.as_str().to_string(),
            size: input.size,
            modified_at: input.modified_at,
            indexed_at: now_millis(),
            node_count: u32::try_from(extraction.nodes.len()).unwrap_or(u32::MAX),
            errors: extraction
                .errors
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect(),
        };
        let unresolved: Vec<UnresolvedRef> = extraction
            .unresolved
            .iter()
            .map(|u| to_db_ref(u, &input.rel, input.language.as_str()))
            .collect();
        self.store
            .replace_file_extraction(
                &input.rel,
                &extraction.nodes,
                &extraction.edges,
                &unresolved,
                &file_record,
            )
            .await
    }
}

/// Read + stat one file. `None` (with a pushed error) on oversize
/// (`size_exceeded` warning — counted a skip by the caller) or a read
/// failure (`read_error`, severity Error).
fn read_input(root: &Path, rel: &str, errors: &mut Vec<ExtractionError>) -> Option<ParseInput> {
    let abs = root.join(rel);
    let meta = match std::fs::metadata(&abs) {
        Ok(m) => m,
        Err(e) => {
            errors.push(ExtractionError {
                message: format!("failed to stat {rel}: {e}"),
                severity: Severity::Error,
                code: ErrorCode::ReadError,
                file_path: Some(rel.to_string()),
            });
            return None;
        }
    };
    if meta.len() > MAX_FILE_SIZE {
        errors.push(ExtractionError {
            message: format!(
                "file exceeds MAX_FILE_SIZE ({} > {MAX_FILE_SIZE} bytes) — skipped",
                meta.len()
            ),
            severity: Severity::Warning,
            code: ErrorCode::SizeExceeded,
            file_path: Some(rel.to_string()),
        });
        return None;
    }
    let content = match std::fs::read_to_string(&abs) {
        Ok(c) => c,
        Err(e) => {
            errors.push(ExtractionError {
                message: format!("failed to read {rel}: {e}"),
                severity: Severity::Error,
                code: ErrorCode::ReadError,
                file_path: Some(rel.to_string()),
            });
            return None;
        }
    };
    // Millisecond-floored mtime (the map's `Math.floor(stat.mtimeMs)` rule —
    // Phase 6 sync compares this value, so truncation must match).
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let language = detect_language(rel, Some(&content));
    Some(ParseInput {
        rel: rel.to_string(),
        content,
        language,
        size: meta.len(),
        modified_at,
    })
}

/// Extraction behind the nesting guard (module docs): adversarial nesting
/// becomes a collected `parse_error` instead of a stack overflow; everything
/// else runs `extract_from_source` (sync, framework names not yet threaded —
/// Phase 3 owns that seam).
fn guarded_extract(input: &ParseInput) -> ExtractionResult {
    let nesting = max_bracket_nesting(&input.content, MAX_NESTING_DEPTH);
    if nesting > MAX_NESTING_DEPTH {
        let mut r = ExtractionResult::default();
        r.errors.push(ExtractionError {
            message: format!(
                "bracket nesting depth {nesting} exceeds MAX_NESTING_DEPTH \
                 ({MAX_NESTING_DEPTH}) — refusing to parse (walker stack-overflow guard)"
            ),
            severity: Severity::Error,
            code: ErrorCode::ParseError,
            file_path: Some(input.rel.clone()),
        });
        return r;
    }
    extract_from_source(&input.rel, &input.content, input.language)
}

/// `UnresolvedReference` → `selene_db::UnresolvedRef` at the seam:
/// denormalized `file_path`/`language` (a block-delegating extractor's own
/// values win when set), `status: Pending`, empty candidates, `name_tail` =
/// the last `.`/`::` segment of the reference name.
fn to_db_ref(u: &UnresolvedReference, file_path: &str, language: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: u.from_node_id.clone(),
        reference_name: u.reference_name.clone(),
        reference_kind: u.reference_kind.clone(),
        line: u.line,
        column: u.column,
        candidates: Vec::new(),
        file_path: u.file_path.clone().unwrap_or_else(|| file_path.to_string()),
        language: u.language.clone().unwrap_or_else(|| language.to_string()),
        status: RefStatus::Pending,
        name_tail: name_tail(&u.reference_name),
    }
}

/// Last `.`/`::` segment of a (possibly qualified) reference name.
fn name_tail(reference_name: &str) -> String {
    let after_colons = reference_name.rsplit("::").next().unwrap_or(reference_name);
    after_colons
        .rsplit('.')
        .next()
        .unwrap_or(after_colons)
        .to_string()
}

/// A root-relative path that climbs out of the root (`..` component).
fn is_path_traversal(rel: &str) -> bool {
    Path::new(rel)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Store malfunctions are collected (never panicked/thrown) — the closest
/// wire code is `parser_error` (infrastructure, not file content); the
/// message prefix names the failing operation.
fn push_store_error(errors: &mut Vec<ExtractionError>, op: &str, e: &selene_db::Error) {
    errors.push(ExtractionError {
        message: format!("store error in {op}: {e}"),
        severity: Severity::Error,
        code: ErrorCode::ParserError,
        file_path: None,
    });
}

fn emit(
    on_progress: Option<ProgressFn<'_>>,
    phase: Phase,
    current: usize,
    total: usize,
    current_file: Option<String>,
) {
    if let Some(f) = on_progress {
        f(&IndexProgress {
            phase,
            current,
            total,
            current_file,
        });
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parse_batch_floor_is_four() {
        assert_eq!(parse_batch(1), 4);
        assert_eq!(parse_batch(2), 4);
        assert_eq!(parse_batch(3), 6);
        assert_eq!(parse_batch(8), 16);
    }

    #[test]
    fn bracket_nesting_scan() {
        assert_eq!(max_bracket_nesting("", 10), 0);
        assert_eq!(max_bracket_nesting("a(b[c{d}e]f)g", 10), 3);
        // Unbalanced closers can't mask a later opener run.
        assert_eq!(max_bracket_nesting(")))((( ", 10), 3);
        // Early exit past the limit still reports a value over the limit.
        let deep = "(".repeat(64);
        assert!(max_bracket_nesting(&deep, 10) > 10);
    }

    #[test]
    fn name_tail_takes_last_segment_of_either_separator() {
        assert_eq!(name_tail("bare"), "bare");
        assert_eq!(name_tail("MathHelper.calculateTotal"), "calculateTotal");
        assert_eq!(name_tail("crate::m::Widget"), "Widget");
        assert_eq!(name_tail("a::b.c"), "c");
    }

    #[test]
    fn path_traversal_detection() {
        assert!(is_path_traversal("../etc/passwd"));
        assert!(is_path_traversal("src/../../x"));
        assert!(!is_path_traversal("src/ok.rs"));
    }
}
