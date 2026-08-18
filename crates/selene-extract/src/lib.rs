//! `selene-extract` — tree-sitter extraction over natively-linked grammars,
//! with rayon parallelism. Target design:
//! `docs/specs/2026-07-11-rust-graph-db-migration-design.md` (PRD §3, §6);
//! build plan: `docs/plans/2026-07-12-phase2-selene-extract.md`; the TS
//! parity source is mapped in
//! `docs/reference/from-codegraph/maps/extraction-core.md`.
//!
//! Extraction is **deterministic** (AST-derived, never LLM-summarized) and
//! **sync** — no async runtime in the extraction path; [`Indexer`] bridges
//! to tokio via `spawn_blocking` at the DB seam only. Errors are
//! **collected into results, never thrown** (see [`ExtractionError`]): a
//! file that fails still contributes whatever it parsed, and a malfunction
//! degrades one file rather than the run.
//!
//! # The pipeline
//!
//! [`scan_directory`] (git fast path, FS fallback, [`ScopeIgnore`]) →
//! [`Indexer::index_all`] (rayon fan-out, **ordered** commit) →
//! [`extract_from_source`] per file → [`ExtractionResult`] (nodes, edges,
//! [`UnresolvedReference`]s) → `selene-db`. Node ids and commit order are
//! part of the determinism contract: two runs over one tree yield identical
//! ids *and* identical stats.
//!
//! # Extraction emits no cross-file edges
//!
//! Every [`selene_core::Edge`] a walk produces joins two ids **within the
//! file being walked**. Anything reaching beyond the file leaves as an
//! [`UnresolvedReference`] for Phase 3 (`selene-resolve`) to bind. The
//! internal `function_ref` reference kind is part of that channel and is
//! deliberately **not** an [`selene_core::EdgeKind`] — see the `fnref`
//! module docs and `selene_core`'s `UnresolvedReference::reference_kind`.
//!
//! # The WASM layer is deleted, not ported
//!
//! CodeGraph's TS build ran grammars as WASM under web-tree-sitter, and a
//! large fraction of its extraction core existed to contain that: worker
//! processes to survive heap corruption, parser resets for heap growth,
//! OOM-retry passes, V8 flags, async grammar loading. Native grammars are
//! statically linked and don't corrupt heaps, so **every one of those
//! mechanisms is dropped rather than reimplemented**. The seven dropped
//! mechanisms are each named, with its reason, in the `orchestrator` module
//! docs ("Deliberately dropped"): `ParseWorkerPool` (+ its recycle /
//! crash-budget / spawn-cap policy), `PARSER_RESET_INTERVAL`, the WASM-OOM
//! retry + comment-blank passes, `wasm-runtime-flags`, the grammar-bytes
//! pre-read + sequential grammar loading (and with it the "needed
//! languages" pre-pass, whose only consumer that loading was),
//! `FILE_IO_BATCH_SIZE` (folded into `PARSE_BATCH` — without worker threads
//! a separate read-batch size buys nothing), and the per-parse timeout (the
//! [`MAX_NESTING_DEPTH`] guard covers the pathological class instead).
//!
//! # Public-interface ledger (extraction-core.md §Public interface)
//!
//! Every item in the map's public interface is either ported or explicitly
//! deferred here:
//!
//! | TS | Rust |
//! |---|---|
//! | `indexAll` / `indexFiles` / `indexFile` | [`Indexer::index_all`] / [`Indexer::index_files`] / [`Indexer::index_file`] |
//! | `IndexProgress` / `IndexResult` | [`IndexProgress`] / [`IndexResult`] |
//! | `extractFromSource` | [`extract_from_source`] (the `frameworkNames` arg is threaded internally as an empty list — framework extractors are Phase 3) |
//! | `scanDirectory` (+ `Async`) | [`scan_directory`] (sync crate; no async variant) |
//! | `ScopeIgnore` / `buildScopeIgnore` / `buildDefaultIgnore` | [`ScopeIgnore`] + [`ScopeOverrides`]; the builders are internal (reached via [`scan_directory`]) |
//! | `discoverEmbeddedRepoRoots` / `findUnindexedIgnoredRepos` | [`discover_embedded_repo_roots`] / [`find_unindexed_ignored_repos`] |
//! | `detectLanguage` / `isSourceFile` / `isFileLevelOnlyLanguage` | [`detect_language`] / [`is_source_file`] / [`is_file_level_only`] |
//! | `generateNodeId` / `hashContent` | `selene_core::node_id` / `selene_core::hash_content` (shared types live in core) |
//! | `getNodeText` / `getChildByField` / `getPrecedingDocstring` | [`get_node_text`] / [`get_child_by_field`] / [`get_preceding_docstring`] |
//! | `EXTENSION_MAP` | internal to `language` ([`Language`] is the wire enum) |
//! | `indexFileWithContent` | **not ported** — an in-memory-content entry point whose consumer is a file watcher; revisit with `selene-sync` (Phase 6) |
//! | `sync` / `getChangedFiles` / `SyncResult` | **Phase 6** (`selene-sync`) |
//! | `isLanguageSupported` / `isGrammarLoaded` / `initGrammars` / `loadGrammarsForLanguages` / `readGrammarWasmBytes` / `getParser` / `resetParser` / `getUnavailableGrammarErrors` | **obsolete** — grammars are statically linked, so "loaded" and "unavailable" have no runtime meaning; support is the [`Language`] enum itself |
//! | `ParseWorkerPool` / `resolveParsePoolSize` | **deleted with the WASM layer** (rayon pool; `SELENE_PARSE_WORKERS` keeps the sizing knob) |
//!
//! # Deferred beyond the WASM deletion
//!
//! - **Standalone extractors and the wave-2 languages** → Phase 8. A wave-2
//!   language still *detects* (see `language`), but extraction returns an
//!   `unsupported_language` **warning**, never an error.
//! - **Force-include *collection*** (discovering gitignored `include`-matched
//!   files off disk, which `git ls-files` never lists) → Phase 8 with the
//!   config loader; until then `include` only affects paths the enumerator
//!   already visits (`scan` module docs).
//! - **[`ScopeIgnore`] config-file loading** (the `.selene` project config) →
//!   Phase 8; overrides arrive as a plain [`ScopeOverrides`] value today.
//! - **Function-ref *resolution*** (unique-or-drop, class-scoped `this.X`,
//!   overload refusal) → Phase 3. This crate only **captures** candidates;
//!   see the `fnref` module docs.
//! - **Framework detection / `fw.extract` append** → Phase 3 (the seam is
//!   threaded through the orchestrator as an empty framework-name list).
//!
//! # Parity with the CodeGraph TS extractor
//!
//! Extraction is held to **count- AND name-parity** with the reference
//! implementation by `tests/parity_gate.rs`, at **tolerance 0**, over a shared
//! corpus of byte-identical fixtures (`tests/fixtures/parity/`). Both halves
//! matter: the count gate cannot see a divergence that keeps the count and
//! changes the identity (`extends:Base` → `extends:Base(A)`), which is exactly
//! what a port under count-pressure produces.
//!
//! ## Deviation ledger — `tests/fixtures/parity/deviations.toml` is the authority
//!
//! Every intentional divergence from TS lives **there**, one entry each, with the
//! observed counts/names and a cited reason. It is machine-checked: an entry that
//! matches no observed difference FAILS the gate as stale, so a fixed divergence
//! cannot leave a whitelist that silently re-permits a regression. Do not record
//! deviations anywhere else — a second list would drift out of sync with the one
//! the gate enforces.
//!
//! As of the Phase 2 gate the ledger holds **three divergences** — every one a case
//! of **"silent beats wrong"**, where TS emits a reference that cannot resolve to
//! anything and we deliberately emit nothing:
//!
//! 1. **C++ phantom base classes.** TS's Go-struct-embedding arm is not
//!    language-gated, and C++ spells member declarations `field_declaration` too —
//!    so TS reads a member's type (a return type, or a pointer field's type) as an
//!    inherited base and emits `extends` refs from classes that have no base clause
//!    at all. We gate that arm to Go.
//! 2. **C# enum storage types.** For `enum Status : byte`, TS emits `extends:byte`
//!    — asserting the enum *inherits from* `byte`. C# enums cannot inherit; `: byte`
//!    picks the storage width, and `byte` is a keyword with no definition node.
//!    [`walker`]'s enum path does not call the inheritance pass.
//! 3. **C# record base names.** For `record D(int A) : Base(A)`, TS emits the raw
//!    `primary_constructor_base_type` text — the literal `Base(A)`, argument list
//!    included — which no symbol carries and no resolver can match. We unwrap to
//!    the type head (`Base`). The counts agree, so only the NAME half sees this.
//!
//! …plus one **grammar drift** (`[[grammar-drift]]`), which is the opposite of a
//! deviation: Kotlin's `tree-sitter-kotlin-ng` shapes differ from the grammar TS
//! ran, the walker compensates, and the output is *identical*. It is recorded so
//! that the compensation is explained — and machine-checked, by asserting the
//! fixture stays at exact count AND name parity.
//!
//! All four are documented in full, with TS line numbers and fixtures, in
//! `deviations.toml`. Each is gated: a fixture exercises it, and a stale entry
//! fails the build.
//!
//! # Versioning
//!
//! `selene_core::EXTRACTION_VERSION` (currently `1`) is bumped by any change
//! to what extraction emits — node/edge emission, id inputs, docstring
//! cleanup, qualified-name spelling. [`Indexer::index_all`] persists it and,
//! on finding an older stored version, returns "re-index recommended"
//! **guidance — never a hard error** (PRD §8.2: `isError` is reserved).

mod docparse;
mod error;
mod fnref;
mod generated;
mod grammars;
mod helpers;
mod language;
mod orchestrator;
mod rules;
mod scan;
mod types;
mod walker;

pub use error::{ErrorCode, ExtractionError, Severity};
pub use generated::is_generated_file;
pub use helpers::{
    clean_comment_markers, get_child_by_field, get_node_text, get_preceding_docstring,
};
pub use language::{Language, detect_language, is_file_level_only, is_source_file};
pub use orchestrator::{IndexProgress, IndexResult, Indexer, MAX_NESTING_DEPTH, Phase, ProgressFn};
pub use rules::{
    ClassKind, ImportInfo, LanguageRules, MethodClass, NodeTypeTables, VariableInfo, rules_for,
};
pub use scan::ignore::{ScopeIgnore, ScopeOverrides};
pub use scan::{
    ScanOverrides, discover_embedded_repo_roots, find_unindexed_ignored_repos, scan_directory,
};
pub use types::{ExtractionResult, MAX_FILE_SIZE, UnresolvedReference};
pub use walker::{NodeExtra, Session, extract_from_source};
