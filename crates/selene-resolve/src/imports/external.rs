//! External-import classification: stdlib/third-party specifiers that name no
//! file in this repo.

use std::collections::HashSet;
use std::sync::LazyLock;

use selene_core::Language;

use crate::context::ResolutionContext;
use crate::imports::workspace::resolve_workspace_import;

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
pub(super) const FALLBACK_ALIASES: [(&str, &str); 6] = [
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
