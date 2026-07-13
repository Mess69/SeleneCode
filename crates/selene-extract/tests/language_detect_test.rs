//! Ported language-detection + generated-file contract tests: the
//! "Language Detection" describe block of `extraction.test.ts`, the
//! zero-config assertions of `extension-mapping.test.ts` (#906's overrides
//! machinery itself is NOT ported — `detect_language` has no overrides
//! parameter in v0), and `generated-detection.test.ts` verbatim.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_extract::{
    Language, MAX_FILE_SIZE, detect_language, is_file_level_only, is_generated_file, is_source_file,
};

/// Shorthand: detect with no content sniffing.
fn detect(path: &str) -> Language {
    detect_language(path, None)
}

// =============================================================================
// extraction.test.ts — describe('Language Detection')
// =============================================================================

#[test]
fn detects_typescript_files() {
    assert_eq!(detect("src/index.ts"), Language::Typescript);
    assert_eq!(detect("components/Button.tsx"), Language::Tsx);
}

#[test]
fn detects_javascript_files() {
    assert_eq!(detect("index.js"), Language::Javascript);
    assert_eq!(detect("App.jsx"), Language::Jsx);
    assert_eq!(detect("config.mjs"), Language::Javascript);
}

#[test]
fn detects_python_go_rust_java() {
    assert_eq!(detect("main.py"), Language::Python);
    assert_eq!(detect("main.go"), Language::Go);
    assert_eq!(detect("lib.rs"), Language::Rust);
    assert_eq!(detect("Main.java"), Language::Java);
}

#[test]
fn detects_c_files() {
    assert_eq!(detect("main.c"), Language::C);
    assert_eq!(detect("utils.h"), Language::C);
}

#[test]
fn detects_cpp_files() {
    assert_eq!(detect("main.cpp"), Language::Cpp);
    assert_eq!(detect("class.hpp"), Language::Cpp);
}

#[test]
fn detects_csharp_php_ruby_swift_dart() {
    assert_eq!(detect("Program.cs"), Language::CSharp);
    assert_eq!(detect("index.php"), Language::Php);
    assert_eq!(detect("app.rb"), Language::Ruby);
    assert_eq!(detect("ViewController.swift"), Language::Swift);
    assert_eq!(detect("main.dart"), Language::Dart);
}

#[test]
fn detects_kotlin_files() {
    assert_eq!(detect("MainActivity.kt"), Language::Kotlin);
    assert_eq!(detect("build.gradle.kts"), Language::Kotlin);
}

#[test]
fn detects_objective_c_files_including_h_sniff() {
    assert_eq!(detect("AppDelegate.m"), Language::Objc);
    assert_eq!(detect("ViewController.mm"), Language::Objc);
    let objc_header = "@interface Foo : NSObject\n@end\n";
    assert_eq!(detect_language("Foo.h", Some(objc_header)), Language::Objc);
    assert_eq!(
        detect_language("stdio.h", Some("#ifndef STDIO_H\nvoid printf();\n#endif\n")),
        Language::C
    );
}

#[test]
fn detects_metal_shaders_as_cpp() {
    // #1121
    assert_eq!(detect("Shaders.metal"), Language::Cpp);
    assert!(is_source_file("Renderer/Shaders.metal"));
}

#[test]
fn detects_cuda_as_cpp() {
    // #387
    assert_eq!(detect("kernels/scan.cu"), Language::Cpp);
    assert_eq!(detect("include/reduce.cuh"), Language::Cpp);
    assert!(is_source_file("csrc/flash_attn/softmax.cu"));
    assert!(is_source_file("include/block_reduce.cuh"));
}

#[test]
fn detects_erlang_files_and_app_resources() {
    assert_eq!(detect("src/my_server.erl"), Language::Erlang);
    assert_eq!(detect("include/records.hrl"), Language::Erlang);
    assert_eq!(detect("bin/release_tool.escript"), Language::Erlang);
    // OTP app resource files route by full suffix — `.src` alone is too generic.
    assert_eq!(detect("src/myapp.app.src"), Language::Erlang);
    assert_eq!(detect("ebin/myapp.app"), Language::Erlang);
    assert_eq!(detect("legacy/module.src"), Language::Unknown);
    assert!(is_source_file("src/myapp.app.src"));
    assert!(is_source_file("ebin/myapp.app"));
    assert!(!is_source_file("legacy/module.src"));
}

