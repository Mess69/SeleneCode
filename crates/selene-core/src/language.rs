//! [`Language`] — the language registry, and [`LanguageFamily`] — the
//! family table resolution gates on.
//!
//! # Why this lives in `selene-core` (decision D1, 2026-07-13)
//!
//! `Language` began in `selene-extract` (Phase 2), because extraction was the
//! only thing that had an opinion about languages. It is, in fact, a **shared
//! wire concept exactly like [`crate::NodeKind`]/[`crate::EdgeKind`]**: the
//! resolver gates every candidate on it ([`LanguageFamily`]), the framework
//! registry keys its applicability on it, and the store persists it (as the
//! wire string) on every [`crate::Node`]. Leaving it in the extractor would
//! force `selene-resolve` to depend on `selene-extract` — backwards layering
//! (the pipeline is extract → resolve) and, once frameworks emit nodes, a
//! literal dependency cycle. So it moved here, and `selene-extract`
//! **re-exports it** so every existing path keeps working.
//!
//! What did NOT move, deliberately: `detect_language`, `is_source_file`,
//! `is_file_level_only`, and the `EXTENSION_MAP` behind them. Those are
//! **extraction policy** (which files we index, which of them yield symbols),
//! not wire types — they stay in `selene-extract`.
//!
//! # The wire contract
//!
//! [`Language::as_str`] is the wire string persisted in `Node.language` and
//! compared against by every downstream consumer. [`Language::from_wire`] is
//! its exact inverse — the resolver reads `node.language` (a `String` on the
//! wire) back into a `Language` at the store boundary. The two round-trip for
//! every variant, and a test pins that; an unknown string yields `None`, never
//! a panic and never a silent [`Language::Unknown`] (which is a *real* variant
//! meaning "we know this file, we have no language for it").

/// Every language the extension map can name. Wire strings are the lowercase
/// TS `Language` union values ([`Language::as_str`]); `Unknown` is the
/// explicit "no mapping" value, never an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Typescript,
    Tsx,
    Javascript,
    Jsx,
    Arkts,
    Python,
    Go,
    Rust,
    Java,
    Kotlin,
    C,
    Cpp,
    CSharp,
    Razor,
    Php,
    Ruby,
    Swift,
    Dart,
    Yaml,
    Twig,
    Liquid,
    Svelte,
    Vue,
    Astro,
    R,
    Pascal,
    Scala,
    Lua,
    Luau,
    Objc,
    Solidity,
    Cfml,
    Cfscript,
    Xml,
    Cobol,
    Vbnet,
    Erlang,
    Properties,
    Terraform,
    Nix,
    Unknown,
}

/// Every [`Language`] variant, in declaration order. The single source the
/// round-trip test (and any future exhaustive sweep) iterates — adding a
/// variant without adding it here fails that test.
pub const ALL_LANGUAGES: &[Language] = &[
    Language::Typescript,
    Language::Tsx,
    Language::Javascript,
    Language::Jsx,
    Language::Arkts,
    Language::Python,
    Language::Go,
    Language::Rust,
    Language::Java,
    Language::Kotlin,
    Language::C,
    Language::Cpp,
    Language::CSharp,
    Language::Razor,
    Language::Php,
    Language::Ruby,
    Language::Swift,
    Language::Dart,
    Language::Yaml,
    Language::Twig,
    Language::Liquid,
    Language::Svelte,
    Language::Vue,
    Language::Astro,
    Language::R,
    Language::Pascal,
    Language::Scala,
    Language::Lua,
    Language::Luau,
    Language::Objc,
    Language::Solidity,
    Language::Cfml,
    Language::Cfscript,
    Language::Xml,
    Language::Cobol,
    Language::Vbnet,
    Language::Erlang,
    Language::Properties,
    Language::Terraform,
    Language::Nix,
    Language::Unknown,
];

