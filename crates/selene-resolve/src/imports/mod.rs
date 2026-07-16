//! Import resolution: the *inputs* (Task 4 — the four submodules below), the
//! module-specifier → file walk ([`resolve_import_path`], Task 5), and
//! `resolve_via_import` (Task 6).
//!
//! # ⚠ A shared seam
//!
//! Task 5 created [`resolve_import_path`] here and Task 6 appends
//! `resolve_via_import` — **strictly sequential**. The four input modules
//! (`mappings`, `aliases`, `workspace`, `go_module`) are independent of each
//! other and of 5/6.

pub mod aliases;
pub mod cpp_includes;
pub mod go_module;
pub mod mappings;
pub mod workspace;

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use selene_core::{Language, Node, NodeKind, UnresolvedRef};
use std::sync::Arc;

use crate::context::ResolutionContext;
use crate::imports::aliases::apply_aliases;
use crate::imports::workspace::resolve_workspace_import;
use crate::types::{ImportMapping, ReExport, ResolvedBy, ResolvedRef};

// =============================================================================
// EXTENSION_RESOLUTION
// =============================================================================

/// The suffixes appended to a module specifier, **in order**, when probing for
/// the file it names (`maps/resolution.md` §Import resolution). The `/index.*`
/// entries are suffixes too, not a separate mechanism.
///
/// **The order is the contract**: TypeScript prefers `./foo.ts` over
/// `./foo/index.ts`, and swapping them silently re-points every barrel import in
/// a repo that has both.
///
/// ⚠ **Kotlin has no row — deliberately.** Neither does the TS build
/// (`import-resolver.ts:17`), and neither does the map. A Kotlin import is an
/// FQN (`com.example.Foo`) and resolves through `resolve_jvm_import` and the JVM
/// branch of `resolve_via_import` (Task 6), which map the FQN onto a path suffix
/// — not through extension probing. Adding a `.kt` row would create resolutions
/// the TS build does not have, which the Part C parity gate would then have to
/// explain away. (The plan's Task 5 text lists `kotlin: ['.kt']`; the map and the
/// source do not, and **the map wins** — reported to the maintainer.)
fn extensions_for(language: Language) -> &'static [&'static str] {
    match language {
        Language::Typescript => &[
            ".ts",
            ".tsx",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.ts",
            "/index.tsx",
            "/index.js",
        ],
        Language::Tsx => &[
            ".tsx",
            ".ts",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.tsx",
            "/index.ts",
            "/index.js",
        ],
        Language::Javascript => &[".js", ".jsx", ".mjs", ".cjs", "/index.js", "/index.jsx"],
        Language::Jsx => &[".jsx", ".js", "/index.jsx", "/index.js"],
        // ArkTS and the SFC languages are wave 2, but their rows cost nothing
        // and keep this table honest against the source.
        Language::Arkts => &[
            ".ets",
            ".ts",
            ".d.ts",
            ".js",
            "/Index.ets",
            "/index.ets",
            "/index.ts",
            "/index.js",
        ],
        Language::Svelte => &[
            ".ts",
            ".js",
            ".svelte",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.svelte",
        ],
        Language::Vue => &[
            ".ts",
            ".js",
            ".vue",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.vue",
        ],
        Language::Astro => &[
            ".ts",
            ".js",
            ".astro",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.astro",
        ],
        Language::Python => &[".py", "/__init__.py"],
        Language::Go => &[".go"],
        Language::Rust => &[".rs", "/mod.rs"],
        Language::Java => &[".java"],
        Language::C => &[".h", ".c"],
        Language::Cpp => &[".h", ".hpp", ".hxx", ".cpp", ".cc", ".cxx"],
        Language::CSharp => &[".cs"],
        Language::Php => &[".php"],
        Language::Ruby => &[".rb"],
        Language::Objc => &[".h", ".m", ".mm"],
        Language::Nix => &[".nix", "/default.nix"],
        _ => &[],
    }
}

// =============================================================================
// External-import classification
// =============================================================================

/// Node built-ins — a JS/TS import of one of these never names a project file.
static JS_NODE_BUILTINS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "fs",
        "path",
        "os",
        "crypto",
        "http",
        "https",
        "url",
        "util",
        "events",
        "stream",
        "child_process",
        "buffer",
    ]
    .into_iter()
    .collect()
});

/// The Python stdlib modules, checked against an import's FIRST dotted segment.
static PYTHON_STDLIB: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "os",
        "sys",
        "json",
        "re",
        "math",
        "datetime",
        "collections",
        "typing",
        "pathlib",
        "logging",
    ]
    .into_iter()
    .collect()
});

