//! Import resolution: the *inputs* (Task 4 — this module's four submodules),
//! the module-specifier → file walk (Task 5), and `resolve_via_import`
//! (Task 6).
//!
//! # ⚠ A shared seam
//!
//! Task 5 creates `resolve_import_path` here and Task 6 appends
//! `resolve_via_import` — **strictly sequential**. The four input modules below
//! (`mappings`, `aliases`, `workspace`, `go_module`) are independent of each
//! other and of 5/6.

pub mod aliases;
pub mod go_module;
pub mod mappings;
pub mod workspace;

// Task 5: `resolve_import_path`, `is_external_import`, `EXTENSION_RESOLUTION`.
// Task 6: `resolve_via_import`, `resolve_jvm_import`, `find_exported_symbol`.
