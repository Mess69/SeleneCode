//! `is_built_in_or_external` — step 1 of the `resolve_one` ladder, and the sets
//! it consults.
//!
//! Ported **verbatim** from `../codegraph/src/resolution/index.ts` lines 71–196
//! (`maps/resolution.md` §`resolveOne` pipeline: "Exact sets are in index.ts
//! lines 71–196 — port verbatim"). These sets are load-bearing precision
//! contracts: every entry is here because a real repo produced a wrong edge
//! without it.
//!
//! # The two shadowing rules — read these before "cleaning up" a set
//!
//! - **Python.** A dotted call whose receiver is a builtin type (`dict.update`)
//!   or whose member is a builtin method (`items.append`) is filtered — *unless*
//!   the **capitalized receiver** names a real symbol (`MyDict.update`: the user
//!   class wins). A **bare** builtin-method name (`get`, `index`, `count`) is
//!   filtered only when **nothing in the codebase declares it** — a Flask view
//!   `def get()` is a real target, and without this guard every handler named
//!   after a builtin method silently loses its route→handler edge.
//! - **C/C++.** `std::` is filtered unconditionally (tree-sitter never emits it
//!   as a user-defined qualified name). But `C_BUILT_INS`/`CPP_BUILT_INS` are
//!   filtered **only when the name has no possible match in the graph** —
//!   C projects routinely shadow stdlib names (custom allocators define
//!   `malloc`/`free`, stream wrappers define `read`/`write`/`open`, containers
//!   define `move`/`swap`). Filtering those makes the graph **wrong**, not
//!   cleaner. User shadowing wins.
//!
//! Wave-2 arms (ArkTS `$r`/`$rawfile`, Pascal `PASCAL_UNIT_PREFIXES` /
//! `PASCAL_BUILT_INS`) are deliberately **not ported here** — those languages
//! have no extractor yet (Phase 8), so a ref in them cannot exist. Their sets
//! land with their languages.

use std::collections::HashSet;
use std::sync::LazyLock;

use selene_core::{Language, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::resolver::has_any_possible_match;

/// Build a `HashSet` from a literal list (the TS `new Set([...])` shape).
fn set(items: &[&'static str]) -> HashSet<&'static str> {
    items.iter().copied().collect()
}

static JS_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "console",
        "window",
        "document",
        "global",
        "process",
        "Promise",
        "Array",
        "Object",
        "String",
        "Number",
        "Boolean",
        "Date",
        "Math",
        "JSON",
        "RegExp",
        "Error",
        "Map",
        "Set",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
        "fetch",
        "require",
        "module",
        "exports",
        "__dirname",
        "__filename",
    ])
});

static REACT_HOOKS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "useState",
        "useEffect",
        "useContext",
        "useReducer",
        "useCallback",
        "useMemo",
        "useRef",
        "useLayoutEffect",
        "useImperativeHandle",
        "useDebugValue",
    ])
});

static PYTHON_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "print",
        "len",
        "range",
        "str",
        "int",
        "float",
        "list",
        "dict",
        "set",
        "tuple",
        "open",
        "input",
        "type",
        "isinstance",
        "hasattr",
        "getattr",
        "setattr",
        "super",
        "self",
        "cls",
        "None",
        "True",
        "False",
    ])
});

static PYTHON_BUILT_IN_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "list",
        "dict",
        "set",
        "tuple",
        "str",
        "int",
        "float",
        "bool",
        "bytes",
        "bytearray",
        "frozenset",
        "object",
        "super",
    ])
});

static PYTHON_BUILT_IN_METHODS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "append",
        "extend",
        "insert",
        "remove",
        "pop",
        "clear",
        "sort",
        "reverse",
        "copy",
        "update",
        "keys",
        "values",
        "items",
        "get",
        "add",
        "discard",
        "union",
        "intersection",
        "difference",
        "split",
        "join",
        "strip",
        "lstrip",
        "rstrip",
        "replace",
        "lower",
        "upper",
        "startswith",
        "endswith",
        "find",
        "index",
        "count",
        "encode",
        "decode",
        "format",
        "isdigit",
        "isalpha",
        "isalnum",
        "read",
        "write",
        "readline",
        "readlines",
        "close",
        "flush",
        "seek",
    ])
});