#[test]
fn detects_solidity_and_terraform() {
    assert_eq!(detect("contracts/Vault.sol"), Language::Solidity);
    assert_eq!(detect("main.tf"), Language::Terraform);
    assert_eq!(detect("variables.tf"), Language::Terraform);
    assert_eq!(detect("terraform.tfvars"), Language::Terraform);
    assert_eq!(detect("versions.tofu"), Language::Terraform);
}

#[test]
fn detects_arkts_but_plain_ts_stays_typescript() {
    assert_eq!(
        detect("entry/src/main/ets/pages/Index.ets"),
        Language::Arkts
    );
    assert_eq!(
        detect("entry/src/main/ets/common/utils.ts"),
        Language::Typescript
    );
}

#[test]
fn detects_nix_files() {
    assert_eq!(detect("default.nix"), Language::Nix);
    assert_eq!(
        detect("pkgs/development/tools/misc/codegraph/default.nix"),
        Language::Nix
    );
    assert!(is_source_file("default.nix"));
}

#[test]
fn detects_export_macro_class_headers_as_cpp() {
    // Lean Unreal-Engine style header: the class is annotated with an export
    // macro and carries no explicit `public:`/`virtual`/`namespace`/`template`
    // — the macro-blind `class\s+\w+\s*[:{]` branch alone can't see it.
    // (#1093 follow-up)
    let macro_class_header = "#pragma once\n#include \"CoreMinimal.h\"\n\nUCLASS()\nclass ENGINE_API UNetConnectionRepControl : public UObject\n{\n\tGENERATED_BODY()\n\tbool IsRepControlEnable() const;\n};\n";
    assert_eq!(
        detect_language("NetConnectionRepControl.h", Some(macro_class_header)),
        Language::Cpp
    );
    // Macro class with no base clause, brace on the next line, still C++.
    assert_eq!(
        detect_language(
            "Foo.h",
            Some("MYMODULE_API_DECL\nclass MYMODULE_API FFoo\n{\n\tint X;\n};\n")
        ),
        Language::Cpp
    );
    // Export-macro struct with inheritance is likewise C++-only.
    assert_eq!(
        detect_language("Bar.h", Some("struct ENGINE_API FBar : public FBase {};\n")),
        Language::Cpp
    );
    // Guard: a genuine C header must NOT be dragged to C++ by the macro branch.
    assert_eq!(
        detect_language(
            "cfoo.h",
            Some(
                "#ifndef CFOO_H\nstruct Point { int x; int y; };\nvoid f(struct Point p);\n#endif\n"
            )
        ),
        Language::C
    );
}

#[test]
fn unknown_for_unsupported_extensions() {
    assert_eq!(detect("styles.css"), Language::Unknown);
    assert_eq!(detect("data.json"), Language::Unknown);
}

// =============================================================================
// Special extension-less / full-suffix routes (map §6)
// =============================================================================

#[test]
fn play_routes_files_route_to_yaml() {
    assert_eq!(detect("conf/routes"), Language::Yaml);
    assert_eq!(detect("service/conf/routes"), Language::Yaml);
    assert_eq!(detect("conf/api.routes"), Language::Yaml);
    assert!(is_source_file("conf/routes"));
    assert!(is_source_file("service/conf/routes"));
}

#[test]
fn shopify_json_templates_route_to_liquid() {
    assert_eq!(detect("templates/product.json"), Language::Liquid);
    assert_eq!(detect("sections/header-group.json"), Language::Liquid);
    // Nested template dirs are allowed, and matching is case-insensitive.
    assert_eq!(detect("templates/customers/login.json"), Language::Liquid);
    assert_eq!(detect("theme/Templates/product.JSON"), Language::Liquid);
    assert!(is_source_file("templates/product.json"));
    // config/ + locales/ JSON have no section refs → not liquid, not source.
    assert_eq!(detect("config/settings_schema.json"), Language::Unknown);
    assert!(!is_source_file("config/settings_schema.json"));
}

