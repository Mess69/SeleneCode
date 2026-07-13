//! [`extract_import_mappings`] and [`extract_re_exports`] — the import bindings
//! a file declares, and the re-exports a barrel forwards.
//!
//! Regexes over **raw source**, ported verbatim from
//! `../codegraph/src/resolution/import-resolver.ts` (`maps/resolution.md`
//! §Import-mapping extraction). These are not the extractor's AST — resolution
//! needs the *binding* (`local_name` → `source`), which no `EdgeKind` carries,
//! and re-parsing a file with tree-sitter to recover it would cost more than the
//! whole resolution pass.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::Language;

use crate::types::{ImportMapping, ReExport};

/// Compile a literal pattern. The literal is compile-time known and every one
/// of them is exercised by a test in this file, so a bad pattern fails a test
/// rather than a production run (the house idiom).
macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by the tests below
            Regex::new($pat).unwrap()
        })
    };
}

// --- JS / TS -----------------------------------------------------------------

static JS_IMPORT: LazyLock<Regex> = re!(
    r#"import\s+(?:(\w+)\s*,?\s*)?(?:\{([^}]+)\})?\s*(?:(\*)\s+as\s+(\w+))?\s*from\s*['"]([^'"]+)['"]"#
);
static JS_REQUIRE: LazyLock<Regex> =
    re!(r#"(?:const|let|var)\s+(?:(\w+)|\{([^}]+)\})\s*=\s*require\(['"]([^'"]+)['"]\)"#);
/// `a as b` inside a `{ … }` specifier list.
static AS_ALIAS: LazyLock<Regex> = re!(r"(\w+)\s+as\s+(\w+)");
/// `{ a: b }` inside a `require` destructure.
static COLON_ALIAS: LazyLock<Regex> = re!(r"(\w+)\s*:\s*(\w+)");

// --- Python ------------------------------------------------------------------

static PY_FROM_IMPORT: LazyLock<Regex> = re!(r"from\s+([\w.]+)\s+import\s+([^#\n]+)");
static PY_IMPORT: LazyLock<Regex> = re!(r"(?m)^import\s+([\w.]+)(?:\s+as\s+(\w+))?");

// --- Go ----------------------------------------------------------------------

static GO_SINGLE_IMPORT: LazyLock<Regex> = re!(r#"import\s+(?:(\w+)\s+)?["']([^"']+)["']"#);
static GO_BLOCK_IMPORT: LazyLock<Regex> = re!(r"(?s)import\s*\(\s*([^)]+)\s*\)");
static GO_BLOCK_LINE: LazyLock<Regex> = re!(r#"(?:(\w+)\s+)?["']([^"']+)["']"#);

// --- JVM ---------------------------------------------------------------------

static JVM_IMPORT: LazyLock<Regex> = re!(r"(?m)^\s*import\s+(static\s+)?([\w.]+(?:\.\*)?)\s*;");
static BLOCK_COMMENT: LazyLock<Regex> = re!(r"(?s)/\*.*?\*/");
static LINE_COMMENT: LazyLock<Regex> = re!(r"//[^\n]*");

// --- PHP / C / C++ -----------------------------------------------------------

static PHP_USE: LazyLock<Regex> = re!(r"use\s+([\w\\]+)(?:\s+as\s+(\w+))?;");
static CPP_INCLUDE: LazyLock<Regex> = re!(r#"(?m)^\s*#\s*include\s+[<"]([^>"]+)[>"]"#);
static HEADER_EXT: LazyLock<Regex> = re!(r"\.(h|hpp|hxx|hh|inl|ipp|cxx|cc|cpp)$");

// --- Re-exports --------------------------------------------------------------

static REEXPORT_WILDCARD: LazyLock<Regex> =
    re!(r#"export\s*\*(?:\s+as\s+\w+)?\s*from\s*['"]([^'"]+)['"]"#);
static REEXPORT_NAMED: LazyLock<Regex> = re!(r#"export\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#);
static NAMED_ALIAS_EXACT: LazyLock<Regex> = re!(r"^(\w+)\s+as\s+(\w+)$");
static BARE_WORD: LazyLock<Regex> = re!(r"^\w+$");
/// A barrel whose OWN extension is JS-family — see [`extract_re_exports`].
static JS_FAMILY_EXT: LazyLock<Regex> = re!(r"(?i)\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?|ets)$");

fn mapping(
    local: &str,
    exported: &str,
    source: &str,
    is_default: bool,
    is_ns: bool,
) -> ImportMapping {
    ImportMapping {
        local_name: local.to_string(),
        exported_name: exported.to_string(),
        source: source.to_string(),
        is_default,
        is_namespace: is_ns,
        resolved_path: None,
    }
}

/// The import bindings `content` declares.
///
/// Order is source order (the regexes scan left to right), and it is stable —
/// the resolver's generic loop takes the first mapping whose `local_name`
/// matches, so a reordering here would change which import a name binds to.
pub fn extract_import_mappings(
    _file_path: &str,
    content: &str,
    language: Language,
) -> Vec<ImportMapping> {
    match language {
        // Svelte/Vue/Astro (wave 2) reuse the JS regex over the WHOLE file: they
        // import via plain ES6 inside `<script>` (Astro: the `---` frontmatter),
        // and the regex only matches `import … from '…'`, so running it over
        // markup and styles too is safe (#629).
        Language::Typescript
        | Language::Javascript
        | Language::Tsx
        | Language::Jsx
        | Language::Arkts
        | Language::Svelte
        | Language::Vue
        | Language::Astro => extract_js_imports(content),
        Language::Python => extract_python_imports(content),
        Language::Go => extract_go_imports(content),
        Language::Java | Language::Kotlin => extract_jvm_imports(content),
        Language::Php => extract_php_imports(content),
        Language::C | Language::Cpp => extract_cpp_imports(content),
        // Ruby has no symbol imports at all: `require` names a PATH, and the
        // language has no binding form for a foreign symbol. Deliberately empty
        // — not an oversight.
        _ => Vec::new(),
    }
}

fn extract_js_imports(content: &str) -> Vec<ImportMapping> {
    let mut out = Vec::new();

    for caps in JS_IMPORT.captures_iter(content) {
        let source = caps.get(5).map(|m| m.as_str()).unwrap_or_default();

        // `import Foo from 'm'`
        if let Some(default_import) = caps.get(1) {
            out.push(mapping(
                default_import.as_str(),
                "default",
                source,
                true,
                false,
            ));
        }

        // `import { a, b as c } from 'm'`
        if let Some(named) = caps.get(2) {
            for raw in named.as_str().split(',') {
                let item = raw.trim();
                if item.is_empty() {
                    continue;
                }
                match AS_ALIAS.captures(item) {
                    Some(a) => out.push(mapping(&a[2], &a[1], source, false, false)),
                    None => out.push(mapping(item, item, source, false, false)),
                }
            }
        }

        // `import * as ns from 'm'`
        if caps.get(3).is_some()
            && let Some(ns) = caps.get(4)
        {
            out.push(mapping(ns.as_str(), "*", source, false, true));
        }
    }

    for caps in JS_REQUIRE.captures_iter(content) {
        let source = caps.get(3).map(|m| m.as_str()).unwrap_or_default();
        if let Some(default_name) = caps.get(1) {
            out.push(mapping(
                default_name.as_str(),
                "default",
                source,
                true,
                false,
            ));
        }
        if let Some(destructured) = caps.get(2) {
            for raw in destructured.as_str().split(',') {
                let item = raw.trim();
                if item.is_empty() {
                    continue;
                }
                // `{ a: b }` renames in a require destructure.
                match COLON_ALIAS.captures(item) {
                    Some(a) => out.push(mapping(&a[2], &a[1], source, false, false)),
                    None => out.push(mapping(item, item, source, false, false)),
                }
            }
        }
    }

    out
}

fn extract_python_imports(content: &str) -> Vec<ImportMapping> {
    let mut out = Vec::new();

    // `from X import a, b as c`
    for caps in PY_FROM_IMPORT.captures_iter(content) {
        let source = &caps[1];
        for raw in caps[2].split(',') {
            let item = raw.trim();
            if item.is_empty() || item == "*" {
                continue;
            }
            match AS_ALIAS.captures(item) {
                Some(a) => out.push(mapping(&a[2], &a[1], source, false, false)),
                None => out.push(mapping(item, item, source, false, false)),
            }
        }
    }

    // `import a.b.c` / `import a.b.c as abc` — a NAMESPACE binding whose local
    // name is the alias, else the LAST dotted segment.
    for caps in PY_IMPORT.captures_iter(content) {
        let source = &caps[1];
        let local = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| source.rsplit('.').next().unwrap_or(source).to_string());
        out.push(mapping(&local, "*", source, false, true));
    }

    out
}

fn extract_go_imports(content: &str) -> Vec<ImportMapping> {
    let mut out = Vec::new();
    let push_go = |alias: Option<&str>, source: &str, out: &mut Vec<ImportMapping>| {
        let package_name = source.rsplit('/').next().unwrap_or(source);
        out.push(mapping(
            alias.unwrap_or(package_name),
            "*",
            source,
            false,
            true,
        ));
    };

    // `import "path"` / `import alias "path"`
    for caps in GO_SINGLE_IMPORT.captures_iter(content) {
        push_go(caps.get(1).map(|m| m.as_str()), &caps[2], &mut out);
    }

    // `import ( … )` blocks.
    for block in GO_BLOCK_IMPORT.captures_iter(content) {
        for caps in GO_BLOCK_LINE.captures_iter(&block[1]) {
            push_go(caps.get(1).map(|m| m.as_str()), &caps[2], &mut out);
        }
    }

    out
}

fn extract_jvm_imports(content: &str) -> Vec<ImportMapping> {
    // Strip comments first, so `// import foo;` never becomes a binding.
    let no_blocks = BLOCK_COMMENT.replace_all(content, "");
    let stripped = LINE_COMMENT.replace_all(&no_blocks, "");

    let mut out = Vec::new();
    for caps in JVM_IMPORT.captures_iter(&stripped) {
        let fqn = &caps[2];
        // `import com.example.*;` — a wildcard names no single local symbol.
        // Skipped: name-matching reaches the members it would have bound.
        if fqn.ends_with(".*") {
            continue;
        }
        let Some(local) = fqn.rsplit('.').next().filter(|s| !s.is_empty()) else {
            continue;
        };
        // A JVM import carries the FULL qualified name of the symbol — which is
        // exactly the disambiguation signal when two packages both declare a
        // `FooConverter` (#314). `import static com.example.Foo.bar;` binds
        // `bar` → the FQN, so a bare `bar(...)` call site resolves through the
        // same lookup.
        out.push(mapping(local, local, fqn, false, false));
    }
    out
}

fn extract_php_imports(content: &str) -> Vec<ImportMapping> {
    PHP_USE
        .captures_iter(content)
        .map(|caps| {
            let full_path = &caps[1];
            let class_name = full_path.rsplit('\\').next().unwrap_or(full_path);
            let local = caps.get(2).map(|m| m.as_str()).unwrap_or(class_name);
            mapping(local, class_name, full_path, false, false)
        })
        .collect()
}

fn extract_cpp_imports(content: &str) -> Vec<ImportMapping> {
    CPP_INCLUDE
        .captures_iter(content)
        .map(|caps| {
            let module_path = &caps[1];
            // `#include` brings every symbol from the header into scope — a
            // namespace import. `local_name` is the header's basename without
            // its extension, so a `MyClass` reference can match any include that
            // might provide it.
            let base = module_path.rsplit('/').next().unwrap_or(module_path);
            let basename = HEADER_EXT.replace(base, "");
            let local = if basename.is_empty() {
                module_path
            } else {
                basename.as_ref()
            };
            mapping(local, "*", module_path, false, true)
        })
        .collect()
}

/// The re-exports declared by the barrel at `barrel_path`.
///
/// # Keyed to the BARREL's extension, not the consumer's language (#629)
///
/// Re-exports are a JS/TS construct, and what matters is the language of the
/// **barrel file itself**. A `.svelte` or `.vue` consumer threading its own
/// language down the re-export chase would make this bail on a `.ts` index
/// barrel and silently break the chain — which surfaced as a false "0 callers"
/// on a live component. So the parse is keyed on `barrel_path`'s extension, and
/// the consumer's language never reaches this function.
pub fn extract_re_exports(content: &str, barrel_path: &str) -> Vec<ReExport> {
    if !JS_FAMILY_EXT.is_match(barrel_path) {
        return Vec::new();
    }

    // A commented-out `// export { x } from './y'` must not produce a phantom
    // edge — and the strip is string-AWARE, because a regex that eats `//`
    // inside a string literal corrupts the barrel (a `"https://…"` URL would
    // truncate everything after it).
    let cleaned = strip_js_comments(content);

    let mut out = Vec::new();

    for caps in REEXPORT_WILDCARD.captures_iter(&cleaned) {
        out.push(ReExport::Wildcard {
            source: caps[1].to_string(),
        });
    }

    for caps in REEXPORT_NAMED.captures_iter(&cleaned) {
        let source = caps[2].to_string();
        for raw in caps[1].split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(a) = NAMED_ALIAS_EXACT.captures(item) {
                // `export { signIn as login }` — the RENAME is what the chase
                // follows: a ref to `login` resolves to the source's `signIn`.
                out.push(ReExport::Named {
                    exported_name: a[2].to_string(),
                    original_name: a[1].to_string(),
                    source: source.clone(),
                });
            } else if BARE_WORD.is_match(item) {
                out.push(ReExport::Named {
                    exported_name: item.to_string(),
                    original_name: item.to_string(),
                    source: source.clone(),
                });
            }
        }
    }

    out
}

/// Blank JS comments, **string-aware**: a `//` inside a string literal is not a
/// comment. Everything that is not a comment is preserved byte-for-byte,
/// including multi-byte UTF-8 (the scanner steps by CHAR, never by byte — an
/// earlier byte-wise version turned `'→'` into mojibake, which a test caught).
fn strip_js_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_string: Option<u8> = None;

    while i < bytes.len() {
        let ch = bytes[i];

        if let Some(quote) = in_string {
            // An escape carries the NEXT char through verbatim (it may be the
            // closing quote, or a multi-byte char).
            if ch == b'\\' {
                i = copy_char(src, i, &mut out);
                if i < bytes.len() {
                    i = copy_char(src, i, &mut out);
                }
                continue;
            }
            if ch == quote {
                in_string = None;
            }
            i = copy_char(src, i, &mut out);
            continue;
        }

        match ch {
            b'"' | b'\'' | b'`' => {
                in_string = Some(ch);
                i = copy_char(src, i, &mut out);
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            _ => i = copy_char(src, i, &mut out),
        }
    }

    out
}

/// Copy the whole UTF-8 char starting at byte `i` into `out`; return the byte
/// index just past it.
fn copy_char(src: &str, i: usize, out: &mut String) -> usize {
    let mut end = i + 1;
    while end < src.len() && !src.is_char_boundary(end) {
        end += 1;
    }
    out.push_str(&src[i..end]);
    end
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn locals(ms: &[ImportMapping]) -> Vec<&str> {
        ms.iter().map(|m| m.local_name.as_str()).collect()
    }

    #[test]
    fn js_default_named_alias_namespace_and_require() {
        let src = r#"
            import Foo from './foo';
            import { a, b as c } from './lib';
            import * as utils from './utils';
            import Def, { x } from './both';
            const fs = require('fs');
            const { readFile: rf, writeFile } = require('fs/promises');
        "#;
        let ms = extract_import_mappings("src/a.ts", src, Language::Typescript);

        let foo = ms.iter().find(|m| m.local_name == "Foo").unwrap();
        assert!(foo.is_default);
        assert_eq!(foo.exported_name, "default");
        assert_eq!(foo.source, "./foo");

        let c = ms.iter().find(|m| m.local_name == "c").unwrap();
        assert_eq!(
            c.exported_name, "b",
            "`b as c` binds local `c` to exported `b`"
        );
        assert!(!c.is_default && !c.is_namespace);

        let utils = ms.iter().find(|m| m.local_name == "utils").unwrap();
        assert!(utils.is_namespace);
        assert_eq!(utils.exported_name, "*");

        // A default + named on one line yields BOTH bindings.
        assert!(ms.iter().any(|m| m.local_name == "Def" && m.is_default));
        assert!(ms.iter().any(|m| m.local_name == "x" && !m.is_default));

        // require: default + destructure with a `:` rename.
        assert!(ms.iter().any(|m| m.local_name == "fs" && m.is_default));
        let rf = ms.iter().find(|m| m.local_name == "rf").unwrap();
        assert_eq!(rf.exported_name, "readFile");
        assert!(ms.iter().any(|m| m.local_name == "writeFile"));
    }

    #[test]
    fn python_from_import_and_plain_import() {
        let src = "\
from app.models import User, Post as Article\n\
from .utils import *\n\
import os\n\
import numpy as np\n\
import a.b.c\n";
        let ms = extract_import_mappings("src/a.py", src, Language::Python);

        let user = ms.iter().find(|m| m.local_name == "User").unwrap();
        assert_eq!(user.source, "app.models");
        let article = ms.iter().find(|m| m.local_name == "Article").unwrap();
        assert_eq!(article.exported_name, "Post");

        assert!(
            !ms.iter().any(|m| m.local_name == "*"),
            "a star import binds no name"
        );

        let np = ms.iter().find(|m| m.local_name == "np").unwrap();
        assert!(np.is_namespace);
        assert_eq!(np.source, "numpy");

        let abc = ms.iter().find(|m| m.source == "a.b.c").unwrap();
        assert_eq!(
            abc.local_name, "c",
            "an unaliased dotted import binds its LAST segment"
        );
    }

    #[test]
    fn go_single_and_block_imports_with_aliases() {
        let src = r#"
            import "fmt"

            import (
                "net/http"
                mylog "github.com/foo/logger"
            )
        "#;
        let ms = extract_import_mappings("main.go", src, Language::Go);
        assert!(ms.iter().any(|m| m.local_name == "fmt" && m.is_namespace));
        let http = ms.iter().find(|m| m.source == "net/http").unwrap();
        assert_eq!(
            http.local_name, "http",
            "the local name is the LAST path segment"
        );
        let logger = ms.iter().find(|m| m.local_name == "mylog").unwrap();
        assert_eq!(
            logger.source, "github.com/foo/logger",
            "the alias wins (#388)"
        );
    }

    #[test]
    fn jvm_imports_carry_the_fqn_and_skip_wildcards() {
        let src = "\
package com.example;\n\
// import com.commented.Out;\n\
/* import com.blocked.Out; */\n\
import com.example.dao.FooConverter;\n\
import com.example.*;\n\
import static com.example.Util.helper;\n";
        let ms = extract_import_mappings("Foo.java", src, Language::Java);

        assert_eq!(
            locals(&ms),
            vec!["FooConverter", "helper"],
            "commented-out imports are stripped; the `.*` wildcard binds no local name"
        );
        let fc = ms.iter().find(|m| m.local_name == "FooConverter").unwrap();
        assert_eq!(
            fc.source, "com.example.dao.FooConverter",
            "the FQN is the disambiguation signal when two packages declare the same class (#314)"
        );
        let h = ms.iter().find(|m| m.local_name == "helper").unwrap();
        assert_eq!(
            h.source, "com.example.Util.helper",
            "a static import binds the member"
        );
    }

    #[test]
    fn php_use_statements() {
        let src = "use App\\Models\\User;\nuse App\\Services\\Mailer as Post;\n";
        let ms = extract_import_mappings("a.php", src, Language::Php);
        let user = ms.iter().find(|m| m.local_name == "User").unwrap();
        assert_eq!(user.source, "App\\Models\\User");
        assert_eq!(user.exported_name, "User");
        let alias = ms.iter().find(|m| m.local_name == "Post").unwrap();
        assert_eq!(
            alias.exported_name, "Mailer",
            "the alias renames the binding"
        );
    }

    #[test]
    fn cpp_includes_bind_the_header_basename() {
        let src = "#include <vector>\n#include \"lib/my_class.hpp\"\n  #  include \"a.h\"\n";
        let ms = extract_import_mappings("a.cpp", src, Language::Cpp);
        assert_eq!(locals(&ms), vec!["vector", "my_class", "a"]);
        assert!(ms.iter().all(|m| m.is_namespace));
        assert_eq!(
            ms[1].source, "lib/my_class.hpp",
            "the source keeps the full path"
        );
    }

    #[test]
    fn ruby_has_no_symbol_imports() {
        let ms = extract_import_mappings("a.rb", "require 'json'\n", Language::Ruby);
        assert!(
            ms.is_empty(),
            "Ruby's `require` names a PATH, and the language has no binding form for a \
             foreign symbol — deliberately empty, not an oversight"
        );
    }

    // --- re-exports ---------------------------------------------------------

    #[test]
    fn re_exports_named_renamed_and_wildcard() {
        let src = r#"
            export { signIn as login, register } from './auth';
            export * from './types';
            export * as helpers from './helpers';
        "#;
        let out = extract_re_exports(src, "src/index.ts");

        assert!(out.contains(&ReExport::Named {
            exported_name: "login".into(),
            original_name: "signIn".into(),
            source: "./auth".into(),
        }));
        assert!(out.contains(&ReExport::Named {
            exported_name: "register".into(),
            original_name: "register".into(),
            source: "./auth".into(),
        }));
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, ReExport::Wildcard { .. }))
                .count(),
            2,
            "`export *` and `export * as ns` are both wildcards"
        );
    }

    /// #629: the parse is keyed on the BARREL's extension, not the consumer's
    /// language. A `.svelte` consumer importing through a `.ts` barrel must
    /// still see the barrel's re-exports.
    #[test]
    fn re_exports_are_keyed_to_the_barrels_own_extension() {
        let src = "export { default as Button } from './Button.svelte';";
        assert_eq!(extract_re_exports(src, "src/index.ts").len(), 1);
        assert_eq!(extract_re_exports(src, "src/index.d.ts").len(), 1);
        assert_eq!(extract_re_exports(src, "src/index.mjs").len(), 1);
        assert_eq!(extract_re_exports(src, "src/Index.ets").len(), 1);
        assert!(
            extract_re_exports(src, "src/index.py").is_empty(),
            "a non-JS barrel has no re-exports (the construct does not exist)"
        );
    }

    #[test]
    fn a_commented_out_re_export_is_not_a_phantom_edge() {
        let src = "\
// export { ghost } from './ghost';\n\
/* export { blocked } from './blocked'; */\n\
export { real } from './real';\n";
        let out = extract_re_exports(src, "index.ts");
        assert_eq!(out.len(), 1);
        assert!(
            matches!(&out[0], ReExport::Named { exported_name, .. } if exported_name == "real")
        );
    }

    /// The comment strip is string-AWARE. A regex that eats `//` inside a string
    /// literal truncates everything after a URL — and would silently drop every
    /// re-export declared below it.
    #[test]
    fn the_comment_strip_does_not_eat_urls_inside_strings() {
        let src = "\
const CDN = 'https://cdn.example.com/x';\n\
export { real } from './real';\n";
        let out = extract_re_exports(src, "index.ts");
        assert_eq!(
            out.len(),
            1,
            "the `//` inside the string is not a comment — a naive strip would \
             have eaten the rest of the file, including this re-export"
        );

        // And the strip itself preserves the string.
        let cleaned = strip_js_comments(src);
        assert!(cleaned.contains("https://cdn.example.com/x"));
    }

    #[test]
    fn the_comment_strip_handles_escapes_and_multibyte() {
        let cleaned =
            strip_js_comments("const s = \"a\\\"b\"; // gone\nconst é = '→'; /* gone */\n");
        assert!(cleaned.contains("a\\\"b"));
        assert!(cleaned.contains("é"));
        assert!(cleaned.contains('→'));
        assert!(!cleaned.contains("gone"));
    }
}
