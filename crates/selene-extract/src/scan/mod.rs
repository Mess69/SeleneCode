//! Scan pipeline (Phase 2 Tasks 16–17): which files are in scope.
//!
//! [`ignore`] (Task 16) carries the scope-ignore semantics — the built-in
//! default ignore dirs, the defensive `.gitignore` reader, and [`ScopeIgnore`]
//! (`crate::ScopeIgnore`), the single source of truth for indexer and watcher
//! scope. The `scan_directory` git fast path / embedded-repo recursion / FS
//! fallback (Task 17) lands here next — this module is its home.

pub(crate) mod ignore;
