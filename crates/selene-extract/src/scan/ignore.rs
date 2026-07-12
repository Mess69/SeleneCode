//! `ScopeIgnore` — the workspace-scope ignore matcher (Task 16), ported from
//! CodeGraph's `src/extraction/index.ts` (the `DEFAULT_IGNORE_DIRS` /
//! `readGitignorePatterns` / `buildDefaultIgnore` / `ScopeIgnore` block).
//!
//! Semantics (extraction-core map §1, all pinned by `tests/scan_ignore_test.rs`):
//!
//! - **Built-in defaults apply to everyone** — tracked files too: committing a
//!   dependency/build dir doesn't make it project code; the explicit root
//!   `.gitignore` negation (e.g. `!vendor/`) is the only opt-in (#407).
//! - **Embedded repos are judged by their own rules** (#514): a super-repo's
//!   `.gitignore` hides a child repo from *git*, not from the index. Paths
//!   under an embedded root get that repo's matcher (defaults + its own root
//!   `.gitignore`) on the *inner* path — except the built-in defaults, which
//!   apply to the full path uniformly (an embedded repo inside `node_modules`
//!   is an npm git-dependency, not project code). Matchers are consulted
//!   longest-root-first so nested embedded repos hit the innermost one, and a
//!   directory that is an ancestor of an embedded root is never pruned (the
//!   walker/watcher must descend to reach it).
//! - **User `exclude` wins over everything** (#999) — it must drop even
//!   git-*tracked* paths (which `.gitignore` cannot) and applies on the full
//!   root-relative path, embedded repos included.
//! - **User `include` forces files back in** despite `.gitignore` — but never
//!   resurrects a built-in default-ignored dir, and never beats `exclude`.
//!   `include_roots` (the patterns' static directory prefixes) keep ancestor
//!   directories of an included subtree walkable even when gitignored.
//!
//! Config-file loading is Phase 8; the overrides arrive as a plain
//! [`ScopeOverrides`] parameter (defaulting to empty).
//!
//! ## The `rel + "/"` convention
//!
//! Callers pass **root-relative, forward-slash** paths; a **trailing slash
//! marks a directory** (dir-only rules like `build/` only match directories).
//! Internally that maps onto the `ignore` crate's
//! `matched_path_or_any_parents(path, is_dir)` — the trailing slash is
//! stripped into `is_dir = true`. Both load-bearing conventions the Task 16
//! brief calls out — this directory mapping and `!pattern` negation
//! (last-match-wins, npm-`ignore`-identical) — are verified against the real
//! matcher by the ported tests.

use std::path::Path;

use ::ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Directory *names* ignored by default at any depth, copied **verbatim**
/// (names and grouping) from CodeGraph's `DEFAULT_IGNORE_DIRS`
/// (`../codegraph/src/extraction/index.ts`). 62 names.
pub(crate) const DEFAULT_IGNORE_DIRS: [&str; 62] = [
    // JS / TS — dependency directories
    "node_modules",
    "bower_components",
    "jspm_packages",
    "web_modules",
    ".yarn",
    ".pnpm-store",
    // JS / TS — framework & bundler build / cache / deploy output
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".vite",
    ".parcel-cache",
    ".angular",
    ".docusaurus",
    "storybook-static",
    ".vinxi",
    ".nitro",
    "out-tsc",
    ".vercel",
    ".netlify",
    ".wrangler",
    // Build output (common across ecosystems)
    "dist",
    "build",
    "out",
    ".output",
    // Test / coverage
    "coverage",
    ".nyc_output",
    // Python
    "__pycache__",
    "__pypackages__",
    ".venv",
    "venv",
    ".pixi",
    ".pdm-build",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".hypothesis",
    ".ipynb_checkpoints",
    ".eggs",
    // Rust / JVM (Maven, Gradle, Scala)
    "target",
    ".gradle",
    // .NET
    "obj",
    // Vendored deps (Go, PHP/Composer, Ruby/Bundler)
    "vendor",
    // Swift / iOS
    ".build",
    "Pods",
    "Carthage",
    "DerivedData",
    ".swiftpm",
    // Dart / Flutter
    ".dart_tool",
    ".pub-cache",
    // Native (Android NDK, C/C++ deps)
    ".cxx",
    ".externalNativeBuild",
    "vcpkg_installed",
    // Scala tooling
    ".bloop",
    ".metals",
    // Lua / Luau (LuaRocks)
    "lua_modules",
    ".luarocks",
    // Delphi / RAD Studio IDE backups (duplicate .pas source — would double-count)
    "__history",
    "__recovery",
    // Generic cache
    ".cache",
];