/// C and C++ standard-library headers: the C form (`stdio.h`) and both C++ forms
/// (`cstdio`, `vector`). The extractor strips the `<>`/`""` delimiters before the
/// name reaches here.
static C_CPP_STDLIB_HEADERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // C standard library headers
        "assert.h",
        "complex.h",
        "ctype.h",
        "errno.h",
        "fenv.h",
        "float.h",
        "inttypes.h",
        "iso646.h",
        "limits.h",
        "locale.h",
        "math.h",
        "setjmp.h",
        "signal.h",
        "stdalign.h",
        "stdarg.h",
        "stdatomic.h",
        "stdbool.h",
        "stddef.h",
        "stdint.h",
        "stdio.h",
        "stdlib.h",
        "stdnoreturn.h",
        "string.h",
        "tgmath.h",
        "threads.h",
        "time.h",
        "uchar.h",
        "wchar.h",
        "wctype.h",
        // C++ C-library wrappers (the `cname` form)
        "cassert",
        "ccomplex",
        "cctype",
        "cerrno",
        "cfenv",
        "cfloat",
        "cinttypes",
        "ciso646",
        "climits",
        "clocale",
        "cmath",
        "csetjmp",
        "csignal",
        "cstdalign",
        "cstdarg",
        "cstdbool",
        "cstddef",
        "cstdint",
        "cstdio",
        "cstdlib",
        "cstring",
        "ctgmath",
        "ctime",
        "cuchar",
        "cwchar",
        "cwctype",
        // C++ STL headers
        "algorithm",
        "any",
        "array",
        "atomic",
        "barrier",
        "bit",
        "bitset",
        "charconv",
        "chrono",
        "codecvt",
        "compare",
        "complex",
        "concepts",
        "condition_variable",
        "coroutine",
        "deque",
        "exception",
        "execution",
        "expected",
        "filesystem",
        "format",
        "forward_list",
        "fstream",
        "functional",
        "future",
        "generator",
        "initializer_list",
        "iomanip",
        "ios",
        "iosfwd",
        "iostream",
        "istream",
        "iterator",
        "latch",
        "limits",
        "list",
        "locale",
        "map",
        "mdspan",
        "memory",
        "memory_resource",
        "mutex",
        "new",
        "numbers",
        "numeric",
        "optional",
        "ostream",
        "print",
        "queue",
        "random",
        "ranges",
        "ratio",
        "regex",
        "scoped_allocator",
        "semaphore",
        "set",
        "shared_mutex",
        "source_location",
        "span",
        "spanstream",
        "sstream",
        "stack",
        "stacktrace",
        "stdexcept",
        "stdfloat",
        "stop_token",
        "streambuf",
        "string",
        "string_view",
        "strstream",
        "syncstream",
        "system_error",
        "thread",
        "tuple",
        "type_traits",
        "typeindex",
        "typeinfo",
        "unordered_map",
        "unordered_set",
        "utility",
        "valarray",
        "variant",
        "vector",
        "version",
    ]
    .into_iter()
    .collect()
});

/// The hard-coded alias fallbacks, for projects that use these conventional
/// prefixes without declaring them in a tsconfig. Tried **after** the real alias
/// map and the workspace map.
const FALLBACK_ALIASES: [(&str, &str); 6] = [
    ("@/", "src/"),
    ("~/", "src/"),
    ("@src/", "src/"),
    ("src/", "src/"),
    ("@app/", "app/"),
    ("app/", "app/"),
];

/// Is `import_path` a third-party/stdlib specifier — one that names no file in
/// this repo?
///
/// An external import resolves to `None` and its reference is **dropped, not
/// guessed**. Each escape below exists because a real repo lost real edges
/// without it: workspace members look exactly like npm specifiers (#629), a
/// project alias prefix looks bare (`@components/*`), and a Go monorepo's own
/// packages look third-party until `go.mod` is read (#388).
pub fn is_external_import<C: ResolutionContext>(
    import_path: &str,
    language: Language,
    ctx: &C,
) -> bool {
    // Relative imports are never external.
    if import_path.starts_with('.') {
        return false;
    }

    // A workspace member (`@scope/ui/widgets`) is LOCAL to the monorepo even
    // though it looks like a bare npm specifier. The map is `None` for a
    // single-package repo, so this costs nothing there.
    if let Some(ws) = ctx.workspace_packages()
        && resolve_workspace_import(import_path, ws).is_some()
    {
        return false;
    }

    match language {
        Language::Typescript
        | Language::Javascript
        | Language::Tsx
        | Language::Jsx
        | Language::Arkts => {
            if JS_NODE_BUILTINS.contains(import_path) {
                return true;
            }
            // A project-declared alias prefix is LOCAL — without this escape the
            // bare-specifier heuristic below would call `@components/Foo` npm.
            if let Some(aliases) = ctx.project_aliases()
                && aliases
                    .patterns
                    .iter()
                    .any(|p| import_path.starts_with(&p.prefix))
            {
                return false;
            }
            // Everything else bare is npm, unless it uses a conventional prefix.
            !import_path.starts_with("@/")
                && !import_path.starts_with("~/")
                && !import_path.starts_with("src/")
        }
        Language::Python => {
            let first = import_path.split('.').next().unwrap_or(import_path);
            PYTHON_STDLIB.contains(first)
        }
        Language::Go => {
            // In-module imports look like `<module-path>/sub/pkg`. Without this
            // check every cross-package call in a Go monorepo is flagged
            // external and simply never resolves (#388).
            if let Some(m) = ctx.go_module()
                && (import_path == m.module_path
                    || import_path.starts_with(&format!("{}/", m.module_path)))
            {
                return false;
            }
            // `internal/` stays local even with no parsed `go.mod` — the
            // pre-#388 escape hatch.
            if import_path.contains("/internal/") {
                return false;
            }
            true
        }
        Language::C | Language::Cpp => {
            if C_CPP_STDLIB_HEADERS.contains(import_path) {
                return true;
            }
            // `<stdio.h>` and `<cstdio>` are both stdlib.
            let without_ext = import_path.strip_suffix(".h").unwrap_or(import_path);
            C_CPP_STDLIB_HEADERS.contains(without_ext)
        }
        _ => false,
    }
}

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

