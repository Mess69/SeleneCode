//! Extension resolution: the per-language suffix tables probed by the
//! module-specifier → file walk.

use selene_core::Language;

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
pub(super) fn extensions_for(language: Language) -> &'static [&'static str] {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
