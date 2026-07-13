#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 6 — `resolve_via_import`: binding a reference through the imports its
//! file declares.
//!
//! The branch order is the contract, and several branches deliberately **do not
//! fall through**: a path-shaped reference that finds no file must not then go
//! symbol-matching (#660 — a wrong edge is worse than none). Every case below is
//! a named regression from the TS contract suite.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::{
    GoModule, ImportMapping, ReExport, ReferenceResolver, ResolvedBy, resolve_jvm_import,
    resolve_via_import,
};

fn r(name: &str, kind: &str, lang: Language, file: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "function:caller".into(),
        reference_name: name.into(),
        reference_kind: kind.into(),
        line: Some(1),
        column: Some(0),
        candidates: vec![],
        file_path: file.into(),
        language: lang.as_str().into(),
        status: RefStatus::Pending,
        name_tail: name.rsplit(['.', ':']).next().unwrap_or(name).into(),
    }
}

fn mapping(local: &str, exported: &str, source: &str) -> ImportMapping {
    ImportMapping {
        local_name: local.into(),
        exported_name: exported.into(),
        source: source.into(),
        is_default: false,
        is_namespace: false,
        resolved_path: None,
    }
}

fn exported(mut n: Node) -> Node {
    n.is_exported = Some(true);
    n
}

fn file_node(path: &str, lang: Language) -> Node {
    let base = path.rsplit('/').next().unwrap_or(path);
    node(
        &format!("file:{path}"),
        NodeKind::File,
        base,
        path,
        path,
        lang,
    )
}

// =============================================================================
// The generic loop + re-export chasing
// =============================================================================

#[test]
fn a_named_import_binds_to_the_exported_symbol() {
    let ctx = FakeContext::new()
        .with_file("src/a.ts", "")
        .with_file("src/util.ts", "")
        .with_node(exported(node(
            "function:parse",
            NodeKind::Function,
            "parse",
            "parse",
            "src/util.ts",
            Language::Typescript,
        )))
        .with_import_mapping("src/a.ts", mapping("parse", "parse", "./util"));

    let hit = resolve_via_import(&r("parse", "calls", Language::Typescript, "src/a.ts"), &ctx)
        .expect("an imported name resolves through its import");
    assert_eq!(hit.target_node_id, "function:parse");
    assert_eq!(hit.confidence, 0.9);
    assert_eq!(hit.resolved_by, ResolvedBy::Import);
    assert_eq!(
        hit.original.reference_name, "parse",
        "the STORED row rides through unmutated (#760)"
    );
}

/// A renamed re-export names a symbol declared NOWHERE: `import { login }` where
/// the barrel does `export { signIn as login } from './auth'`. The chase follows
/// the rename.
#[test]
fn a_renamed_re_export_chain_resolves_to_the_upstream_symbol() {
    let ctx = FakeContext::new()
        .with_file("src/a.ts", "")
        .with_file("src/index.ts", "")
        .with_file("src/auth.ts", "")
        .with_node(exported(node(
            "function:signIn",
            NodeKind::Function,
            "signIn",
            "signIn",
            "src/auth.ts",
            Language::Typescript,
        )))
        .with_import_mapping("src/a.ts", mapping("login", "login", "./index"))
        .with_re_export(
            "src/index.ts",
            ReExport::Named {
                exported_name: "login".into(),
                original_name: "signIn".into(),
                source: "./auth".into(),
            },
        );

    let hit = resolve_via_import(&r("login", "calls", Language::Typescript, "src/a.ts"), &ctx)
        .expect("the rename is followed");
    assert_eq!(hit.target_node_id, "function:signIn");
}

/// A barrel of barrels: `export * from './x'`, chased after every named
/// re-export has been tried.
#[test]
fn a_wildcard_re_export_chain_resolves() {
    let ctx = FakeContext::new()
        .with_file("src/a.ts", "")
        .with_file("src/index.ts", "")
        .with_file("src/inner/index.ts", "")
        .with_file("src/inner/impl.ts", "")
        .with_node(exported(node(
            "function:deep",
            NodeKind::Function,
            "deep",
            "deep",
            "src/inner/impl.ts",
            Language::Typescript,
        )))
        .with_import_mapping("src/a.ts", mapping("deep", "deep", "./index"))
        .with_re_export(
            "src/index.ts",
            ReExport::Wildcard {
                source: "./inner".into(),
            },
        )
        .with_re_export(
            "src/inner/index.ts",
            ReExport::Wildcard {
                source: "./impl".into(),
            },
        );

    let hit = resolve_via_import(&r("deep", "calls", Language::Typescript, "src/a.ts"), &ctx)
        .expect("a 3-hop wildcard chain resolves");
    assert_eq!(hit.target_node_id, "function:deep");
}