// =============================================================================
// Path helpers (repo-relative, forward-slashed, lexical)
// =============================================================================

/// The directory holding `file` — `""` for a root-level file.
fn parent_dir(file: &str) -> String {
    match file.rfind('/') {
        Some(i) => file[..i].to_string(),
        None => String::new(),
    }
}

/// Join `rel` onto `dir` and normalize `.`/`..` **lexically**.
///
/// Never touches the filesystem: every path here is a candidate we are about to
/// *look up* in the file index, and `canonicalize` would fail on the ones that
/// do not exist — which is most of them.
fn join_rel(dir: &str, rel: &str) -> String {
    let joined = if dir.is_empty() {
        PathBuf::from(rel)
    } else {
        Path::new(dir).join(rel)
    };

    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn join_rel_normalizes_lexically() {
        assert_eq!(join_rel("src/lib", "./util"), "src/lib/util");
        assert_eq!(join_rel("src/lib", "../util"), "src/util");
        assert_eq!(join_rel("src/a/b", "../../c/d"), "src/c/d");
        assert_eq!(join_rel("", "./x"), "x");
    }

    #[test]
    fn parent_dir_of_a_root_file_is_empty() {
        assert_eq!(parent_dir("main.go"), "");
        assert_eq!(parent_dir("src/a/b.ts"), "src/a");
    }

    /// The extension order is a contract: `.ts` before `/index.ts`, so a repo
    /// holding BOTH `foo.ts` and `foo/index.ts` binds `./foo` to the former.
    #[test]
    fn the_typescript_extension_order_prefers_a_file_over_a_barrel() {
        let exts = extensions_for(Language::Typescript);
        let ts = exts.iter().position(|e| *e == ".ts").unwrap();
        let index = exts.iter().position(|e| *e == "/index.ts").unwrap();
        assert!(ts < index, "`.ts` must be probed before `/index.ts`");
    }

    #[test]
    fn kotlin_has_no_extension_row() {
        assert!(
            extensions_for(Language::Kotlin).is_empty(),
            "deliberate — a Kotlin import is an FQN and resolves through the JVM \
             branch (Task 6), not through extension probing. See the doc comment."
        );
    }
}

// =============================================================================
// resolve_via_import (Task 6)
// =============================================================================

/// How deep a re-export chase may go before it gives up. A barrel-of-barrels is
/// real; an infinite one is a cycle, and the `visited` set catches that — this
/// cap catches the pathological-but-acyclic case.
pub const REEXPORT_MAX_DEPTH: usize = 8;

/// Node kinds that own static members reachable as `Container.member` (#825).
const STATIC_MEMBER_CONTAINERS: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Enum,
    NodeKind::Trait,
    NodeKind::Protocol,
];

/// What `find_exported_symbol` is looking for in a module.
#[derive(Debug, Clone)]
struct Want {
    is_default: bool,
    is_namespace: bool,
    exported_name: String,
    member_name: Option<String>,
}

fn imported(r: &UnresolvedRef, target: &str, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        // ⚠ The STORED ROW, unmutated — the keyed delete matches on it (#760).
        original: r.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: ResolvedBy::Import,
    }
}

/// A Java/Kotlin `import com.example.Bar` → the `Bar` declared in package
/// `com.example`, through the **qualified-name index** — confidence **0.95**.
///
/// Ladder step 5, ahead of the frameworks and the name matcher: a JVM FQN is
/// unambiguous even when several `Bar` classes exist in different packages,
/// which is exactly the collision the path-proximity matcher cannot resolve
/// (#314). JVM imports are decoupled from filenames (a Kotlin `Utils.kt` can
/// export `Bar`), so the JS-style filesystem walk misses them entirely.
pub fn resolve_jvm_import<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    if r.reference_kind != "imports" {
        return None;
    }
    let lang = r.language;
    if !matches!(lang, Language::Java | Language::Kotlin) {
        return None;
    }

    let fqn = r.reference_name.as_str();
    let last_dot = fqn.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let (pkg, sym) = (&fqn[..last_dot], &fqn[last_dot + 1..]);
    // A wildcard import names no single symbol — it punts to name-matching.
    if sym == "*" {
        return None;
    }

    let candidates = ctx.nodes_by_qualified_name(&format!("{pkg}::{sym}"));
    let best = match candidates.len() {
        0 => return None,
        1 => candidates.first().cloned()?,
        _ => pick_closest_jvm_candidate(&candidates, &r.file_path)?,
    };
    Some(imported(r, &best.id, 0.95))
}

