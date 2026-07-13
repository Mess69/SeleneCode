#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 5 — `resolve_import_path`: a module specifier → the file it names.
//!
//! Every case here is one a real repo lost real edges over. The two directions
//! of failure are not symmetric: mis-classifying a LOCAL import as external
//! silently deletes every edge through that specifier, while mis-classifying an
//! external one merely wastes a lookup. That asymmetry is why the escapes
//! (workspace members, alias prefixes, Go's module path) all exist.

mod common;

use common::FakeContext;
use selene_core::Language;
use selene_resolve::{
    AliasMap, AliasPattern, GoModule, WorkspacePackages, is_external_import, resolve_import_path,
};

/// A context whose file index holds exactly `files`.
fn ctx_with_files(files: &[&str]) -> FakeContext {
    let mut ctx = FakeContext::new().with_root("/repo");
    for f in files {
        ctx = ctx.with_file(f, "");
    }
    ctx
}

// =============================================================================
// Relative imports
// =============================================================================

#[test]
fn a_relative_import_resolves_through_the_extension_list_in_order() {
    let ctx = ctx_with_files(&["src/a.ts", "src/util.ts", "src/lib/index.ts"]);

    assert_eq!(
        resolve_import_path("./util", "src/a.ts", Language::Typescript, &ctx).unwrap(),
        "src/util.ts"
    );
    assert_eq!(
        resolve_import_path("./lib", "src/a.ts", Language::Typescript, &ctx).unwrap(),
        "src/lib/index.ts",
        "a directory import falls through to its `/index.ts` barrel"
    );
    assert_eq!(
        resolve_import_path("../util", "src/deep/b.ts", Language::Typescript, &ctx).unwrap(),
        "src/util.ts",
        "`..` is normalized lexically, against the IMPORTING file's directory"
    );
    assert!(
        resolve_import_path("./ghost", "src/a.ts", Language::Typescript, &ctx).is_none(),
        "a specifier naming no file is a MISS, never a guess"
    );
}

/// The extension ORDER is a contract, not a detail: a repo with both `foo.ts`
/// and `foo/index.ts` must bind `./foo` to the file, exactly as `tsc` does.
#[test]
fn a_file_wins_over_a_same_named_barrel() {
    let ctx = ctx_with_files(&["src/a.ts", "src/foo.ts", "src/foo/index.ts"]);
    assert_eq!(
        resolve_import_path("./foo", "src/a.ts", Language::Typescript, &ctx).unwrap(),
        "src/foo.ts"
    );
}

#[test]
fn a_specifier_that_already_carries_its_extension_resolves_bare() {
    let ctx = ctx_with_files(&["src/a.js", "src/util.js"]);
    assert_eq!(
        resolve_import_path("./util.js", "src/a.js", Language::Javascript, &ctx).unwrap(),
        "src/util.js"
    );
}

/// Python's leading dots are PACKAGE LEVELS, not directory names. One dot is the
/// current package; two is the parent. A plain path join reads `.certs` as a
/// hidden filename and resolves nothing.
#[test]
fn python_dotted_relative_imports_translate_to_paths() {
    let ctx = ctx_with_files(&[
        "app/views/main.py",
        "app/views/helpers.py",
        "app/models/user.py",
        "app/models/__init__.py",
    ]);

    assert_eq!(
        resolve_import_path(".helpers", "app/views/main.py", Language::Python, &ctx).unwrap(),
        "app/views/helpers.py",
        "one dot = the current package"
    );
    assert_eq!(
        resolve_import_path("..models.user", "app/views/main.py", Language::Python, &ctx).unwrap(),
        "app/models/user.py",
        "two dots = the parent package, and the rest is a dotted submodule path"
    );
    assert_eq!(
        resolve_import_path("..models", "app/views/main.py", Language::Python, &ctx).unwrap(),
        "app/models/__init__.py",
        "a package resolves to its `__init__.py`"
    );
}

// =============================================================================
// External classification
// =============================================================================