// =============================================================================
// extension-mapping.test.ts — zero-config assertions only (#906 overrides
// machinery not ported; detect_language has no overrides parameter in v0)
// =============================================================================

#[test]
fn zero_config_detection_and_source_selection() {
    assert_eq!(detect("a/b.foo"), Language::Unknown);
    assert!(!is_source_file("a/b.foo"));
    assert_eq!(detect("x.h"), Language::C);
    assert_eq!(detect("x.ts"), Language::Typescript);
    assert_eq!(detect("x.py"), Language::Python);
    assert!(is_source_file("x.ts"));
    assert!(!is_source_file("x.unknownext"));
    assert!(!is_source_file("no_extension_file"));
}

// =============================================================================
// File-level-only set + constants
// =============================================================================

#[test]
fn file_level_only_set_is_yaml_twig_properties() {
    assert!(is_file_level_only(Language::Yaml));
    assert!(is_file_level_only(Language::Twig));
    assert!(is_file_level_only(Language::Properties));
    assert!(!is_file_level_only(Language::Typescript));
    assert!(!is_file_level_only(Language::Xml));
    assert!(!is_file_level_only(Language::Unknown));
}

#[test]
fn max_file_size_is_one_mib() {
    assert_eq!(MAX_FILE_SIZE, 1024 * 1024);
}

// =============================================================================
// generated-detection.test.ts — verbatim port
// =============================================================================

#[test]
fn classifies_go_protobuf_grpc_pulsar_mock_outputs_as_generated() {
    assert!(is_generated_file("api/cosmos/bank/v1beta1/tx_grpc.pb.go"));
    assert!(is_generated_file("x/bank/types/tx.pb.go"));
    assert!(is_generated_file("api/cosmos/bank/v1beta1/tx.pulsar.go"));
    // cosmos-sdk uses `<base>_mocks.go`; mockgen's default is `mock_<src>.go`;
    // many projects use `<base>_mock.go`. All three are mockgen output.
    assert!(is_generated_file(
        "x/auth/testutil/expected_keepers_mocks.go"
    ));
    assert!(is_generated_file("internal/foo_mock.go"));
    assert!(is_generated_file("mock_keeper.go"));
}

#[test]
fn does_not_flag_hand_written_keepers_as_generated() {
    assert!(!is_generated_file("x/bank/keeper/msg_server.go"));
    assert!(!is_generated_file("x/bank/keeper/send.go"));
}

#[test]
fn catches_common_cross_language_codegen_suffixes() {
    assert!(is_generated_file("app/foo.generated.ts"));
    assert!(is_generated_file("app/foo.generated.tsx"));
    assert!(is_generated_file("proto/bar_pb2.py"));
    assert!(is_generated_file("proto/bar_pb2_grpc.py"));
    assert!(is_generated_file("lib/baz.pb.cc"));
    assert!(is_generated_file("lib/baz.pb.h"));
    assert!(is_generated_file("lib/quux.g.dart"));
    assert!(is_generated_file("lib/quux.freezed.dart"));
}

#[test]
fn leaves_ordinary_source_files_alone() {
    assert!(!is_generated_file("src/index.ts"));
    assert!(!is_generated_file("src/components/Foo.tsx"));
    assert!(!is_generated_file("lib/main.dart"));
    assert!(!is_generated_file("cmd/server/main.go"));
    assert!(!is_generated_file("app/db.py"));
}

// =============================================================================
// Wire strings
// =============================================================================

#[test]
fn language_wire_strings_are_lowercase() {
    assert_eq!(Language::Typescript.as_str(), "typescript");
    assert_eq!(Language::Tsx.as_str(), "tsx");
    assert_eq!(Language::CSharp.as_str(), "csharp");
    assert_eq!(Language::Objc.as_str(), "objc");
    assert_eq!(Language::Vbnet.as_str(), "vbnet");
    assert_eq!(Language::Arkts.as_str(), "arkts");
    assert_eq!(Language::Unknown.as_str(), "unknown");
}