/// Among same-FQN candidates, the one **closest to the importing file** by
/// shared directory prefix, preferring an `expect` declaration on a tie.
///
/// Kotlin Multiplatform: an `expect` declaration and its `actual`s share one FQN
/// across source sets (commonMain / androidMain / appleMain). Taking the first
/// candidate let a single platform `actual` absorb every common-side import, so
/// the `expect` — the canonical API a commonMain file imports — looked unused.
fn pick_closest_jvm_candidate(candidates: &[Arc<Node>], from_path: &str) -> Option<Arc<Node>> {
    let from_dirs: Vec<&str> = from_path.split('/').collect();
    let from_dirs = &from_dirs[..from_dirs.len().saturating_sub(1)];

    let shared_prefix = |p: &str| -> usize {
        let parts: Vec<&str> = p.split('/').collect();
        let dirs = &parts[..parts.len().saturating_sub(1)];
        from_dirs
            .iter()
            .zip(dirs.iter())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let is_expect = |n: &Node| n.decorators.iter().any(|d| d == "expect");

    let mut best: Option<&Arc<Node>> = None;
    let mut best_prox = 0usize;
    for c in candidates {
        let prox = shared_prefix(&c.file_path);
        let take = match best {
            None => true,
            Some(b) => prox > best_prox || (prox == best_prox && is_expect(c) && !is_expect(b)),
        };
        if take {
            best_prox = prox;
            best = Some(c);
        }
    }
    best.cloned()
}

/// Bind a reference through the imports its file declares.
///
/// Ladder step 8. The branch order below **is the contract** — the ecosystem
/// branches each own their reference shapes completely, and several of them
/// deliberately **do not fall through** on a miss (a path-shaped reference that
/// finds no file must not then go name-matching: a wrong edge is worse than
/// none, #660).
pub fn resolve_via_import<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    let lang = r.language;

    // --- C/C++ `#include` → a file→file edge -------------------------------
    if matches!(lang, Language::C | Language::Cpp) && r.reference_kind == "imports" {
        return resolve_c_include(r, lang, ctx);
    }

    // Wave 2 (Phase 8), each a NO-FALLTHROUGH branch of its own: COBOL
    // copybooks, Nix path imports (`import ./x.nix`).

    // --- PHP include/require → a file→file edge ----------------------------
    if crate::resolver::is_php_include_path_ref(r) {
        // NO FALLTHROUGH on a miss (#660): falling back to the symbol matcher
        // would mis-connect `inc/db.php` to an unrelated `db.php` elsewhere.
        return resolve_php_include(r, ctx);
    }

    let imports = ctx.import_mappings(&r.file_path);

    // --- Go cross-package (`pkga.FuncX`) ------------------------------------
    if lang == Language::Go
        && let Some(hit) = resolve_go_cross_package(r, &imports, ctx)
    {
        return Some(hit);
    }

    // --- Java/Kotlin (`Foo.bar()` / bare `Foo`, through an imported FQN) -----
    if matches!(lang, Language::Java | Language::Kotlin)
        && let Some(hit) = resolve_jvm_imported_reference(r, lang, &imports, ctx)
    {
        return Some(hit);
    }

    // --- Python module members + absolute dotted modules ---------------------
    if lang == Language::Python {
        if let Some(hit) = resolve_python_module_member(r, &imports, ctx) {
            return Some(hit);
        }
        if let Some(hit) = resolve_python_absolute_module(r, ctx) {
            return Some(hit);
        }
    }

    // --- Rust `crate::a::b::Item` / `self::` / `super::` ---------------------
    if lang == Language::Rust
        && r.reference_name.contains("::")
        && let Some(hit) = resolve_rust_path_reference(r, ctx)
    {
        return Some(hit);
    }

    // Wave 2: Lua/Luau `require(...)`.

    // --- whole-module / namespace imports → the module FILE ------------------
    if matches!(
        lang,
        Language::Python
            | Language::Typescript
            | Language::Tsx
            | Language::Javascript
            | Language::Jsx
            | Language::Arkts
    ) && let Some(hit) = resolve_module_import_to_file(r, lang, &imports, ctx)
    {
        return Some(hit);
    }

    // --- the generic loop: a name bound by an import --------------------------
    for imp in imports.iter() {
        let matches_bare = r.reference_name == imp.local_name;
        let matches_member = r
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_member {
            continue;
        }

        let Some(resolved_path) = resolve_import_path(&imp.source, &r.file_path, lang, ctx) else {
            continue;
        };

        let want = Want {
            is_default: imp.is_default,
            is_namespace: imp.is_namespace,
            exported_name: if imp.is_default {
                "default".to_string()
            } else {
                imp.exported_name.clone()
            },
            member_name: if imp.is_namespace {
                Some(
                    r.reference_name
                        .replacen(&format!("{}.", imp.local_name), "", 1),
                )
            } else {
                None
            },
        };

        let Some(target) =
            find_exported_symbol(&resolved_path, &want, lang, ctx, &mut HashSet::new(), 0)
        else {
            continue;
        };

        // #825 — `Foo.bar()` on a NAMED class import: `find_exported_symbol`
        // resolved `Foo` to the class, so descend into it and bind the MEMBER.
        // Without this the edge points at the class, `create_edges` then promotes
        // the call to `instantiates`, and the static method shows zero callers
        // and a hollow impact radius.
        if !imp.is_namespace
            && matches_member
            && let Some(member) = resolve_static_member(&target, r, &imp.local_name, ctx)
        {
            return Some(imported(r, &member.id, 0.9));
        }

        return Some(imported(r, &target.id, 0.9));
    }

    None
}