static GO_STDLIB_PACKAGES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "fmt",
        "os",
        "io",
        "net",
        "http",
        "log",
        "math",
        "sort",
        "sync",
        "time",
        "path",
        "bytes",
        "strings",
        "strconv",
        "errors",
        "context",
        "json",
        "xml",
        "csv",
        "html",
        "template",
        "regexp",
        "reflect",
        "runtime",
        "testing",
        "flag",
        "bufio",
        "crypto",
        "encoding",
        "filepath",
        "hash",
        "mime",
        "rand",
        "signal",
        "sql",
        "syscall",
        "unicode",
        "unsafe",
        "atomic",
        "binary",
        "debug",
        "exec",
        "heap",
        "ring",
        "scanner",
        "tar",
        "zip",
        "gzip",
        "zlib",
        "tls",
        "url",
        "user",
        "pprof",
        "trace",
        "ast",
        "build",
        "parser",
        "printer",
        "token",
        "types",
        "cgo",
        "plugin",
        "race",
        "ioutil",
        // Kubernetes-common stdlib aliases
        "utilruntime",
        "utilwait",
        "utilnet",
    ])
});

static GO_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "make",
        "new",
        "len",
        "cap",
        "append",
        "copy",
        "delete",
        "close",
        "panic",
        "recover",
        "print",
        "println",
        "complex",
        "real",
        "imag",
        "error",
        "nil",
        "true",
        "false",
        "iota",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "uintptr",
        "float32",
        "float64",
        "complex64",
        "complex128",
        "string",
        "bool",
        "byte",
        "rune",
        "any",
    ])
});

static C_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        // Standard C library functions
        "printf",
        "fprintf",
        "sprintf",
        "snprintf",
        "scanf",
        "fscanf",
        "sscanf",
        "malloc",
        "calloc",
        "realloc",
        "free",
        "memcpy",
        "memmove",
        "memset",
        "memcmp",
        "memchr",
        "strlen",
        "strcpy",
        "strncpy",
        "strcat",
        "strncat",
        "strcmp",
        "strncmp",
        "strstr",
        "strchr",
        "strrchr",
        "strtok",
        "strdup",
        "fopen",
        "fclose",
        "fread",
        "fwrite",
        "fgets",
        "fputs",
        "fputc",
        "fgetc",
        "feof",
        "ferror",
        "fflush",
        "fseek",
        "ftell",
        "rewind",
        "exit",
        "abort",
        "atexit",
        "atoi",
        "atol",
        "atof",
        "strtol",
        "strtoul",
        "strtod",
        "qsort",
        "bsearch",
        "abs",
        "labs",
        "rand",
        "srand",
        "sin",
        "cos",
        "tan",
        "sqrt",
        "pow",
        "log",
        "log10",
        "exp",
        "ceil",
        "floor",
        "fabs",
        "time",
        "clock",
        "difftime",
        "mktime",
        "localtime",
        "gmtime",
        "strftime",
        "asctime",
        "assert",
        "errno",
        "perror",
        "remove",
        "rename",
        "tmpfile",
        "tmpnam",
        "getenv",
        "system",
        "signal",
        "raise",
        "setjmp",
        "longjmp",
        "va_start",
        "va_end",
        "va_arg",
        "va_copy",
        "NULL",
        "EOF",
        "BUFSIZ",
        "FILENAME_MAX",
        "RAND_MAX",
        "EXIT_SUCCESS",
        "EXIT_FAILURE",
        "size_t",
        "ptrdiff_t",
        "wchar_t",
        "intptr_t",
        "uintptr_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "FILE",
        // POSIX additions commonly seen
        "stat",
        "lstat",
        "fstat",
        "open",
        "close",
        "read",
        "write",
        "pipe",
        "fork",
        "exec",
        "waitpid",
        "getpid",
        "getppid",
        "kill",
        "sleep",
        "usleep",
        "pthread_create",
        "pthread_join",
        "pthread_mutex_lock",
        "pthread_mutex_unlock",
        "dlopen",
        "dlsym",
        "dlclose",
    ])
});

static CPP_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        // iostream objects (often used without std:: prefix via `using`)
        "cout",
        "cin",
        "cerr",
        "clog",
        "endl",
        "flush",
        "ws",
        "std", // the namespace itself when used as std::something
        // Common C++ keywords that leak as references
        "nullptr",
        "true",
        "false",
        "this",
        "sizeof",
        "alignof",
        "typeid",
        "static_cast",
        "dynamic_cast",
        "reinterpret_cast",
        "const_cast",
        "make_unique",
        "make_shared",
        "make_pair",
        "move",
        "forward",
        "swap",
    ])
});

