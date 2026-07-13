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

use selene_core::Language;

use crate::context::ResolutionContext;
use crate::imports::aliases::apply_aliases;
use crate::imports::workspace::resolve_workspace_import;

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