/// C/C++: the same-dir sibling first (**0.92**), then the `-I`-resolved path
/// (**0.9**).
///
/// A quoted `#include "X.h"` searches the INCLUDING file's own directory first
/// (the C standard's quoted-include order). Without that preference the include-dir
/// heuristic picks an arbitrary same-named header — a `windows/.../RNCAsyncStorage.h`
/// absorbing the include meant for the `apple/.../RNCAsyncStorage.h` next door —
/// and the real local header ends up with no dependents at all.
fn resolve_c_include<C: ResolutionContext>(
    r: &UnresolvedRef,
    lang: Language,
    ctx: &C,
) -> Option<ResolvedRef> {
    let from_dir = parent_dir(&r.file_path);
    let sibling_path = join_rel(&from_dir, &r.reference_name);
    if let Some(node) = file_node_at(&sibling_path, ctx) {
        return Some(imported(r, &node.id, 0.92));
    }

    let resolved = resolve_import_path(&r.reference_name, &r.file_path, lang, ctx)?;
    let node = file_node_at(&resolved, ctx)?;
    Some(imported(r, &node.id, 0.9))
}

/// PHP `include`/`require` → the included file (**0.9**), or nothing.
///
/// PHP resolves an include relative to the INCLUDING file's directory (the
/// common case for procedural codebases); `php.ini`'s `include_path` is not
/// modeled. The literal may omit `.php`.
fn resolve_php_include<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    let from_dir = parent_dir(&r.file_path);
    let base = join_rel(&from_dir, &r.reference_name);

    let path = if ctx.file_exists(&base) {
        base
    } else {
        let mut found = None;
        for ext in extensions_for(Language::Php) {
            let candidate = format!("{base}{ext}");
            if ctx.file_exists(&candidate) {
                found = Some(candidate);
                break;
            }
        }
        found?
    };

    let node = file_node_at(&path, ctx)?;
    Some(imported(r, &node.id, 0.9))
}

/// Go `pkga.FuncX` — the receiver names an imported PACKAGE DIRECTORY, not a
/// symbol (**0.9**).
///
/// The generic file-based lookup cannot follow that: an import maps to a
/// *directory* containing one or more `.go` files (#388). The candidate must be
/// an **exported** Go node whose **immediate parent directory** is exactly the
/// package dir — matching loosely would let `pkga.FuncX` land on a `FuncX`
/// declared in `pkga/subpkg/`.
fn resolve_go_cross_package<C: ResolutionContext>(
    r: &UnresolvedRef,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    let module = ctx.go_module()?;
    let dot = r.reference_name.find('.')?;
    if dot == 0 {
        return None;
    }
    let receiver = &r.reference_name[..dot];
    let member = &r.reference_name[dot + 1..];
    if member.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        // Only an IN-MODULE import maps to a directory we know.
        let pkg_dir = if imp.source == module.module_path {
            String::new()
        } else if let Some(rest) = imp.source.strip_prefix(&format!("{}/", module.module_path)) {
            rest.to_string()
        } else {
            continue;
        };

        for node in ctx.nodes_by_name(member).iter() {
            if node.language != Language::Go || node.is_exported != Some(true) {
                continue;
            }
            // '\\' only appears in Windows-indexed paths — skip the per-candidate
            // alloc when absent (the common case).
            let same_pkg = if node.file_path.contains('\\') {
                parent_dir(&node.file_path.replace('\\', "/")) == pkg_dir
            } else {
                parent_dir(&node.file_path) == pkg_dir
            };
            if same_pkg {
                return Some(imported(r, &node.id, 0.9));
            }
        }
    }
    None
}

/// Java/Kotlin: a reference whose receiver is the simple name of an imported FQN
/// (**0.9**).
///
/// `import com.example.Foo;` + `Foo.bar()` → the FQN becomes a **path suffix**
/// (`com/example/Foo.java`), which uniquely identifies the right symbol when
/// several classes share a simple name (#314). The file may live under any source
/// root (`src/main/java/`, `src/`, …), so it is matched by suffix, never by exact
/// path. `import static com.example.Foo.bar;` uses the OWNER's path instead.
fn resolve_jvm_imported_reference<C: ResolutionContext>(
    r: &UnresolvedRef,
    lang: Language,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    let ext = if lang == Language::Kotlin {
        ".kt"
    } else {
        ".java"
    };

    for imp in imports {
        let matches_bare = imp.local_name == r.reference_name;
        let matches_qualified = r
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_qualified {
            continue;
        }

        let fqn_path = format!("{}{ext}", imp.source.replace('.', "/"));
        let member_name = if matches_bare {
            imp.local_name.clone()
        } else {
            r.reference_name[imp.local_name.len() + 1..].to_string()
        };

        let candidates = ctx.nodes_by_name(&member_name);
        for node in candidates.iter() {
            if node.language != lang {
                continue;
            }
            let fp: std::borrow::Cow<'_, str> = if node.file_path.contains('\\') {
                node.file_path.replace('\\', "/").into()
            } else {
                node.file_path.as_str().into()
            };
            if fp.ends_with(&fqn_path) {
                return Some(imported(r, &node.id, 0.9));
            }
        }

        // `import static com.example.Util.helper;` — the FQN's tail IS the
        // member, so the owner class's path is what identifies it.
        if matches_bare
            && let Some(dot) = imp.source.rfind('.')
            && dot > 0
        {
            let owner_path = format!("{}{ext}", imp.source[..dot].replace('.', "/"));
            for node in candidates.iter() {
                if node.language != lang {
                    continue;
                }
                let fp: std::borrow::Cow<'_, str> = if node.file_path.contains('\\') {
                    node.file_path.replace('\\', "/").into()
                } else {
                    node.file_path.as_str().into()
                };
                if fp.ends_with(&owner_path) {
                    return Some(imported(r, &node.id, 0.9));
                }
            }
        }
    }
    None
}

