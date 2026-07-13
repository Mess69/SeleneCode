//! Language registry + detection: the FULL `EXTENSION_MAP` ported verbatim
//! from `extraction-langs.md` §Wire ("the single source of truth for
//! indexability" — drifting it changes which files are counted), the
//! detection order of map §6, and the `.h` content sniffers.
//!
//! v0 registers grammars for 13 of these languages (Task 4); every other
//! (wave-2) language **detects** here but extraction returns an
//! `unsupported_language` warning + skip — matching the TS missing-grammar
//! semantics. Detection stays total so file counts and language stats are
//! stable across waves.
//!
//! **`Language` itself now lives in `selene-core`** (decision D1) and is
//! re-exported here. What stays is extraction POLICY: the `EXTENSION_MAP`, the
//! detection order, the `.h` sniffers, and `is_file_level_only` — which files we
//! index and which of them yield symbol nodes.
//!
//! Deviation from TS (documented): `detectLanguage`/`isSourceFile` carried an
//! `overrides` parameter for `codegraph.json` custom extension maps (#906).
//! v0 has no project-config loader yet, so the Rust signatures omit it — the
//! zero-config path is byte-identical (pinned by
//! `tests/language_detect_test.rs`).

use std::sync::LazyLock;

use regex::Regex;
// The `Language` enum itself lives in `selene-core` (decision D1, 2026-07-13):
// it is a shared WIRE type — resolution gates on it, the framework registry keys
// on it, the store persists it — and keeping it here would force
// `selene-resolve` to depend on `selene-extract` (backwards layering, and a
// cycle once frameworks emit nodes). Re-exported so every `selene_extract::
// Language` path keeps working unchanged.
pub use selene_core::Language;

/// The FULL `EXTENSION_MAP`, ported verbatim (extraction-langs.md §Wire;
/// see the TS source's per-extension rationale comments — `.mts/.cts` #366,
/// `.ets` ArkTS #648, `.xsjs` SAP HANA #556, Drupal `.module/.install/
/// .theme/.inc`, `.metal` ≈ C++14 #1121, `.cu/.cuh` CUDA #387, …). A match
/// (not a map structure) so it is compile-time exhaustive-checked and
/// allocation-free; `is_source_file` derives from this same function, so
/// parser support and indexing selection can never drift apart.
fn language_for_extension(ext: &str) -> Option<Language> {
    Some(match ext {
        ".ts" | ".mts" | ".cts" => Language::Typescript,
        ".tsx" => Language::Tsx,
        ".ets" => Language::Arkts,
        ".js" | ".mjs" | ".cjs" | ".xsjs" | ".xsjslib" => Language::Javascript,
        ".jsx" => Language::Jsx,
        ".py" | ".pyw" => Language::Python,
        ".go" => Language::Go,
        ".rs" => Language::Rust,
        ".java" => Language::Java,
        // `.h` could also be C++ or ObjC — content-sniffed in detect_language.
        ".c" | ".h" => Language::C,
        ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hxx" | ".metal" | ".cu" | ".cuh" => Language::Cpp,
        ".cs" => Language::CSharp,
        ".cshtml" | ".razor" => Language::Razor,
        ".php" | ".module" | ".install" | ".theme" | ".inc" => Language::Php,
        ".yml" | ".yaml" => Language::Yaml,
        ".twig" => Language::Twig,
        ".rb" | ".rake" => Language::Ruby,
        ".swift" => Language::Swift,
        ".kt" | ".kts" => Language::Kotlin,
        ".dart" => Language::Dart,
        ".liquid" => Language::Liquid,
        ".svelte" => Language::Svelte,
        ".vue" => Language::Vue,
        ".astro" => Language::Astro,
        ".r" => Language::R,
        ".pas" | ".dpr" | ".dpk" | ".lpr" | ".dfm" | ".fmx" => Language::Pascal,
        ".scala" | ".sc" => Language::Scala,
        ".lua" => Language::Lua,
        ".luau" => Language::Luau,
        ".m" | ".mm" => Language::Objc,
        ".sol" => Language::Solidity,
        ".cfc" | ".cfm" => Language::Cfml,
        ".cfs" => Language::Cfscript,
        ".xml" => Language::Xml,
        ".cbl" | ".cob" | ".cobol" | ".cpy" => Language::Cobol,
        ".vb" => Language::Vbnet,
        ".erl" | ".hrl" | ".escript" => Language::Erlang,
        ".properties" => Language::Properties,
        ".tf" | ".tfvars" | ".tofu" => Language::Terraform,
        ".nix" => Language::Nix,
        _ => return None,
    })
}

