//! The **Cargo workspace crate map** (Task 18) — a helper, not a
//! [`FrameworkResolver`](super::FrameworkResolver).
//!
//! # What it buys
//!
//! In a Cargo workspace, `use my_crate::thing` names a **sibling crate**, and the
//! crate lives in a directory the reference never mentions. Without this map a
//! module-shaped reference either resolves to a same-named *local* module (a wrong
//! edge) or to nothing. With it, `my-crate` and `my_crate` both point at
//! `crates/my-crate/`, and `rust_fw`'s module arm can land on that crate's
//! `lib.rs` — at **0.95**, deliberately above the name matcher's 0.7 self-file
//! score, because a workspace crate reference must not lose to a coincidence.
//!
//! # Hand-rolled TOML, deliberately
//!
//! `frameworks-synth.md` specifies a hand-rolled, escape-aware section/array/quote
//! parser, and the parity source is lenient in ways a strict TOML crate is not: a
//! `Cargo.toml` that fails to parse must degrade to "no workspace", never to a
//! failed index. Pulling in `toml` would make a malformed manifest an *error*
//! where the TS build simply found no members — a behavioral divergence in the
//! worst direction (errors are collected, never thrown).
//!
//! # Reading the disk, not the index
//!
//! `Cargo.toml` has no grammar, so it is **not an indexed file** — `file_exists`
//! (which answers from the index) would say it does not exist. Every read here
//! goes through [`ResolutionContext::read_file`], which reads the working tree.

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use crate::context::ResolutionContext;

/// Directories a glob walk never descends into. `target/` is the big one: it
/// holds thousands of directories and not one of them is a workspace member.
const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", "dist", "build"];

/// How deep a `members = ["crates/*"]` glob may walk. Ported verbatim.
const MAX_GLOB_WALK_DEPTH: usize = 5;

/// crate name → member directory, for **both** spellings (`my-crate` *and*
/// `my_crate` — the second is how the crate is actually named in `use`).
pub type CrateMap = BTreeMap<String, String>;

/// The cache. Keyed by `(project root, hash of the root Cargo.toml)`, so a
/// workspace whose membership changes is re-read rather than served stale.
///
/// (The TS build cached per-context in a `WeakMap`. Our resolvers are zero-sized
/// statics — there is no instance to hang a `OnceCell` on — so the context's root
/// is the key instead. [`clear_cargo_workspace_cache`] exists for the sync path.)
type Cache = HashMap<(PathBuf, u64), Arc<CrateMap>>;
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop the memoized crate maps. The incremental-sync path calls this when a
/// member's own `Cargo.toml` changes (the root's hash would not have moved).
pub fn clear_cargo_workspace_cache() {
    if let Ok(mut c) = CACHE.lock() {
        c.clear();
    }
}

/// The workspace's crate map, memoized. Empty when there is no `[workspace]`
/// section, no root manifest, or nothing parses — never an error.
pub fn cargo_workspace_crate_map(ctx: &dyn ResolutionContext) -> Arc<CrateMap> {
    let Some(root_toml) = ctx.read_file("Cargo.toml") else {
        return Arc::new(CrateMap::new());
    };

    let mut h = DefaultHasher::new();
    root_toml.hash(&mut h);
    let key = (ctx.project_root().to_path_buf(), h.finish());

    if let Ok(cache) = CACHE.lock()
        && let Some(hit) = cache.get(&key)
    {
        return Arc::clone(hit);
    }

    let map = Arc::new(build_crate_map(&root_toml, ctx));
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, Arc::clone(&map));
    }
    map
}

fn build_crate_map(root_toml: &str, ctx: &dyn ResolutionContext) -> CrateMap {
    let mut map = CrateMap::new();
    for member in workspace_members(root_toml) {
        for dir in expand_member(&member, ctx) {
            let Some(name) = ctx
                .read_file(&format!("{dir}/Cargo.toml"))
                .and_then(|t| package_name(&t))
            else {
                continue;
            };
            // BOTH spellings: the manifest says `my-crate`, every `use` says
            // `my_crate`, and the reference we are resolving is the latter.
            map.insert(name.replace('-', "_"), dir.clone());
            map.insert(name, dir);
        }
    }
    map
}

// =============================================================================
// The hand-rolled parser
// =============================================================================