/// Python `certs.where()` — the receiver names an imported **module** (a file),
/// not a symbol (**0.85**).
///
/// The generic symbol lookup would search the *package* for `certs` instead of
/// looking **inside** the module. `method` is deliberately excluded from the
/// accepted kinds, so `mod.foo` can never land on a same-named class method.
fn resolve_python_module_member<C: ResolutionContext>(
    r: &UnresolvedRef,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    let dot = r.reference_name.find('.')?;
    if dot == 0 {
        return None;
    }
    let receiver = &r.reference_name[..dot];
    // The IMMEDIATE member of the module: the first segment after the receiver.
    let member = r.reference_name[dot + 1..].split('.').next()?;
    if member.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        // `import mod` binds the module at `source`; `from . import certs` /
        // `from pkg import mod` bind a SUBMODULE whose dotted path is the source
        // joined with the imported name.
        let module_path = if imp.is_namespace {
            imp.source.clone()
        } else if imp.source.ends_with('.') {
            format!("{}{}", imp.source, imp.local_name)
        } else {
            format!("{}.{}", imp.source, imp.local_name)
        };

        // `resolve_import_path` only maps RELATIVE dotted paths; an ABSOLUTE
        // package path resolves to nothing there, so fall back to the dotted
        // module-file lookup. Without this, `module.func()` after
        // `from pkg import module` dropped its `calls` edge even though the
        // import edge resolved (#578).
        let resolved = resolve_import_path(&module_path, &r.file_path, Language::Python, ctx)
            .or_else(|| {
                find_python_module_file(&module_path, &r.file_path, ctx)
                    .map(|n| n.file_path.clone())
            });

        let Some(resolved) = resolved else { continue };
        if resolved == r.file_path {
            continue;
        }

        let group = ctx.nodes_in_file(&resolved);
        let target = group.iter().find(|n| {
            n.name == member
                && matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Class | NodeKind::Variable | NodeKind::Constant
                )
        });
        if let Some(t) = target {
            return Some(imported(r, &t.id, 0.85));
        }
    }
    None
}

/// A Python ABSOLUTE dotted module import (`import a.b.c`) → its file (**0.9**).
///
/// The Django `AppConfig.ready(): import myapp.signals` pattern, and any
/// side-effect module import.
fn resolve_python_absolute_module<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    if r.reference_kind != "imports" {
        return None;
    }
    let node = find_python_module_file(&r.reference_name, &r.file_path, ctx)?;
    Some(imported(r, &node.id, 0.9))
}

/// The file node for a Python dotted module path `a.b.c`: a module file ending
/// in `a/b/c.py`, or a package `a/b/c/__init__.py`.
///
/// Suffix-matched, so a package rooted under `src/` still resolves. `None` for
/// stdlib/external modules (no matching repo file), so `import os` creates no
/// edge.
fn find_python_module_file<C: ResolutionContext>(
    module: &str,
    exclude: &str,
    ctx: &C,
) -> Option<Arc<Node>> {
    if module.is_empty() || module.starts_with('.') {
        return None; // relative imports are handled elsewhere
    }
    let rel = module.replace('.', "/");
    let last_seg = module.rsplit('.').next()?;

    let ends_with = |p: &str, want: &str| p == want || p.ends_with(&format!("/{want}"));

    let module_file = ctx
        .nodes_by_name(&format!("{last_seg}.py"))
        .iter()
        .find(|n| {
            n.kind == NodeKind::File
                && n.file_path != exclude
                && ends_with(&n.file_path, &format!("{rel}.py"))
        })
        .cloned();
    if module_file.is_some() {
        return module_file;
    }

    ctx.nodes_by_name("__init__.py")
        .iter()
        .find(|n| {
            n.kind == NodeKind::File
                && n.file_path != exclude
                && ends_with(&n.file_path, &format!("{rel}/__init__.py"))
        })
        .cloned()
}

/// Rust `crate::m::Item` / `self::sub::Item` / `super::m::func` → the leaf symbol
/// in the module's file (**0.9**).
///
/// Disambiguates the common-name `pub use self::read::read` re-export that
/// name-matching lands on the wrong same-named symbol.
fn resolve_rust_path_reference<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let segments: Vec<&str> = r
        .reference_name
        .split("::")
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let leaf = segments[segments.len() - 1];
    let mod_segs = &segments[..segments.len() - 1];

    let file = resolve_rust_module_file(mod_segs, &r.file_path, ctx)?;
    if file == r.file_path {
        return None;
    }

    let group = ctx.nodes_in_file(&file);
    let target = group.iter().find(|n| {
        n.name == leaf
            && matches!(
                n.kind,
                NodeKind::Function
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::TypeAlias
                    | NodeKind::Constant
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Interface
            )
    })?;
    Some(imported(r, &target.id, 0.9))
}

