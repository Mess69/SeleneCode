//! The module-specifier → file walk ([`resolve_import_path`], Task 5).

use selene_core::Language;

use crate::context::ResolutionContext;
use crate::imports::aliases::apply_aliases;
use crate::imports::extensions::extensions_for;
use crate::imports::external::{FALLBACK_ALIASES, is_external_import};
use crate::imports::path_helpers::{join_rel, parent_dir};
use crate::imports::workspace::resolve_workspace_import;

// =============================================================================
// resolve_import_path
// =============================================================================

/// A module specifier → the repo-relative file it names, or `None`.
///
/// `None` is an ordinary outcome: the specifier is external, or it names a file
/// this repo does not have. Task 6's `resolve_via_import` then leaves the
/// reference unresolved rather than guessing at a same-named symbol.
pub fn resolve_import_path<C: ResolutionContext>(
    import_path: &str,
    from_file: &str,
    language: Language,
    ctx: &C,
) -> Option<String> {
    // Wave 2 (Phase 8): COBOL copybooks resolve FIRST, ahead of the external
    // check — `COPY CVACT01Y` names a library member, and the bare-specifier
    // heuristic would misread it as a third-party package.

    if is_external_import(import_path, language, ctx) {
        return None;
    }

    if import_path.starts_with('.') {
        return resolve_relative_import(import_path, from_file, language, ctx);
    }

    if let Some(hit) = resolve_aliased_import(import_path, language, ctx) {
        return Some(hit);
    }

    // C/C++ last resort: the `-I` search path.
    if matches!(language, Language::C | Language::Cpp) {
        return resolve_cpp_include_path(import_path, language, ctx);
    }

    None
}

/// `./foo`, `../lib/bar` — and Python's dotted-relative form.
fn resolve_relative_import<C: ResolutionContext>(
    import_path: &str,
    from_file: &str,
    language: Language,
    ctx: &C,
) -> Option<String> {
    let from_dir = parent_dir(from_file);

    // Python's leading dots are PACKAGE LEVELS, not directory names: one dot is
    // the current package, two is the parent. `from ..pkg.mod import x` means
    // `../pkg/mod`, and treating `.pkg` as a literal hidden filename (which a
    // plain path join does) resolves nothing at all.
    if language == Language::Python {
        let dots = import_path.chars().take_while(|c| *c == '.').count();
        let up = "../".repeat(dots.saturating_sub(1)); // 1 dot = the current dir
        let rest = import_path[dots..].replace('.', "/"); // `sub.mod` → `sub/mod`
        let base = join_rel(&from_dir, &format!("{up}{rest}"));
        return try_extensions(&base, language, ctx);
    }

    let base = join_rel(&from_dir, import_path);
    try_extensions(&base, language, ctx)
}

/// A bare specifier: a tsconfig alias, then a workspace member, then a
/// conventional prefix, then a plain root-relative path — **in that order**.
fn resolve_aliased_import<C: ResolutionContext>(
    import_path: &str,
    language: Language,
    ctx: &C,
) -> Option<String> {
    // 1. The project's own `compilerOptions.paths`.
    if let Some(aliases) = ctx.project_aliases() {
        for candidate in apply_aliases(import_path, aliases, ctx.project_root()) {
            if let Some(hit) = try_extensions(&candidate, language, ctx) {
                return Some(hit);
            }
        }
    }

    // 2. A workspace member (`@scope/ui/widgets` → `packages/ui/widgets`); the
    //    extension/index probing then finds its barrel (#629).
    if let Some(ws) = ctx.workspace_packages()
        && let Some(base) = resolve_workspace_import(import_path, ws)
        && let Some(hit) = try_extensions(&base, language, ctx)
    {
        return Some(hit);
    }

    // 3. The conventional prefixes.
    for (alias, replacement) in FALLBACK_ALIASES {
        if let Some(rest) = import_path.strip_prefix(alias) {
            let rewritten = format!("{replacement}{rest}");
            if let Some(hit) = try_extensions(&rewritten, language, ctx) {
                return Some(hit);
            }
        }
    }

    // 4. A plain root-relative path.
    try_extensions(import_path, language, ctx)
}

/// The `-I` include-directory search — C/C++'s last resort.
fn resolve_cpp_include_path<C: ResolutionContext>(
    import_path: &str,
    language: Language,
    ctx: &C,
) -> Option<String> {
    for dir in ctx.cpp_include_dirs() {
        let dir = dir.replace('\\', "/");
        if let Some(hit) = try_extensions(&format!("{dir}/{import_path}"), language, ctx) {
            return Some(hit);
        }
    }
    None
}

/// Probe `base` against the language's extension list, **in order**, then bare.
fn try_extensions<C: ResolutionContext>(base: &str, language: Language, ctx: &C) -> Option<String> {
    if base.is_empty() {
        return None;
    }
    for ext in extensions_for(language) {
        let candidate = format!("{base}{ext}");
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
    }
    // The specifier may already carry its extension (`./util.js`, `foo/bar.h`).
    if ctx.file_exists(base) {
        return Some(base.to_string());
    }
    None
}