/// The `members = [...]` array of the `[workspace]` section (single- or
/// multi-line). Anything unparseable yields no members, never an error.
pub fn workspace_members(toml: &str) -> Vec<String> {
    let mut in_workspace = false;
    let mut collecting = false;
    let mut buf = String::new();

    for raw in toml.lines() {
        let line = strip_comment(raw);
        let trimmed = line.trim();

        if !collecting && trimmed.starts_with('[') {
            // `[workspace]` — but NOT `[workspace.dependencies]`, whose members
            // key (if any) is not ours.
            in_workspace = trimmed.trim_end_matches(|c: char| c.is_whitespace()) == "[workspace]";
            continue;
        }
        if !in_workspace {
            continue;
        }

        if !collecting {
            let Some(rest) = trimmed.strip_prefix("members") else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            collecting = true;
            buf.push_str(rest);
        } else {
            buf.push_str(&line);
        }

        if buf.contains(']') {
            break;
        }
        buf.push('\n');
    }

    quoted_strings(&buf)
}

/// The `name = "…"` of the `[package]` section.
pub fn package_name(toml: &str) -> Option<String> {
    let mut in_package = false;
    for raw in toml.lines() {
        let line = strip_comment(raw);
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name")
            && let Some(rest) = rest.trim_start().strip_prefix('=')
            && let Some(name) = quoted_strings(rest).into_iter().next()
        {
            return Some(name);
        }
    }
    None
}

/// Drop a `#` comment — but not one inside a string (`name = "a#b"`).
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for c in line.chars() {
        match quote {
            Some(q) => {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '#' {
                    break;
                }
                if c == '"' || c == '\'' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// Every quoted string in `s`, escape-aware.
fn quoted_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<(char, String)> = None;
    let mut escaped = false;

    for c in s.chars() {
        match cur {
            Some((q, ref mut buf)) => {
                if escaped {
                    buf.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    let (_, done) = cur.take().unwrap_or((q, String::new()));
                    out.push(done);
                } else {
                    buf.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    cur = Some((c, String::new()));
                }
            }
        }
    }
    out
}

// =============================================================================
// Glob expansion
// =============================================================================

/// A member entry → the directories it names. `crates/*` walks; `crates/foo` is
/// itself. Bounded by [`MAX_GLOB_WALK_DEPTH`] and [`SKIP_DIRS`].
fn expand_member(member: &str, ctx: &dyn ResolutionContext) -> Vec<String> {
    let segments: Vec<&str> = member
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() || segments.len() > MAX_GLOB_WALK_DEPTH {
        return Vec::new();
    }

    let mut frontier = vec![String::new()];
    for seg in segments {
        let mut next = Vec::new();
        for base in &frontier {
            match seg {
                // `*` / `**` — every child directory worth descending into.
                "*" | "**" => {
                    for dir in ctx.list_directories(base) {
                        if is_skippable(&dir) {
                            continue;
                        }
                        next.push(join(base, &dir));
                    }
                }
                literal => next.push(join(base, literal)),
            }
        }
        frontier = next;
    }
    frontier
}

fn is_skippable(dir: &str) -> bool {
    dir.starts_with('.') || SKIP_DIRS.contains(&dir)
}

fn join(base: &str, seg: &str) -> String {
    if base.is_empty() {
        seg.to_string()
    } else {
        format!("{base}/{seg}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn members_parse_single_and_multi_line() {
        assert_eq!(
            workspace_members("[workspace]\nmembers = [\"crates/*\"]\n"),
            vec!["crates/*"]
        );
        assert_eq!(
            workspace_members(
                "[workspace]\nmembers = [\n    \"crates/a\",   # the first\n    \"crates/b\",\n]\nresolver = \"2\"\n"
            ),
            vec!["crates/a", "crates/b"],
            "a trailing `#` comment is not a member"
        );
    }

    #[test]
    fn a_workspace_subsection_is_not_the_workspace_section() {
        // `[workspace.dependencies]` must not be mistaken for `[workspace]`, or a
        // `members`-shaped key inside it would poison the map.
        assert!(
            workspace_members("[workspace.dependencies]\nmembers = [\"nope\"]\n").is_empty(),
            "only the [workspace] table declares members"
        );
    }

    #[test]
    fn a_malformed_manifest_yields_no_members_and_no_error() {
        // The whole reason the parser is hand-rolled: this must not be an error.
        assert!(workspace_members("[workspace\nmembers = ").is_empty());
        assert!(workspace_members("").is_empty());
        assert!(package_name("name = \"orphan\"").is_none(), "no [package]");
    }

    #[test]
    fn package_name_reads_only_the_package_table() {
        assert_eq!(
            package_name("[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n").as_deref(),
            Some("my-crate")
        );
        assert_eq!(
            package_name("[dependencies]\nname = \"impostor\"\n[package]\nname = \"real\"\n")
                .as_deref(),
            Some("real")
        );
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        assert_eq!(
            strip_comment(r#"name = "a#b" # tail"#).trim(),
            r#"name = "a#b""#
        );
    }
}
