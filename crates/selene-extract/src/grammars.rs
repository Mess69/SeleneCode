//! v0 grammar registry: [`Language`] → `tree_sitter::Language` for the 13
//! pinned grammars (Task 1 spike). Wave-2 languages return `None` — they
//! detect (`src/language.rs`) but extraction skips them with an
//! `unsupported_language` warning.
//!
//! Parser construction is cheap with native grammars (a `LanguageFn` is a
//! function pointer; no WASM heap, no load step), so v0 builds a fresh
//! `Parser` per extraction. Per-thread parser reuse is a Task 16/18
//! (rayon orchestrator) optimization if profiling asks for it.

use tree_sitter::Language as TsLanguage;

use crate::Language;

/// The tree-sitter grammar for `l`, when one is pinned in v0.
pub(crate) fn grammar_for(l: Language) -> Option<TsLanguage> {
    Some(match l {
        Language::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        // JS grammar parses JSX too (same crate lineage as TS's jsx handling).
        Language::Javascript | Language::Jsx => tree_sitter_javascript::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        // The `php` fn (mixed HTML mode), not `php_only` — Task 1 pin.
        Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        _ => return None,
    })
}