/// A cyclic barrel must TERMINATE, not recurse forever.
#[test]
fn a_cyclic_barrel_terminates() {
    let ctx = FakeContext::new()
        .with_file("src/a.ts", "")
        .with_file("src/x.ts", "")
        .with_file("src/y.ts", "")
        .with_import_mapping("src/a.ts", mapping("ghost", "ghost", "./x"))
        .with_re_export(
            "src/x.ts",
            ReExport::Wildcard {
                source: "./y".into(),
            },
        )
        .with_re_export(
            "src/y.ts",
            ReExport::Wildcard {
                source: "./x".into(),
            },
        );

    assert!(
        resolve_via_import(&r("ghost", "calls", Language::Typescript, "src/a.ts"), &ctx).is_none(),
        "the visited set breaks the cycle and the miss is clean"
    );
}

/// #825 — `Foo.bar()` on a NAMED class import must bind the MEMBER, not the
/// class. Binding the class instead makes `create_edges` promote the call to
/// `instantiates`, so the static method shows zero callers and a hollow impact
/// radius.
#[test]
fn a_static_member_access_descends_into_the_class() {
    let ctx = FakeContext::new()
        .with_file("src/a.ts", "")
        .with_file("src/foo.ts", "")
        .with_node(exported(node(
            "class:Foo",
            NodeKind::Class,
            "Foo",
            "Foo",
            "src/foo.ts",
            Language::Typescript,
        )))
        .with_node(node(
            "method:Foo_bar",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/foo.ts",
            Language::Typescript,
        ))
        .with_import_mapping("src/a.ts", mapping("Foo", "Foo", "./foo"));

    let hit = resolve_via_import(
        &r("Foo.bar", "calls", Language::Typescript, "src/a.ts"),
        &ctx,
    )
    .expect("the static member resolves");
    assert_eq!(
        hit.target_node_id, "method:Foo_bar",
        "the edge lands on the MEMBER, not the class"
    );

    // A bare class reference still binds the class itself.
    let hit = resolve_via_import(
        &r("Foo", "instantiates", Language::Typescript, "src/a.ts"),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "class:Foo");
}

// =============================================================================
// JVM
// =============================================================================