/// The crate-root directory (the one holding `lib.rs`/`main.rs`), walking up.
///
/// Capped at **64** levels — a repo nested deeper than that is pathological, and
/// an uncapped walk on a symlinked tree does not terminate.
fn rust_crate_root_dir<C: ResolutionContext>(from_file: &str, ctx: &C) -> Option<String> {
    let mut dir = parent_dir(from_file);
    for _ in 0..64 {
        let lib = join_rel(&dir, "lib.rs");
        let main = join_rel(&dir, "main.rs");
        if ctx.file_exists(&lib) || ctx.file_exists(&main) {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent_dir(&dir);
    }
    None
}

/// The directory under which THIS file's module declares its submodules.
///
/// `mod.rs`/`lib.rs`/`main.rs` own their directory; `foo.rs`'s submodules live
/// in `foo/`.
fn rust_self_module_dir(from_file: &str) -> String {
    let dir = parent_dir(from_file);
    let base = from_file.rsplit('/').next().unwrap_or(from_file);
    if matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
        return dir;
    }
    let stem = base.strip_suffix(".rs").unwrap_or(base);
    join_rel(&dir, stem)
}

/// Walk module segments down from `start_dir`, mapping each to `<seg>.rs` or
/// `<seg>/mod.rs`. `None` if any segment has no file.
fn resolve_rust_under<C: ResolutionContext>(
    start_dir: Option<String>,
    rest: &[&str],
    ctx: &C,
) -> Option<String> {
    let mut dir = start_dir?;
    let mut target: Option<String> = None;
    for seg in rest {
        if matches!(*seg, "self" | "crate" | "super") {
            continue;
        }
        let as_file = join_rel(&dir, &format!("{seg}.rs"));
        let as_mod = join_rel(&dir, &format!("{seg}/mod.rs"));
        if ctx.file_exists(&as_file) {
            target = Some(as_file);
        } else if ctx.file_exists(&as_mod) {
            target = Some(as_mod);
        } else {
            return None;
        }
        dir = join_rel(&dir, seg);
    }
    target
}

/// A Rust module path (segments WITHOUT the leaf symbol) → the last module
/// segment's file.
fn resolve_rust_module_file<C: ResolutionContext>(
    segments: &[&str],
    from_file: &str,
    ctx: &C,
) -> Option<String> {
    let first = *segments.first()?;

    match first {
        "crate" => resolve_rust_under(rust_crate_root_dir(from_file, ctx), &segments[1..], ctx),
        "self" => resolve_rust_under(Some(rust_self_module_dir(from_file)), &segments[1..], ctx),
        "super" => {
            let supers = segments.iter().take_while(|s| **s == "super").count();
            let mut dir = Some(rust_self_module_dir(from_file));
            for _ in 0..supers {
                dir = dir.filter(|d| !d.is_empty()).map(|d| parent_dir(&d));
            }
            resolve_rust_under(dir, &segments[supers..], ctx)
        }
        // A BARE path. In expression position (`submodule::item()` — the
        // router-assembly and general cross-module-call pattern) the prefix is a
        // SUBMODULE of the current module, i.e. 2018 `self::`-relative — so try
        // self-relative FIRST, then crate-relative for 2015-edition / crate-root
        // items. An external crate path (`serde::de::Error`) misses both and
        // falls through to name-matching.
        _ => resolve_rust_under(Some(rust_self_module_dir(from_file)), segments, ctx)
            .or_else(|| resolve_rust_under(rust_crate_root_dir(from_file, ctx), segments, ctx)),
    }
}

/// A whole-MODULE import → that module's file (**0.9**) — a file→file dependency.
///
/// The imported name is a module, not a symbol, so there is nothing to bind to —
/// but importing a module IS a dependency on it. It is also the backstop for the
/// Python module-member path and for TS namespace usage: it records the
/// dependency even when the used member is re-exported elsewhere, or the usage is
/// module-level code that is not extracted as a call. A NAMED TS/JS import binds a
/// symbol, not a module, and is deliberately left alone.
fn resolve_module_import_to_file<C: ResolutionContext>(
    r: &UnresolvedRef,
    lang: Language,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    if r.reference_kind != "imports" || r.reference_name.contains('.') {
        return None;
    }

    for imp in imports {
        if imp.local_name != r.reference_name {
            continue;
        }

        let module_path = if imp.is_namespace || imp.is_default {
            // `import * as ns from './x'` / `import x from './x'` — the
            // dependency is on the module FILE. A default import binds a
            // (possibly renamed) local to whatever the module default-exports, so
            // the binding name is not findable as a symbol. An external module
            // resolves to no file, so `import React from 'react'` creates no edge.
            imp.source.clone()
        } else if lang == Language::Python {
            // `from . import certs` — the imported NAME is a submodule of the source.
            if imp.source.ends_with('.') {
                format!("{}{}", imp.source, imp.local_name)
            } else {
                format!("{}.{}", imp.source, imp.local_name)
            }
        } else {
            // A named TS/JS import binds a symbol, not a module.
            continue;
        };

        if let Some(resolved) = resolve_import_path(&module_path, &r.file_path, lang, ctx)
            && resolved != r.file_path
            && let Some(node) = file_node_at(&resolved, ctx)
        {
            return Some(imported(r, &node.id, 0.9));
        }

        // Python's absolute `from a.b import submodule` (a FastAPI router
        // aggregator's `from app.api.routes import authentication`):
        // `resolve_import_path` maps only RELATIVE dotted paths.
        if lang == Language::Python
            && let Some(node) = find_python_module_file(&module_path, &r.file_path, ctx)
        {
            return Some(imported(r, &node.id, 0.9));
        }
    }
    None
}