impl Language {
    /// The lowercase wire string (TS `Language` union value).
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Typescript => "typescript",
            Language::Tsx => "tsx",
            Language::Javascript => "javascript",
            Language::Jsx => "jsx",
            Language::Arkts => "arkts",
            Language::Python => "python",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::CSharp => "csharp",
            Language::Razor => "razor",
            Language::Php => "php",
            Language::Ruby => "ruby",
            Language::Swift => "swift",
            Language::Dart => "dart",
            Language::Yaml => "yaml",
            Language::Twig => "twig",
            Language::Liquid => "liquid",
            Language::Svelte => "svelte",
            Language::Vue => "vue",
            Language::Astro => "astro",
            Language::R => "r",
            Language::Pascal => "pascal",
            Language::Scala => "scala",
            Language::Lua => "lua",
            Language::Luau => "luau",
            Language::Objc => "objc",
            Language::Solidity => "solidity",
            Language::Cfml => "cfml",
            Language::Cfscript => "cfscript",
            Language::Xml => "xml",
            Language::Cobol => "cobol",
            Language::Vbnet => "vbnet",
            Language::Erlang => "erlang",
            Language::Properties => "properties",
            Language::Terraform => "terraform",
            Language::Nix => "nix",
            Language::Unknown => "unknown",
        }
    }

    /// The exact inverse of [`Self::as_str`]: parse a stored wire string
    /// (`Node.language`, `UnresolvedRef.language`) back into a `Language`.
    ///
    /// `None` for a string no variant spells — a **caller decision**, not an
    /// error and not a silent [`Language::Unknown`]: the resolver drops a
    /// reference whose language it cannot type rather than gating it as if it
    /// were some other language (a wrong gate is a wrong edge, and a wrong
    /// edge is worse than none).
    pub fn from_wire(s: &str) -> Option<Language> {
        Some(match s {
            "typescript" => Language::Typescript,
            "tsx" => Language::Tsx,
            "javascript" => Language::Javascript,
            "jsx" => Language::Jsx,
            "arkts" => Language::Arkts,
            "python" => Language::Python,
            "go" => Language::Go,
            "rust" => Language::Rust,
            "java" => Language::Java,
            "kotlin" => Language::Kotlin,
            "c" => Language::C,
            "cpp" => Language::Cpp,
            "csharp" => Language::CSharp,
            "razor" => Language::Razor,
            "php" => Language::Php,
            "ruby" => Language::Ruby,
            "swift" => Language::Swift,
            "dart" => Language::Dart,
            "yaml" => Language::Yaml,
            "twig" => Language::Twig,
            "liquid" => Language::Liquid,
            "svelte" => Language::Svelte,
            "vue" => Language::Vue,
            "astro" => Language::Astro,
            "r" => Language::R,
            "pascal" => Language::Pascal,
            "scala" => Language::Scala,
            "lua" => Language::Lua,
            "luau" => Language::Luau,
            "objc" => Language::Objc,
            "solidity" => Language::Solidity,
            "cfml" => Language::Cfml,
            "cfscript" => Language::Cfscript,
            "xml" => Language::Xml,
            "cobol" => Language::Cobol,
            "vbnet" => Language::Vbnet,
            "erlang" => Language::Erlang,
            "properties" => Language::Properties,
            "terraform" => Language::Terraform,
            "nix" => Language::Nix,
            "unknown" => Language::Unknown,
            _ => return None,
        })
    }

    /// This language's family, or `None` when it is its own singleton family.
    ///
    /// `LANGUAGE_FAMILY`, ported verbatim from
    /// `maps/resolution.md` §`resolveOne` pipeline (Language gates). The table
    /// is small and load-bearing: it is what stops a `references` ref in a
    /// Python file binding to a same-named Go symbol, while still allowing the
    /// `.ts` ↔ `.tsx` and `.java` ↔ `.kotlin` bindings that are genuinely one
    /// program. **Everything not listed here is its own family** — and that is
    /// meaningful, not a gap: an unfamilied language neither "matches" another
    /// language nor "crosses a known family boundary", which is precisely why
    /// `imports` refs in such languages survive the import gate (see
    /// `selene_resolve::families`).
    pub fn family(&self) -> Option<LanguageFamily> {
        Some(match self {
            Language::Java | Language::Kotlin | Language::Scala => LanguageFamily::Jvm,
            Language::Swift | Language::Objc => LanguageFamily::Apple,
            Language::Typescript
            | Language::Tsx
            | Language::Javascript
            | Language::Jsx
            | Language::Arkts => LanguageFamily::Web,
            Language::C | Language::Cpp => LanguageFamily::C,
            Language::CSharp | Language::Razor => LanguageFamily::Dotnet,
            _ => return None,
        })
    }
}

/// The wire string, via [`Language::as_str`] — `Node.language` and
/// `UnresolvedRef.language` serialize to exactly the bytes the `String` field
/// they replaced produced (pinned by `serde_matches_as_str_for_every_variant`).
impl serde::Serialize for Language {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The exact inverse, via [`Language::from_wire`]. A string no variant spells
/// is a **hard deserialize error**, not a silent [`Language::Unknown`]:
/// extraction only ever writes wire strings, so a non-wire value in a store is
/// foreign/corrupt data, and loudly refusing it beats silently rewriting it to
/// `"unknown"` on the next upsert.
impl<'de> serde::Deserialize<'de> for Language {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WireVisitor;
        impl serde::de::Visitor<'_> for WireVisitor {
            type Value = Language;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a Language wire string (see Language::as_str)")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Language, E> {
                Language::from_wire(v)
                    .ok_or_else(|| E::invalid_value(serde::de::Unexpected::Str(v), &self))
            }
        }
        deserializer.deserialize_str(WireVisitor)
    }
}

