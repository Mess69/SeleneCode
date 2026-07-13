//! `tsconfig.json` / `jsconfig.json` path aliases — [`load_project_aliases`]
//! and [`apply_aliases`].
//!
//! This is the single biggest blocker to accurate resolution on a modern JS/TS
//! codebase: `@/components/Foo` (Next, Nuxt, Nest, Vite) points into a
//! `compilerOptions.paths` map, and an import through an alias the resolver
//! cannot read is simply **unresolvable**.
//!
//! # JSONC tolerance is load-bearing
//!
//! Real tsconfigs carry `//` and `/* */` comments and trailing commas, which
//! strict JSON rejects. If the parse fails, [`load_project_aliases`] returns
//! `None` and **every aliased import in the repo silently regresses to
//! unresolved** — which is why the spike (F7) gated the parser choice before a
//! line of this was written. `json5` tolerates both, and (unlike the TS build's
//! hand-rolled `stripJsonc` + trailing-comma regex) it is a real parser, so a
//! `//` inside a string value cannot corrupt the file.
//!
//! # Documented non-features (preserved from the TS build, deliberately)
//!
//! - **`extends` chains are not followed.** Most projects do not need it, and a
//!   half-followed chain is worse than none.
//! - Vite/webpack/Rollup alias configs are not read.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::types::{AliasMap, AliasPattern};

#[derive(Debug, Deserialize)]
struct RawTsconfig {
    #[serde(rename = "compilerOptions")]
    compiler_options: Option<CompilerOptions>,
}

#[derive(Debug, Deserialize)]
struct CompilerOptions {
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    paths: Option<std::collections::BTreeMap<String, Vec<String>>>,
}

/// Load `compilerOptions.paths` from `tsconfig.json`, else `jsconfig.json`.
///
/// `None` when neither exists, neither parses, or neither declares `paths` —
/// `baseUrl` alone is not an alias (relative imports already resolve).
pub fn load_project_aliases(project_root: &Path) -> Option<AliasMap> {
    let raw = ["tsconfig.json", "jsconfig.json"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(project_root.join(name)).ok())
        // `json5` handles comments AND trailing commas (spike F7). A file that
        // fails to parse is a MISS, never an error — a malformed tsconfig
        // degrades resolution, it does not fail an index.
        .filter_map(|text| json5::from_str::<RawTsconfig>(&text).ok())
        .find(|cfg| {
            cfg.compiler_options
                .as_ref()
                .and_then(|co| co.paths.as_ref())
                .is_some_and(|p| !p.is_empty())
        })?;

    let co = raw.compiler_options?;
    let base_url_rel = co.base_url.unwrap_or_else(|| ".".to_string());
    let base_url = project_root.join(base_url_rel);
    let paths = co.paths?;

    let mut patterns: Vec<AliasPattern> = paths
        .into_iter()
        .filter(|(_, targets)| !targets.is_empty())
        .map(|(pattern, replacements)| {
            let (prefix, suffix, has_wildcard) = match pattern.find('*') {
                Some(star) => (
                    pattern[..star].to_string(),
                    pattern[star + 1..].to_string(),
                    true,
                ),
                None => (pattern.clone(), String::new(), false),
            };
            AliasPattern {
                prefix,
                suffix,
                has_wildcard,
                replacements,
            }
        })
        .collect();

    if patterns.is_empty() {
        return None;
    }

    // Specificity: the LONGER prefix first, then literal before wildcard —
    // TypeScript's own "most specific match wins". `@/lib/*` must beat `@/*`.
    patterns.sort_by(|a, b| {
        b.prefix
            .len()
            .cmp(&a.prefix.len())
            .then(a.has_wildcard.cmp(&b.has_wildcard))
    });

    Some(AliasMap { base_url, patterns })
}

/// Rewrite `import_path` through the alias map.
///
/// Returns the candidate paths **relative to the project root**, in tsconfig's
/// declared priority order (one alias may name several targets). Empty when no
/// pattern matches. The caller still applies the language's extension list —
/// this only does the rewrite.
pub fn apply_aliases(import_path: &str, aliases: &AliasMap, project_root: &Path) -> Vec<String> {
    for pat in &aliases.patterns {
        if !import_path.starts_with(&pat.prefix) {
            continue;
        }
        if !pat.suffix.is_empty() && !import_path.ends_with(&pat.suffix) {
            continue;
        }

        let captured = if pat.has_wildcard {
            let start = pat.prefix.len();
            let end = import_path.len() - pat.suffix.len();
            if end < start {
                continue;
            }
            &import_path[start..end]
        } else if import_path != pat.prefix {
            // A literal pattern must match exactly.
            continue;
        } else {
            ""
        };

        let mut out = Vec::new();
        for target in &pat.replacements {
            // ⚠ `replacen(.., 1)`, NOT `replace`. JS's `String.replace('*', x)`
            // substitutes only the FIRST `*`; Rust's `str::replace` substitutes
            // EVERY one. A two-star replacement would otherwise expand twice
            // (spike F5b).
            let filled = if pat.has_wildcard {
                target.replacen('*', captured, 1)
            } else {
                target.clone()
            };

            let absolute = normalize(&aliases.base_url.join(filled));
            let root = normalize(project_root);
            // A rewrite that escapes the project root is unusable (it cannot be
            // looked up in the file index) and unsafe — drop it.
            let Ok(relative) = absolute.strip_prefix(&root) else {
                continue;
            };
            let rel = relative.to_string_lossy().replace('\\', "/");
            if !rel.is_empty() {
                out.push(rel);
            }
        }
        // The FIRST matching pattern wins (they are sorted most-specific-first),
        // even when it produces no usable candidate — matching TS's `return`.
        return out;
    }
    Vec::new()
}