/// Android resource directory types (#1047). A `res/` tree holds only
/// non-code resources split into one typed subdirectory per kind, optionally
/// qualifier-suffixed (`values-es`, `drawable-hdpi`, `layout-v21`, …) — on an
/// Android app it can dominate the tree (26k XML files = 97% of one reported
/// project, 0 symbols). `res/raw/` is deliberately absent: it holds arbitrary
/// bundled assets that can be code-ish. Override with a `.gitignore` negation
/// (e.g. `!res/values/`).
const ANDROID_RES_TYPES: [&str; 12] = [
    "anim",
    "animator",
    "color",
    "drawable",
    "font",
    "layout",
    "menu",
    "mipmap",
    "navigation",
    "transition",
    "values",
    "xml",
];

/// Gitignore-style pattern list for the defaults: every
/// [`DEFAULT_IGNORE_DIRS`] name as a dir-only rule, plus the glob rules
/// (Python packaging metadata, CLion/CMake build trees, Bazel output trees,
/// Android res dirs at any depth with their qualifier variants).
fn default_ignore_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_IGNORE_DIRS
        .iter()
        .map(|d| format!("{d}/"))
        .collect();
    patterns.push("*.egg-info/".to_string());
    patterns.push("cmake-build-*/".to_string());
    patterns.push("bazel-*/".to_string());
    patterns.extend(ANDROID_RES_TYPES.iter().map(|t| format!("**/res/{t}*/")));
    patterns
}

/// Build a matcher from `lines`, dropping any line the matcher cannot
/// compile (the per-line half of the defensive posture — see
/// [`read_gitignore_lines`], which pre-filters real `.gitignore` files the
/// same way). The builder is rooted at `""` so relative paths are matched
/// as-is.
fn matcher_from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Gitignore {
    let mut builder = GitignoreBuilder::new("");
    for line in lines {
        // An uncompilable line is dropped; the rest are kept (#682). The
        // built-in defaults are known-good literals, so for them this is a
        // no-op (pinned by the `all_default_patterns_compile` unit test).
        let _ = builder.add_line(None, line);
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Defaults-only matcher (no `.gitignore` merged). Used wherever a repo's
/// own ignore rules must NOT apply — inside embedded child repos, whose
/// gitignore semantics their own `git ls-files` already enforced (#514).
pub(crate) fn defaults_only_ignore() -> Gitignore {
    let patterns = default_ignore_patterns();
    matcher_from_lines(patterns.iter().map(String::as_str))
}

/// Matcher seeded with the built-in defaults, merged with `root`'s
/// `.gitignore` so a negation there (e.g. `!vendor/`) overrides a default —
/// gitignore's last-match-wins rule makes the later file lines take
/// precedence. Shared by both enumeration paths so behavior is identical
/// with or without git.
pub(crate) fn build_default_ignore(root: &Path) -> Gitignore {
    let mut lines = default_ignore_patterns();
    lines.extend(read_gitignore_lines(&root.join(".gitignore")));
    matcher_from_lines(lines.iter().map(String::as_str))
}

/// Defensively read a `.gitignore`, returning only lines safe to hand to the
/// matcher — never failing, even when the file isn't real gitignore text.
/// Two in-the-wild failure modes (#682):
///
/// - The file isn't valid UTF-8 (or contains NUL) — e.g. transparently
///   encrypted in place by endpoint-security software. None of it is
///   meaningful patterns, so the **whole file** is skipped.
/// - A single line can't be compiled to a matcher (`a[` and friends). That
///   one line is dropped via a single-pattern compile probe; the rest are
///   kept.
///
/// Missing/unreadable files (permissions, races) are treated as absent.
pub(crate) fn read_gitignore_lines(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // A NUL byte never appears in real gitignore text; a strict UTF-8 decode
    // catches the rest. Such a file isn't ignore patterns at all.
    if bytes.contains(&0) {
        return Vec::new();
    }
    let Ok(content) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|line| {
            // Per-line compile probe: keep only lines a single-pattern
            // matcher accepts, so one bad pattern can't poison the file.
            GitignoreBuilder::new("").add_line(None, line).is_ok()
        })
        .map(str::to_string)
        .collect()
}

/// `rel` (root-relative, forward slashes, trailing `/` marks a directory)
/// against `matcher`, per the module docs' `rel + "/"` convention. Matches
/// the path **or any parent** (a file under an ignored dir is ignored).
pub(crate) fn matches_rel(matcher: &Gitignore, rel: &str) -> bool {
    let (path, is_dir) = match rel.strip_suffix('/') {
        Some(stripped) => (stripped, true),
        None => (rel, false),
    };
    if path.is_empty() {
        return false;
    }
    matcher
        .matched_path_or_any_parents(path, is_dir)
        .is_ignore()
}

/// The `{include, exclude}` overrides [`ScopeIgnore::build`] accepts —
/// gitignore-style pattern lists, matched against full root-relative paths.
/// Config-file loading (the `.selene` project config) is Phase 8; until then
/// callers pass these directly (default: empty, the zero-config behavior).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeOverrides {
    /// First-party source forced INTO the index despite `.gitignore`. Never
    /// resurrects a built-in default-ignored dir, never beats `exclude`.
    pub include: Vec<String>,
    /// Paths kept OUT of the index even when git-tracked (which `.gitignore`
    /// cannot do — #999). Wins over everything.
    pub exclude: Vec<String>,
}