/// The symbol a module exports under `want` — chasing re-exports.
///
/// Order: a **direct hit** in the file, then **named** re-exports (following the
/// rename), then **wildcard** re-exports (the barrel-of-barrels case). Capped at
/// [`REEXPORT_MAX_DEPTH`], with a `visited` set so a cyclic barrel terminates.
fn find_exported_symbol<C: ResolutionContext>(
    file_path: &str,
    want: &Want,
    lang: Language,
    ctx: &C,
    visited: &mut HashSet<String>,
    depth: usize,
) -> Option<Arc<Node>> {
    if depth > REEXPORT_MAX_DEPTH || !visited.insert(file_path.to_string()) {
        return None;
    }

    let nodes = ctx.nodes_in_file(file_path);

    // 1. A direct hit: the symbol is declared right here.
    let direct = if want.is_default {
        // A Svelte/Vue single-file component IS the module's default export, but
        // extracts as kind `component` — so prefer it, then fall back to an
        // exported function/class (the `export default fn` case). Without the
        // component branch, `export { default as X } from './X.svelte'` never
        // resolves and the component shows a false 0 callers (#629).
        nodes
            .iter()
            .find(|n| n.is_exported == Some(true) && n.kind == NodeKind::Component)
            .or_else(|| {
                nodes.iter().find(|n| {
                    n.is_exported == Some(true)
                        && matches!(n.kind, NodeKind::Function | NodeKind::Class)
                })
            })
    } else if want.is_namespace
        && let Some(member) = &want.member_name
    {
        nodes
            .iter()
            .find(|n| n.name == *member && n.is_exported == Some(true))
    } else {
        nodes
            .iter()
            .find(|n| n.name == want.exported_name && n.is_exported == Some(true))
    };
    if let Some(hit) = direct {
        return Some(hit.clone());
    }

    // 2. A re-export hit: this file forwards the symbol somewhere else.
    let re_exports = ctx.re_exports(file_path);
    if re_exports.is_empty() {
        return None;
    }

    let target_name = if want.is_default {
        "default"
    } else {
        want.exported_name.as_str()
    };

    // Named re-exports first — and the RENAME is followed: to chase `login`
    // through `export { signIn as login } from './auth'`, look for `signIn`.
    for rex in re_exports.iter() {
        if let ReExport::Named {
            exported_name,
            original_name,
            source,
        } = rex
            && exported_name == target_name
            && let Some(next) = resolve_import_path(source, file_path, lang, ctx)
        {
            let chained = Want {
                is_default: original_name == "default",
                is_namespace: false,
                exported_name: original_name.clone(),
                member_name: None,
            };
            if let Some(hit) = find_exported_symbol(&next, &chained, lang, ctx, visited, depth + 1)
            {
                return Some(hit);
            }
        }
    }

    // 3. Wildcard re-exports last — try every forwarding source.
    for rex in re_exports.iter() {
        if let ReExport::Wildcard { source } = rex
            && let Some(next) = resolve_import_path(source, file_path, lang, ctx)
            && let Some(hit) = find_exported_symbol(&next, want, lang, ctx, visited, depth + 1)
        {
            return Some(hit);
        }
    }

    None
}

/// `Container.member` on a NAMED class import → the member node (#825).
///
/// Members carry a `Container::member` qualified name, so look up
/// `{container.qualified_name}::{member}` **within the container's own file** —
/// the file filter is what disambiguates same-named classes in other modules.
/// `None` when the container is not a member-owning kind or the member is absent,
/// so the caller falls back to the container itself.
fn resolve_static_member<C: ResolutionContext>(
    container: &Node,
    r: &UnresolvedRef,
    local_name: &str,
    ctx: &C,
) -> Option<Arc<Node>> {
    if !STATIC_MEMBER_CONTAINERS.contains(&container.kind) {
        return None;
    }
    // The first segment after the receiver: `Foo.bar.baz` → `bar`.
    let member = r.reference_name[local_name.len() + 1..].split('.').next()?;
    if member.is_empty() {
        return None;
    }

    let candidates: Vec<Arc<Node>> = ctx
        .nodes_by_qualified_name(&format!("{}::{member}", container.qualified_name))
        .iter()
        .filter(|n| n.file_path == container.file_path)
        .cloned()
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // A CALL prefers a callable member when several nodes share the qualified
    // name (a static property and a method can collide).
    if r.reference_kind == "calls"
        && let Some(callable) = candidates
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Method | NodeKind::Function))
    {
        return Some(callable.clone());
    }
    candidates.into_iter().next()
}

/// The `file`-kind node at `path`.
fn file_node_at<C: ResolutionContext>(path: &str, ctx: &C) -> Option<Arc<Node>> {
    ctx.nodes_in_file(path)
        .iter()
        .find(|n| n.kind == NodeKind::File)
        .cloned()
}
