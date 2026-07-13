//! `selene-extract` — Tree-sitter extraction (native) + standalone
//! extractors; Rayon parallelism. Target design:
//! `docs/specs/2026-07-11-rust-graph-db-migration-design.md` (PRD §3, §6);
//! build plan: `docs/plans/2026-07-12-phase2-selene-extract.md`.
//!
//! Extraction is **deterministic** (AST-derived, never LLM-summarized) and
//! **sync** — no async runtime in this crate; the orchestrator bridges to
//! tokio via `spawn_blocking` at the surface layer. Errors are collected
//! into results, never thrown (see [`ExtractionError`]).
//!
//! Currently implemented (Phase 2): contract types ([`ExtractionResult`],
//! [`ExtractionError`], [`UnresolvedReference`]), the full language
//! registry + detection ([`detect_language`], [`is_source_file`] — the
//! indexability predicate), and the generated-file classifier
//! ([`is_generated_file`]). Grammar registry, walker, rules, scan, and
//! orchestrator land in Tasks 4–18.
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
//! As of the Phase 2 gate the ledger holds exactly two divergences, both cases of
//! **"silent beats wrong"** where TS is the one that is wrong:
//!
//! 1. **C++ phantom base classes.** TS's Go-struct-embedding arm is not
//!    language-gated, and C++ spells member declarations `field_declaration` too —
//!    so TS reads a member's type (a return type, or a pointer field's type) as an
//!    inherited base and emits `extends` refs from classes that have no base clause
//!    at all. We gate that arm to Go and emit nothing.
//! 2. **C# record base names.** For `record D(int A) : Base(A)`, TS emits the raw
//!    `primary_constructor_base_type` text — the literal `Base(A)`, argument list
//!    included — which no symbol carries and no resolver can match. We unwrap to
//!    the type head (`Base`). The counts agree, so only the NAME gate sees this.
//!
//! Both are documented in full, with the TS line numbers, in `deviations.toml`.

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