#[test]
fn js_bare_specifiers_are_external_but_conventional_prefixes_are_not() {
    let ctx = ctx_with_files(&["src/a.ts"]);

    for external in ["react", "@scope/pkg", "fs", "path", "lodash/merge"] {
        assert!(
            is_external_import(external, Language::Typescript, &ctx),
            "{external} is npm/node — not a project file"
        );
    }
    for local in ["@/x", "~/x", "src/x", "./x"] {
        assert!(
            !is_external_import(local, Language::Typescript, &ctx),
            "{local} is a conventional local prefix"
        );
    }
}

/// A tsconfig alias prefix makes a bare-looking specifier LOCAL. Without this
/// escape, `@components/Foo` is classified npm and the edge never exists.
#[test]
fn a_declared_alias_prefix_is_not_external() {
    let aliases = AliasMap {
        base_url: "/repo".into(),
        patterns: vec![AliasPattern {
            prefix: "@components/".into(),
            suffix: String::new(),
            has_wildcard: true,
            replacements: vec!["src/components/*".into()],
        }],
    };
    let ctx = ctx_with_files(&["src/a.ts", "src/components/Button.tsx"]).with_aliases(aliases);

    assert!(
        !is_external_import("@components/Button", Language::Typescript, &ctx),
        "a declared alias prefix is local"
    );
    assert_eq!(
        resolve_import_path("@components/Button", "src/a.ts", Language::Typescript, &ctx).unwrap(),
        "src/components/Button.tsx",
        "…and it resolves through the alias map"
    );
    assert!(
        is_external_import("@other/thing", Language::Typescript, &ctx),
        "an undeclared scope is still npm"
    );
}

/// #629: a workspace member looks exactly like an npm specifier. Mis-classifying
/// it surfaced as a false "0 callers" on a live component.
#[test]
fn a_workspace_member_is_local_and_resolves_to_its_barrel() {
    let mut by_name = std::collections::HashMap::new();
    by_name.insert("@scope/ui".to_string(), "packages/ui".to_string());
    let ws = WorkspacePackages {
        by_name,
        entry_by_name: std::collections::HashMap::new(),
    };
    let ctx = ctx_with_files(&[
        "apps/web/a.ts",
        "packages/ui/index.ts",
        "packages/ui/widgets.ts",
    ])
    .with_workspace(ws);

    assert!(!is_external_import("@scope/ui", Language::Typescript, &ctx));
    assert_eq!(
        resolve_import_path("@scope/ui", "apps/web/a.ts", Language::Typescript, &ctx).unwrap(),
        "packages/ui/index.ts",
        "a bare member import lands on its barrel"
    );
    assert_eq!(
        resolve_import_path(
            "@scope/ui/widgets",
            "apps/web/a.ts",
            Language::Typescript,
            &ctx
        )
        .unwrap(),
        "packages/ui/widgets.ts",
        "a subpath import resolves inside the member"
    );
    assert!(
        is_external_import("@scope/other", Language::Typescript, &ctx),
        "a non-member scope stays npm"
    );
}

#[test]
fn python_stdlib_is_external_by_its_first_segment() {
    let ctx = ctx_with_files(&["app/main.py"]);
    assert!(is_external_import("os", Language::Python, &ctx));
    assert!(is_external_import("os.path", Language::Python, &ctx));
    assert!(is_external_import(
        "collections.abc",
        Language::Python,
        &ctx
    ));
    assert!(!is_external_import("app.models", Language::Python, &ctx));
}

/// #388: without the module path, every in-module Go import looks third-party
/// and cross-package calls do not resolve at all.
#[test]
fn go_locality_is_decided_by_the_module_path() {
    let ctx = ctx_with_files(&["main.go", "pkga/svc.go"]).with_go_module(GoModule {
        module_path: "github.com/example/myproject".into(),
        root_dir: String::new(),
    });

    assert!(
        !is_external_import("github.com/example/myproject/pkga", Language::Go, &ctx),
        "an in-module import is LOCAL"
    );
    assert!(
        is_external_import("github.com/other/lib", Language::Go, &ctx),
        "a third-party module is external"
    );
    assert!(
        is_external_import("fmt", Language::Go, &ctx),
        "the stdlib is external"
    );

    // The `internal/` escape hatch survives even with no go.mod at all.
    let no_mod = ctx_with_files(&["main.go"]);
    assert!(!is_external_import("x/internal/svc", Language::Go, &no_mod));
    assert!(is_external_import(
        "github.com/example/myproject/pkga",
        Language::Go,
        &no_mod
    ));
}