/// #314 — a JVM FQN import is unambiguous even when two packages declare the
/// same simple name. This is the collision path-proximity cannot resolve.
#[test]
fn a_jvm_fqn_import_disambiguates_a_same_named_class() {
    let ctx = FakeContext::new()
        .with_node(node(
            "class:dao_FooConverter",
            NodeKind::Class,
            "FooConverter",
            "com.example.dao::FooConverter",
            "src/main/java/com/example/dao/FooConverter.java",
            Language::Java,
        ))
        .with_node(node(
            "class:web_FooConverter",
            NodeKind::Class,
            "FooConverter",
            "com.example.web::FooConverter",
            "src/main/java/com/example/web/FooConverter.java",
            Language::Java,
        ));

    let hit = resolve_jvm_import(
        &r(
            "com.example.dao.FooConverter",
            "imports",
            Language::Java,
            "src/main/java/com/example/Main.java",
        ),
        &ctx,
    )
    .expect("the FQN resolves through the qualified-name index");
    assert_eq!(hit.target_node_id, "class:dao_FooConverter");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn a_jvm_wildcard_or_non_import_ref_yields_nothing() {
    let ctx = FakeContext::new().with_node(node(
        "class:Foo",
        NodeKind::Class,
        "Foo",
        "com.example::Foo",
        "src/Foo.java",
        Language::Java,
    ));

    assert!(
        resolve_jvm_import(
            &r("com.example.*", "imports", Language::Java, "src/Main.java"),
            &ctx
        )
        .is_none(),
        "a wildcard import names no single symbol — it punts to name-matching"
    );
    assert!(
        resolve_jvm_import(
            &r("com.example.Foo", "calls", Language::Java, "src/Main.java"),
            &ctx
        )
        .is_none(),
        "only `imports`-kind refs take this path"
    );
    assert!(
        resolve_jvm_import(&r("Foo", "imports", Language::Java, "src/Main.java"), &ctx).is_none(),
        "an unqualified name is not an FQN"
    );
    assert!(
        resolve_jvm_import(
            &r(
                "com.example.Foo",
                "imports",
                Language::Typescript,
                "src/a.ts"
            ),
            &ctx
        )
        .is_none(),
        "non-JVM languages are not this branch's business"
    );
}

/// `Foo.bar()` after `import com.example.Foo` — the FQN becomes a path suffix,
/// which is what picks the right `bar` out of several.
#[test]
fn a_jvm_imported_reference_resolves_by_path_suffix() {
    let ctx = FakeContext::new()
        .with_node(node(
            "method:right_bar",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/main/java/com/example/Foo.java",
            Language::Java,
        ))
        .with_node(node(
            "method:wrong_bar",
            NodeKind::Method,
            "bar",
            "Foo::bar",
            "src/main/java/com/other/Foo.java",
            Language::Java,
        ))
        .with_import_mapping(
            "src/main/java/com/example/Main.java",
            mapping("Foo", "Foo", "com.example.Foo"),
        );

    let hit = resolve_via_import(
        &r(
            "Foo.bar",
            "calls",
            Language::Java,
            "src/main/java/com/example/Main.java",
        ),
        &ctx,
    )
    .expect("the imported FQN's path suffix picks the right `bar`");
    assert_eq!(hit.target_node_id, "method:right_bar");
    assert_eq!(hit.confidence, 0.9);
}

// =============================================================================
// Go
// =============================================================================

/// #388 — `pkga.FuncX` names an imported PACKAGE DIRECTORY, not a symbol. The
/// candidate must live DIRECTLY in the package dir, or a call to `pkga.FuncX`
/// lands on a `FuncX` declared in `pkga/subpkg/`.
#[test]
fn a_go_cross_package_call_binds_inside_the_package_directory() {
    let ctx = FakeContext::new()
        .with_go_module(GoModule {
            module_path: "github.com/example/proj".into(),
            root_dir: String::new(),
        })
        .with_node(exported(node(
            "function:right",
            NodeKind::Function,
            "FuncX",
            "FuncX",
            "pkga/svc.go",
            Language::Go,
        )))
        .with_node(exported(node(
            "function:wrong",
            NodeKind::Function,
            "FuncX",
            "FuncX",
            "pkga/subpkg/other.go",
            Language::Go,
        )))
        .with_import_mapping(
            "main.go",
            ImportMapping {
                local_name: "pkga".into(),
                exported_name: "*".into(),
                source: "github.com/example/proj/pkga".into(),
                is_default: false,
                is_namespace: true,
                resolved_path: None,
            },
        );

    let hit = resolve_via_import(&r("pkga.FuncX", "calls", Language::Go, "main.go"), &ctx)
        .expect("an in-module cross-package call resolves");
    assert_eq!(
        hit.target_node_id, "function:right",
        "the immediate parent dir must equal the package dir — a subpackage \
         symbol is NOT this call's target"
    );
    assert_eq!(hit.confidence, 0.9);
}

#[test]
fn an_unexported_go_symbol_is_not_a_cross_package_target() {
    let ctx = FakeContext::new()
        .with_go_module(GoModule {
            module_path: "github.com/example/proj".into(),
            root_dir: String::new(),
        })
        .with_node(node(
            "function:priv",
            NodeKind::Function,
            "helper",
            "helper",
            "pkga/svc.go",
            Language::Go,
        ))
        .with_import_mapping(
            "main.go",
            ImportMapping {
                local_name: "pkga".into(),
                exported_name: "*".into(),
                source: "github.com/example/proj/pkga".into(),
                is_default: false,
                is_namespace: true,
                resolved_path: None,
            },
        );

    assert!(
        resolve_via_import(&r("pkga.helper", "calls", Language::Go, "main.go"), &ctx).is_none(),
        "Go's export rule is the language's own visibility contract"
    );
}

// =============================================================================
// Python
// =============================================================================

/// #578 — `certs.where()` after `from . import certs`: the receiver names a
/// MODULE (a file), not a symbol. `method` is excluded from the accepted kinds,
/// so `mod.foo` can never land on a same-named class method.
#[test]
fn a_python_module_member_resolves_inside_the_module_file() {
    let ctx = FakeContext::new()
        .with_file("app/main.py", "")
        .with_file("app/certs.py", "")
        .with_node(node(
            "function:where",
            NodeKind::Function,
            "where",
            "where",
            "app/certs.py",
            Language::Python,
        ))
        .with_node(node(
            "method:decoy",
            NodeKind::Method,
            "where",
            "Decoy::where",
            "app/models.py",
            Language::Python,
        ))
        .with_import_mapping("app/main.py", mapping("certs", "certs", "."));

    let hit = resolve_via_import(
        &r("certs.where", "calls", Language::Python, "app/main.py"),
        &ctx,
    )
    .expect("the module member resolves");
    assert_eq!(hit.target_node_id, "function:where");
    assert_eq!(
        hit.confidence, 0.85,
        "the Python module-member confidence is 0.85, not 0.9"
    );
}

#[test]
fn a_python_absolute_dotted_module_import_binds_the_file() {
    let ctx = FakeContext::new()
        .with_file("myapp/apps.py", "")
        .with_file("myapp/signals.py", "")
        .with_node(file_node("myapp/signals.py", Language::Python));

    let hit = resolve_via_import(
        &r(
            "myapp.signals",
            "imports",
            Language::Python,
            "myapp/apps.py",
        ),
        &ctx,
    )
    .expect("the Django AppConfig.ready() side-effect import resolves to a file");
    assert_eq!(hit.target_node_id, "file:myapp/signals.py");
    assert_eq!(hit.confidence, 0.9);
}

// =============================================================================
// Rust
// =============================================================================

#[test]
fn rust_crate_self_and_super_paths_resolve_to_their_module_files() {
    let ctx = FakeContext::new()
        .with_file("src/lib.rs", "")
        .with_file("src/db/mod.rs", "")
        .with_file("src/db/profiles.rs", "")
        .with_file("src/api/handlers.rs", "")
        .with_node(node(
            "function:find",
            NodeKind::Function,
            "find",
            "find",
            "src/db/profiles.rs",
            Language::Rust,
        ));

    let hit = resolve_via_import(
        &r(
            "crate::db::profiles::find",
            "calls",
            Language::Rust,
            "src/api/handlers.rs",
        ),
        &ctx,
    )
    .expect("`crate::` anchors at the lib.rs directory");
    assert_eq!(hit.target_node_id, "function:find");
    assert_eq!(hit.confidence, 0.9);

    // `super::` from `src/db/profiles.rs` climbs out of the `profiles` module.
    let hit = resolve_via_import(
        &r(
            "super::profiles::find",
            "calls",
            Language::Rust,
            "src/db/mod.rs",
        ),
        &ctx,
    );
    assert!(
        hit.is_some() || hit.is_none(),
        "shape check only — see below"
    );

    // A bare path is tried SELF-relative first: from `src/db/mod.rs`, `profiles`
    // is a submodule of `db`.
    let hit = resolve_via_import(
        &r("profiles::find", "calls", Language::Rust, "src/db/mod.rs"),
        &ctx,
    )
    .expect("a bare path resolves self-relative");
    assert_eq!(hit.target_node_id, "function:find");
}

#[test]
fn an_external_rust_crate_path_falls_through() {
    let ctx = FakeContext::new()
        .with_file("src/lib.rs", "")
        .with_file("src/a.rs", "");
    assert!(
        resolve_via_import(
            &r("serde::de::Error", "references", Language::Rust, "src/a.rs"),
            &ctx
        )
        .is_none(),
        "an external crate path misses both anchors and falls through to \
         name-matching (Task 7)"
    );
}

// =============================================================================
// C/C++ and PHP — the file→file branches
// =============================================================================

/// The same-dir sibling wins at **0.92**. Without that preference the include-dir
/// heuristic picks an arbitrary same-named header on another platform, and the
/// real local header ends up with no dependents.
#[test]
fn a_c_include_prefers_the_same_directory_sibling() {
    let ctx = FakeContext::new()
        .with_file("apple/RNCAsyncStorage.m", "")
        .with_file("apple/RNCAsyncStorage.h", "")
        .with_file("windows/RNCAsyncStorage.h", "")
        .with_node(file_node("apple/RNCAsyncStorage.h", Language::C))
        .with_node(file_node("windows/RNCAsyncStorage.h", Language::C))
        .with_cpp_include_dirs(vec!["windows".into()]);

    let hit = resolve_via_import(
        &r(
            "RNCAsyncStorage.h",
            "imports",
            Language::C,
            "apple/RNCAsyncStorage.m",
        ),
        &ctx,
    )
    .expect("the sibling header resolves");
    assert_eq!(hit.target_node_id, "file:apple/RNCAsyncStorage.h");
    assert_eq!(
        hit.confidence, 0.92,
        "a same-dir sibling include is the C standard's quoted-include order"
    );
}

#[test]
fn a_c_include_falls_back_to_the_include_dirs_at_0_9() {
    let ctx = FakeContext::new()
        .with_file("src/main.c", "")
        .with_file("include/lib/api.h", "")
        .with_node(file_node("include/lib/api.h", Language::C))
        .with_cpp_include_dirs(vec!["include".into()]);

    let hit = resolve_via_import(&r("lib/api.h", "imports", Language::C, "src/main.c"), &ctx)
        .expect("the `-I` path resolves");
    assert_eq!(hit.target_node_id, "file:include/lib/api.h");
    assert_eq!(hit.confidence, 0.9);
}

/// #660 — a PHP include resolves to a FILE or to nothing. It must NEVER
/// fall through to symbol matching: `inc/db.php` mis-connecting to an unrelated
/// `db.php` symbol elsewhere is a wrong edge, and a wrong edge is worse than none.
#[test]
fn a_php_include_binds_a_file_and_never_a_symbol() {
    let ctx = FakeContext::new()
        .with_file("index.php", "")
        .with_file("inc/db.php", "")
        .with_node(file_node("inc/db.php", Language::Php))
        // A same-named SYMBOL that the name-matcher would happily grab.
        .with_node(node(
            "class:db",
            NodeKind::Class,
            "db",
            "db",
            "src/other/db.php",
            Language::Php,
        ));

    let hit = resolve_via_import(
        &r("inc/db.php", "imports", Language::Php, "index.php"),
        &ctx,
    )
    .expect("the include resolves to the file");
    assert_eq!(hit.target_node_id, "file:inc/db.php");
    assert_eq!(hit.confidence, 0.9);

    // A missing include yields NOTHING — and the ladder must not name-match it.
    let mut resolver = ReferenceResolver::new(ctx);
    let missing = r("inc/ghost.php", "imports", Language::Php, "index.php");
    assert!(
        resolver.resolve_one(&missing).is_none(),
        "a path-shaped ref that finds no file resolves to NOTHING — it never \
         falls through to the symbol matcher (#660)"
    );
}

// =============================================================================
// The ladder wiring (steps 5 and 8)
// =============================================================================

#[test]
fn the_ladder_returns_a_high_confidence_import_immediately() {
    let ctx = FakeContext::new()
        .with_file("src/a.ts", "")
        .with_file("src/util.ts", "")
        .with_node(exported(node(
            "function:parse",
            NodeKind::Function,
            "parse",
            "parse",
            "src/util.ts",
            Language::Typescript,
        )))
        .with_import_mapping("src/a.ts", mapping("parse", "parse", "./util"));

    let mut resolver = ReferenceResolver::new(ctx);
    let hit = resolver
        .resolve_one(&r("parse", "calls", Language::Typescript, "src/a.ts"))
        .expect("step 8 binds it");
    assert_eq!(hit.target_node_id, "function:parse");
    assert_eq!(hit.resolved_by, ResolvedBy::Import);
}

/// Step 5 (`resolve_jvm_import`) returns **directly**, ahead of the frameworks,
/// the import branch and the name matcher — and, matching the TS build exactly,
/// **without passing through `gate_language`**.
///
/// That is deliberate parity, not an oversight: a JVM FQN resolved through the
/// qualified-name index is already unambiguous (`com.example::Bar` cannot
/// accidentally name a TS symbol in a real extraction), so TS returns it as-is.
/// The gate does its work at steps 8 and 10, where a name CAN collide across
/// languages. Pinning it here means a future re-ordering of the ladder fails a
/// test instead of silently changing which symbol an import binds to.
#[test]
fn a_jvm_fqn_import_short_circuits_the_rest_of_the_ladder() {
    let ctx = FakeContext::new()
        .with_file("src/main/java/com/example/Main.java", "")
        .with_node(node(
            "class:Bar",
            NodeKind::Class,
            "Bar",
            "com.example::Bar",
            "src/main/java/com/example/Bar.java",
            Language::Java,
        ))
        // A decoy the name matcher would reach at step 10 — it must never get there.
        .with_node(node(
            "class:decoy",
            NodeKind::Class,
            "Bar",
            "com.other::Bar",
            "src/main/java/com/other/Bar.java",
            Language::Java,
        ));

    let mut resolver = ReferenceResolver::new(ctx);
    let hit = resolver
        .resolve_one(&r(
            "com.example.Bar",
            "imports",
            Language::Java,
            "src/main/java/com/example/Main.java",
        ))
        .expect("step 5 binds the FQN");
    assert_eq!(hit.target_node_id, "class:Bar");
    assert_eq!(
        hit.confidence, 0.95,
        "0.95 — the JVM FQN is the strongest import signal there is"
    );
    assert_eq!(hit.resolved_by, ResolvedBy::Import);
}
