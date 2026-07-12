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

mod error;
mod generated;
mod language;
mod types;

pub use error::{ErrorCode, ExtractionError, Severity};
pub use generated::is_generated_file;
pub use language::{Language, detect_language, is_file_level_only, is_source_file};
pub use types::{ExtractionResult, MAX_FILE_SIZE, UnresolvedReference};