/// Lexically normalize a path (resolve `.` and `..`) **without touching the
/// filesystem** — `fs::canonicalize` would fail on a path that does not exist
/// yet, and every alias target is a path we are about to *look up*, not one we
/// know is there.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    /// The tolerance test. If this ever regresses, alias loading silently
    /// vanishes and EVERY aliased import in the repo becomes unresolved.
    #[test]
    fn a_tsconfig_with_comments_and_trailing_commas_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{
              // The compiler options.
              "compilerOptions": {
                "baseUrl": ".",
                /* Path aliases — the thing we actually need. */
                "paths": {
                  "@/*": ["src/*"],
                  "~/lib/*": ["lib/*", "vendor/lib/*"],
                },
              },
            }"#,
        );

        let aliases = load_project_aliases(dir.path()).expect("JSONC must parse");
        assert_eq!(aliases.patterns.len(), 2);

        assert_eq!(
            apply_aliases("@/components/Button", &aliases, dir.path()),
            vec!["src/components/Button"]
        );
        assert_eq!(
            apply_aliases("~/lib/x", &aliases, dir.path()),
            vec!["lib/x", "vendor/lib/x"],
            "several targets are returned in DECLARED priority order"
        );
        assert!(apply_aliases("./relative", &aliases, dir.path()).is_empty());
    }

    /// Spike F5b: JS replaces only the FIRST `*`; Rust's `str::replace` would
    /// replace every one and expand the path twice.
    #[test]
    fn only_the_first_star_in_a_replacement_is_filled() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@/*":["src/*/index/*"]}}}"#,
        );
        let aliases = load_project_aliases(dir.path()).unwrap();
        assert_eq!(
            apply_aliases("@/thing", &aliases, dir.path()),
            vec!["src/thing/index/*"],
            "the SECOND `*` is left alone — JS `String.replace` semantics"
        );
    }

    #[test]
    fn the_most_specific_pattern_wins() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{
                "@/*": ["src/*"],
                "@/lib/*": ["packages/lib/*"]
            }}}"#,
        );
        let aliases = load_project_aliases(dir.path()).unwrap();
        assert_eq!(
            apply_aliases("@/lib/x", &aliases, dir.path()),
            vec!["packages/lib/x"],
            "the longer prefix `@/lib/*` beats `@/*`"
        );
        assert_eq!(
            apply_aliases("@/other", &aliases, dir.path()),
            vec!["src/other"]
        );
    }

    #[test]
    fn base_url_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":"./src","paths":{"@/*":["app/*"]}}}"#,
        );
        let aliases = load_project_aliases(dir.path()).unwrap();
        assert_eq!(
            apply_aliases("@/x", &aliases, dir.path()),
            vec!["src/app/x"],
            "targets resolve against baseUrl, and the result is root-relative"
        );
    }

    #[test]
    fn a_rewrite_that_escapes_the_project_root_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@evil/*":["../outside/*"]}}}"#,
        );
        let aliases = load_project_aliases(dir.path()).unwrap();
        assert!(
            apply_aliases("@evil/x", &aliases, dir.path()).is_empty(),
            "an escaping rewrite cannot be looked up in the file index anyway"
        );
    }

    #[test]
    fn a_literal_pattern_must_match_exactly() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@app":["src/app.ts"]}}}"#,
        );
        let aliases = load_project_aliases(dir.path()).unwrap();
        assert_eq!(
            apply_aliases("@app", &aliases, dir.path()),
            vec!["src/app.ts"]
        );
        assert!(apply_aliases("@app/sub", &aliases, dir.path()).is_empty());
    }

    #[test]
    fn absence_is_graceful() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load_project_aliases(dir.path()).is_none(),
            "no tsconfig at all"
        );

        write(
            dir.path(),
            "tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":"."}}"#,
        );
        assert!(
            load_project_aliases(dir.path()).is_none(),
            "baseUrl alone is not an alias map"
        );

        write(dir.path(), "tsconfig.json", "{ not json at all ///");
        assert!(
            load_project_aliases(dir.path()).is_none(),
            "a malformed tsconfig is a MISS, never an error"
        );
    }

    #[test]
    fn jsconfig_is_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "jsconfig.json",
            r#"{"compilerOptions":{"paths":{"@/*":["src/*"]}}}"#,
        );
        let aliases = load_project_aliases(dir.path()).unwrap();
        assert_eq!(apply_aliases("@/x", &aliases, dir.path()), vec!["src/x"]);
    }
}