#[test]
fn c_and_cpp_system_headers_are_external_in_both_spellings() {
    let ctx = ctx_with_files(&["src/main.c", "src/util.h", "include/lib/api.hpp"]);

    assert!(is_external_import("stdio.h", Language::C, &ctx));
    assert!(is_external_import("vector", Language::Cpp, &ctx));
    assert!(is_external_import("cstdio", Language::Cpp, &ctx));
    assert!(
        is_external_import("string.h", Language::Cpp, &ctx),
        "the `.h`-stripped form is checked too"
    );
    assert!(!is_external_import("util.h", Language::C, &ctx));
    assert!(
        resolve_import_path("stdio.h", "src/main.c", Language::C, &ctx).is_none(),
        "a system header resolves to nothing — it is not our file"
    );
}

// =============================================================================
// C/C++ include resolution
// =============================================================================

#[test]
fn a_c_include_resolves_same_dir_then_subdir_then_include_dirs() {
    let ctx = ctx_with_files(&[
        "src/main.c",
        "src/util.h",
        "src/sub/deep.h",
        "include/lib/api.hpp",
    ])
    .with_cpp_include_dirs(vec!["include".into()]);

    assert_eq!(
        resolve_import_path("./util.h", "src/main.c", Language::C, &ctx).unwrap(),
        "src/util.h",
        "a same-dir sibling include"
    );
    assert_eq!(
        resolve_import_path("./sub/deep.h", "src/main.c", Language::C, &ctx).unwrap(),
        "src/sub/deep.h"
    );
    assert_eq!(
        resolve_import_path("lib/api.hpp", "src/main.c", Language::Cpp, &ctx).unwrap(),
        "include/lib/api.hpp",
        "the `-I` search path is the last resort"
    );
    assert!(resolve_import_path("lib/missing.hpp", "src/main.c", Language::Cpp, &ctx).is_none());
}

/// The include search is extension-permuting: `#include "lib/api"` finds
/// `lib/api.hpp` through the C++ extension list.
#[test]
fn an_include_dir_probe_permutes_extensions() {
    let ctx = ctx_with_files(&["src/main.cpp", "include/lib/api.hpp"])
        .with_cpp_include_dirs(vec!["include".into()]);
    assert_eq!(
        resolve_import_path("lib/api", "src/main.cpp", Language::Cpp, &ctx).unwrap(),
        "include/lib/api.hpp"
    );
}

// =============================================================================
// Other ecosystems
// =============================================================================

#[test]
fn rust_and_php_and_ruby_relative_specifiers() {
    let rust = ctx_with_files(&["src/main.rs", "src/util.rs", "src/deep/mod.rs"]);
    assert_eq!(
        resolve_import_path("./util", "src/main.rs", Language::Rust, &rust).unwrap(),
        "src/util.rs"
    );
    assert_eq!(
        resolve_import_path("./deep", "src/main.rs", Language::Rust, &rust).unwrap(),
        "src/deep/mod.rs",
        "a Rust module directory resolves to its `mod.rs`"
    );

    let php = ctx_with_files(&["index.php", "inc/db.php"]);
    assert_eq!(
        resolve_import_path("./inc/db.php", "index.php", Language::Php, &php).unwrap(),
        "inc/db.php"
    );
    assert_eq!(
        resolve_import_path("./inc/db", "index.php", Language::Php, &php).unwrap(),
        "inc/db.php",
        "an include that omits `.php` still resolves"
    );
}

/// Kotlin has NO extension row — deliberately, and matching the TS source and
/// the map. Its imports are FQNs, resolved by the JVM branch (Task 6), and a
/// `.kt` row here would create resolutions the parity gate would have to explain.
#[test]
fn kotlin_does_not_resolve_through_extension_probing() {
    let ctx = ctx_with_files(&["src/Main.kt", "src/Util.kt"]);
    assert!(
        resolve_import_path("./Util", "src/Main.kt", Language::Kotlin, &ctx).is_none(),
        "no extension row ⇒ no extension-probed resolution (Task 6's JVM branch \
         is what resolves a Kotlin import)"
    );
}