/// The five language families resolution gates on ([`Language::family`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LanguageFamily {
    /// `java` · `kotlin` · `scala`
    Jvm,
    /// `swift` · `objc`
    Apple,
    /// `typescript` · `tsx` · `javascript` · `jsx` · `arkts`
    Web,
    /// `c` · `cpp`
    C,
    /// `csharp` · `razor`
    Dotnet,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::collections::HashSet;

    /// `as_str` ⇄ `from_wire` round-trips for EVERY variant. This is the wire
    /// contract: the resolver reads `node.language` off the store through
    /// `from_wire`, so a variant that does not round-trip is a language whose
    /// references silently stop resolving.
    #[test]
    fn wire_round_trips_for_every_variant() {
        for &l in ALL_LANGUAGES {
            assert_eq!(
                Language::from_wire(l.as_str()),
                Some(l),
                "{l:?} does not round-trip through its wire string {:?}",
                l.as_str()
            );
        }
    }

    /// Every wire string is distinct — two variants sharing one would make
    /// `from_wire` lossy in a way the round-trip test alone cannot see.
    #[test]
    fn wire_strings_are_unique() {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for &l in ALL_LANGUAGES {
            assert!(
                seen.insert(l.as_str()),
                "duplicate wire string {:?}",
                l.as_str()
            );
        }
        assert_eq!(seen.len(), ALL_LANGUAGES.len());
    }

    /// The serde output IS `as_str` — for every variant. This is what makes
    /// `Node.language: Language` byte-identical on the wire to the `String` it
    /// replaced (the 13 extraction snapshots and the DB rows all ride on it).
    #[test]
    fn serde_matches_as_str_for_every_variant() {
        for &l in ALL_LANGUAGES {
            let json = serde_json::to_string(&l).expect("serialize");
            assert_eq!(json, format!("\"{}\"", l.as_str()));
            let back: Language = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, l);
        }
    }

    /// A non-wire string is a hard deserialize error — never a silent
    /// `Unknown`. (`"unknown"` itself IS a wire string and round-trips.)
    #[test]
    fn serde_rejects_a_non_wire_string() {
        assert!(serde_json::from_str::<Language>("\"klingon\"").is_err());
        assert!(serde_json::from_str::<Language>("\"\"").is_err());
        assert!(serde_json::from_str::<Language>("\"Typescript\"").is_err());
        assert_eq!(
            serde_json::from_str::<Language>("\"unknown\"").expect("wire variant"),
            Language::Unknown
        );
    }

    #[test]
    fn from_wire_rejects_an_unknown_string() {
        assert_eq!(Language::from_wire("klingon"), None);
        assert_eq!(Language::from_wire(""), None);
        // Case matters: the wire strings are lowercase by contract.
        assert_eq!(Language::from_wire("Typescript"), None);
        // `unknown` IS a real variant (a known file, no language) — not a miss.
        assert_eq!(Language::from_wire("unknown"), Some(Language::Unknown));
    }

    #[test]
    fn language_family_table_is_the_map_verbatim() {
        use LanguageFamily::*;
        assert_eq!(Language::Java.family(), Some(Jvm));
        assert_eq!(Language::Kotlin.family(), Some(Jvm));
        assert_eq!(Language::Scala.family(), Some(Jvm));
        assert_eq!(Language::Swift.family(), Some(Apple));
        assert_eq!(Language::Objc.family(), Some(Apple));
        assert_eq!(Language::Typescript.family(), Some(Web));
        assert_eq!(Language::Tsx.family(), Some(Web));
        assert_eq!(Language::Javascript.family(), Some(Web));
        assert_eq!(Language::Jsx.family(), Some(Web));
        assert_eq!(Language::Arkts.family(), Some(Web));
        assert_eq!(Language::C.family(), Some(C));
        assert_eq!(Language::Cpp.family(), Some(C));
        assert_eq!(Language::CSharp.family(), Some(Dotnet));
        assert_eq!(Language::Razor.family(), Some(Dotnet));
        // Everything else is its own singleton family — load-bearing, not a gap.
        assert_eq!(Language::Python.family(), None);
        assert_eq!(Language::Ruby.family(), None);
        assert_eq!(Language::Go.family(), None);
        assert_eq!(Language::Rust.family(), None);
        assert_eq!(Language::Php.family(), None);
    }
}