/// ASCII capitalization of the first character (`items` → `Items`).
///
/// The TS source is `charAt(0).toUpperCase() + slice(1)`. ASCII-only here, a
/// deliberate policy call (`maps/resolution.md` §Rust port notes: "ASCII-only
/// matches the TS behavior closely enough for identifiers") — a Unicode-aware
/// uppercase can change a string's *length*, which would corrupt the slicing.
pub(crate) fn capitalize_ascii(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Is this reference a language built-in or an external-library symbol we
/// should not even try to resolve?
///
/// Step 1 of the ladder: an early, cheap `None`. See the module docs for the two
/// shadowing rules (Python's `known_names` guard and C/C++'s
/// `!has_any_possible_match`) — they are what keep the filter from deleting real
/// user symbols that happen to share a builtin's name.
pub fn is_built_in_or_external<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> bool {
    let name = r.reference_name.as_str();
    let lang = r.language;

    let is_js_ts = matches!(
        lang,
        Language::Typescript
            | Language::Javascript
            | Language::Tsx
            | Language::Jsx
            | Language::Arkts
    );

    // --- JS / TS -------------------------------------------------------------
    if is_js_ts {
        if JS_BUILT_INS.contains(name) {
            return true;
        }
        // Common library calls: console.log, Math.floor, JSON.parse.
        if name.starts_with("console.") || name.starts_with("Math.") || name.starts_with("JSON.") {
            return true;
        }
        if REACT_HOOKS.contains(name) {
            return true;
        }
    }

    // --- Python --------------------------------------------------------------
    if lang == Language::Python {
        if PYTHON_BUILT_INS.contains(name) {
            return true;
        }
        if let Some(dot) = name.find('.')
            && dot > 0
        {
            let receiver = &name[..dot];
            let method = &name[dot + 1..];
            // Calls on built-in types: list.append, dict.update, …
            if PYTHON_BUILT_IN_TYPES.contains(receiver) {
                return true;
            }
            // Built-in methods on a non-class receiver (`items.append` where
            // `items` is a local list) — UNLESS the capitalized receiver names a
            // real class (`MyDict.update`: the user's class wins).
            if PYTHON_BUILT_IN_METHODS.contains(method)
                && !ctx.known_names().contains(&capitalize_ascii(receiver))
            {
                return true;
            }
        }
        // A BARE name colliding with a builtin method (`get`, `index`, `count`)
        // is a builtin only when NOTHING in the codebase declares it. A Flask
        // view `def get()` is a real target — without this guard every handler
        // named after a builtin method silently loses its route→handler edge.
        if PYTHON_BUILT_IN_METHODS.contains(name) && !ctx.known_names().contains(name) {
            return true;
        }
    }

    // --- Go ------------------------------------------------------------------
    if lang == Language::Go {
        if let Some(dot) = name.find('.')
            && dot > 0
            && GO_STDLIB_PACKAGES.contains(&name[..dot])
        {
            return true;
        }
        if GO_BUILT_INS.contains(name) {
            return true;
        }
    }

    // --- C / C++ -------------------------------------------------------------
    if matches!(lang, Language::C | Language::Cpp) {
        // `std::foo` is never a user-defined qualified name in tree-sitter
        // output — safe to filter unconditionally.
        if name.starts_with("std::") {
            return true;
        }
        if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
            // USER SHADOWING WINS: only filter when no user symbol could match.
            return !has_any_possible_match(name, ctx);
        }
    }

    // Wave 2 (Phase 8), each landing with its language: ArkTS `$r`/`$rawfile`;
    // Pascal's `PASCAL_UNIT_PREFIXES` (startsWith) + `PASCAL_BUILT_INS`.

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalize_is_ascii_and_length_preserving() {
        assert_eq!(capitalize_ascii("items"), "Items");
        assert_eq!(capitalize_ascii("Items"), "Items");
        assert_eq!(capitalize_ascii(""), "");
        assert_eq!(capitalize_ascii("_x"), "_x");
        // Non-ASCII is left alone — a Unicode uppercase could change the byte
        // length, and identifier receivers are ASCII in every case that matters.
        assert_eq!(capitalize_ascii("ünter"), "ünter");
    }

    #[test]
    fn the_sets_carry_their_load_bearing_entries() {
        // Spot-check the entries the map calls out by name; the full sets are
        // exercised through `is_built_in_or_external` in the ladder tests.
        assert!(JS_BUILT_INS.contains("console"));
        assert!(REACT_HOOKS.contains("useEffect"));
        assert!(PYTHON_BUILT_INS.contains("print"));
        assert!(PYTHON_BUILT_IN_TYPES.contains("dict"));
        assert!(PYTHON_BUILT_IN_METHODS.contains("get"));
        assert!(GO_STDLIB_PACKAGES.contains("fmt"));
        assert!(GO_STDLIB_PACKAGES.contains("utilruntime")); // the k8s aliases
        assert!(GO_BUILT_INS.contains("make"));
        assert!(C_BUILT_INS.contains("malloc"));
        assert!(CPP_BUILT_INS.contains("make_unique"));
    }
}