/// Play Framework routes file: the extensionless `conf/routes` (and included
/// `conf/*.routes`). No grammar — processed through the no-symbol (yaml)
/// path; the Play framework resolver extracts route nodes.
fn is_play_routes_file(path: &str) -> bool {
    path == "conf/routes" || path.ends_with("/conf/routes") || path.ends_with(".routes")
}

/// Shopify OS 2.0 JSON template (`templates/**.json`) or section group
/// (`sections/**.json`) — they reference sections by `"type"`, so the Liquid
/// extractor links them. Nested template dirs allowed; case-insensitive.
fn is_shopify_liquid_json(path: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"(?i)(^|/)(templates|sections)/.+\.json$").unwrap()
    });
    RE.is_match(path)
}

/// OTP application resource file: `<app>.app.src` or its compiled
/// `<app>.app` — Erlang terms the grammar parses as top-level expressions.
/// Routed by full suffix because the last-dot extension (`.src`) is far too
/// generic for the extension map.
fn is_erlang_app_file(path: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"(?i)\.app(?:\.src)?$").unwrap()
    });
    RE.is_match(path)
}

/// First ~8 KiB of `source` (byte-based, backed off to a char boundary — the
/// TS heuristic sampled 8192 UTF-16 units; for an ASCII-dominated sniff
/// window the difference is immaterial).
fn sniff_sample(source: &str) -> &str {
    if source.len() <= 8192 {
        return source;
    }
    let mut end = 8192;
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    &source[..end]
}

/// Heuristic: does a `.h` file contain C++ constructs? Patterns are unique
/// to C++ and never valid C — including the `class MACRO Name [:{]`
/// export-macro branch (#1093 follow-up: a lean Unreal-Engine header whose
/// ONLY C++ signal is a macro-annotated class must not fall through to the
/// C extractor, which would drop the class entirely).
fn looks_like_cpp(source: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(
            r"\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)",
        )
        .unwrap()
    });
    RE.is_match(sniff_sample(source))
}

/// Heuristic: does a `.h` file contain Objective-C constructs?
fn looks_like_objc(source: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"@(?:interface|implementation|protocol|synthesize)\b").unwrap()
    });
    RE.is_match(sniff_sample(source))
}

/// The lowercased last-dot suffix of `path` (`".ts"`), or the whole path
/// lowercased when there is no dot — mirroring the TS
/// `substring(lastIndexOf('.'))` behavior exactly (a dotless name can never
/// match a `.x` map key, so both yield Unknown; mirrored anyway so the two
/// implementations stay comparable line-by-line).
fn last_dot_extension(path: &str) -> String {
    match path.rfind('.') {
        Some(i) => path[i..].to_lowercase(),
        None => path.to_lowercase(),
    }
}

/// Detect the language of `file_path`, optionally sniffing `source` content
/// (only used for `.h` C/C++/ObjC disambiguation). Detection order (map §6):
/// special extension-less/full-suffix routes first (Play routes → yaml,
/// Shopify JSON → liquid, OTP `.app`/`.app.src` → erlang), then the
/// lowercased last-dot extension against the map, else [`Language::Unknown`]
/// — never an error.
pub fn detect_language(file_path: &str, source: Option<&str>) -> Language {
    if is_play_routes_file(file_path) {
        return Language::Yaml;
    }
    if is_shopify_liquid_json(file_path) {
        return Language::Liquid;
    }
    if is_erlang_app_file(file_path) {
        return Language::Erlang;
    }
    let ext = last_dot_extension(file_path);
    let lang = language_for_extension(&ext).unwrap_or(Language::Unknown);

    // .h files could be C, C++, or Objective-C — check source content.
    if lang == Language::C
        && ext == ".h"
        && let Some(src) = source
    {
        if looks_like_cpp(src) {
            return Language::Cpp;
        }
        if looks_like_objc(src) {
            return Language::Objc;
        }
    }
    lang
}

/// Whether a file is one the extractor can process, based purely on its
/// path — THE indexability predicate, derived from the same extension map
/// as [`detect_language`] so the two can never drift.
pub fn is_source_file(file_path: &str) -> bool {
    if is_play_routes_file(file_path)
        || is_shopify_liquid_json(file_path)
        || is_erlang_app_file(file_path)
    {
        return true;
    }
    let Some(dot) = file_path.rfind('.') else {
        return false;
    };
    language_for_extension(&file_path[dot..].to_lowercase()).is_some()
}

/// Languages tracked at file level only — no symbol extraction
/// (`extraction-langs.md` §Wire: `{yaml, twig, properties}`).
pub fn is_file_level_only(l: Language) -> bool {
    matches!(l, Language::Yaml | Language::Twig | Language::Properties)
}