/// Workspace-scope ignore matcher — the single source of truth for indexer
/// and watcher scope (they must not diverge). See the module docs for the
/// full semantics; every rule is pinned by `tests/scan_ignore_test.rs`.
#[derive(Debug)]
pub struct ScopeIgnore {
    /// Defaults + the scan root's `.gitignore`.
    root_matcher: Gitignore,
    /// Defaults only — applied to full paths uniformly, and consulted by the
    /// `include` branch so an include can't revive a default-ignored dir.
    defaults: Gitignore,
    /// `(trailing-slashed root, that repo's defaults + own .gitignore)`,
    /// sorted longest-root-first so nested embedded repos hit the innermost
    /// matcher.
    embedded: Vec<(String, Gitignore)>,
    exclude: Option<Gitignore>,
    include: Option<Gitignore>,
    /// Static directory prefixes of the include patterns, so gitignored
    /// ancestor dirs of an included subtree stay walkable.
    include_roots: Vec<String>,
}

impl ScopeIgnore {
    /// Build the workspace-scope matcher for `root`. `embedded_roots` are
    /// root-relative embedded-repo prefixes (with or without the trailing
    /// slash — normalized here); each gets its own matcher of defaults + its
    /// repo-root `.gitignore`.
    pub fn build(root: &Path, embedded_roots: &[String], overrides: &ScopeOverrides) -> Self {
        let mut embedded: Vec<(String, Gitignore)> = embedded_roots
            .iter()
            .map(|r| {
                let rel = if r.ends_with('/') {
                    r.clone()
                } else {
                    format!("{r}/")
                };
                let matcher = build_default_ignore(&root.join(&rel));
                (rel, matcher)
            })
            .collect();
        // Longest root first so paths in nested embedded repos hit the
        // innermost matcher (name order as the deterministic tiebreak).
        embedded.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

        let compile_overrides = |patterns: &[String]| -> Option<Gitignore> {
            let usable: Vec<&str> = patterns
                .iter()
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();
            if usable.is_empty() {
                None
            } else {
                Some(matcher_from_lines(usable))
            }
        };
        let include = compile_overrides(&overrides.include);

        Self {
            root_matcher: build_default_ignore(root),
            defaults: defaults_only_ignore(),
            embedded,
            exclude: compile_overrides(&overrides.exclude),
            include_roots: if include.is_some() {
                include_static_roots(&overrides.include)
            } else {
                Vec::new()
            },
            include,
        }
    }

    /// Whether `rel` (root-relative, forward slashes, trailing `/` = a
    /// directory) is out of scope. Precedence: user `exclude` → user
    /// `include` (unless default-ignored) → the owning embedded repo's rules
    /// (defaults on the full path, its own rules on the inner path) →
    /// embedded-ancestor walkability → the root matcher.
    pub fn ignores(&self, rel: &str) -> bool {
        // User `exclude` (#999) first, on the full root-relative path: it
        // must drop git-TRACKED paths and apply everywhere, embedded repos
        // and their ancestors included.
        if let Some(exclude) = &self.exclude
            && matches_rel(exclude, rel)
        {
            return true;
        }
        // User `include`: force first-party source back in despite
        // `.gitignore` — but never resurface a built-in default-ignored dir.
        if let Some(include) = &self.include
            && !matches_rel(&self.defaults, rel)
        {
            if rel.ends_with('/') {
                // A directory on (or leading to) an included subtree must
                // stay walkable so the walker/watcher descends to reach
                // the forced-in files.
                if self
                    .include_roots
                    .iter()
                    .any(|r| r.starts_with(rel) || rel.starts_with(r.as_str()))
                {
                    return false;
                }
            } else if matches_rel(include, rel) {
                return false;
            }
        }
        for (root, matcher) in &self.embedded {
            if let Some(inner) = rel.strip_prefix(root.as_str()) {
                if inner.is_empty() {
                    return false;
                }
                // Built-in defaults apply to the FULL path uniformly (#407);
                // everything else is the embedded repo's own business (#514).
                return matches_rel(&self.defaults, rel) || matches_rel(matcher, inner);
            }
        }
        // Never prune a directory that leads to an embedded repo.
        if rel.ends_with('/') && self.embedded.iter().any(|(root, _)| root.starts_with(rel)) {
            return false;
        }
        matches_rel(&self.root_matcher, rel)
    }
}

/// The static (glob-free) directory prefixes of `patterns` — ported verbatim
/// from CodeGraph's `includeStaticRoots`. A wholly-literal pattern with no
/// trailing slash names a file, so its last segment is dropped (walk the
/// containing dir, let the matcher pick the file); a top-level glob forces a
/// whole-tree walk (`[""]`); roots nested under a broader root collapse away.
fn include_static_roots(patterns: &[String]) -> Vec<String> {
    const GLOB_META: [char; 7] = ['*', '?', '[', ']', '{', '}', '!'];
    let mut roots: Vec<String> = Vec::new();
    for pattern in patterns {
        let trimmed = pattern.trim_start_matches('/');
        let trailing_slash = trimmed.ends_with('/');
        let body = trimmed.trim_end_matches('/');
        let segs: Vec<&str> = body.split('/').filter(|s| !s.is_empty()).collect();
        let mut lead: Vec<&str> = segs
            .iter()
            .take_while(|s| !s.contains(GLOB_META))
            .copied()
            .collect();
        let had_wildcard = lead.len() < segs.len();
        if !had_wildcard && !trailing_slash && !lead.is_empty() {
            lead.pop();
        }
        if lead.is_empty() {
            // A top-level glob forces a whole-tree walk; nothing narrower matters.
            return vec![String::new()];
        }
        let root = format!("{}/", lead.join("/"));
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    // Collapse roots nested under a broader one (drop `a/b/` if `a/` exists).
    let all = roots.clone();
    roots.retain(|r| {
        !all.iter()
            .any(|other| other != r && r.starts_with(other.as_str()))
    });
    roots
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Every hand-copied default pattern must actually compile — a silently
    /// dropped default would erode scan scope without any signal.
    #[test]
    fn all_default_patterns_compile() {
        for p in default_ignore_patterns() {
            assert!(
                GitignoreBuilder::new("").add_line(None, &p).is_ok(),
                "default pattern failed to compile: {p}"
            );
        }
        assert_eq!(DEFAULT_IGNORE_DIRS.len(), 62, "verbatim TS list length");
        assert_eq!(default_ignore_patterns().len(), 62 + 3 + 12);
    }

    #[test]
    fn include_static_roots_ports_the_ts_cases() {
        let r = |ps: &[&str]| {
            include_static_roots(&ps.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        // Glob tail → the literal lead is the root.
        assert_eq!(r(&["generated/src/**/*.ts"]), vec!["generated/src/"]);
        // Trailing slash = already a directory.
        assert_eq!(r(&["generated/"]), vec!["generated/"]);
        // Wholly-literal file pattern → containing dir.
        assert_eq!(r(&["generated/api.ts"]), vec!["generated/"]);
        // Top-level glob → whole-tree walk.
        assert_eq!(r(&["**/*.ts"]), vec![String::new()]);
        // Nested roots collapse under the broader one.
        assert_eq!(r(&["a/", "a/b/**"]), vec!["a/"]);
    }
}
