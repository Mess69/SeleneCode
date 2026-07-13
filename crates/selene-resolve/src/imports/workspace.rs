//! npm / yarn / bun / pnpm workspace members — [`load_workspace_packages`] and
//! [`resolve_workspace_import`].
//!
//! A cross-package import like `@scope/ui/widgets` is **local** to the monorepo,
//! but to a single-package resolver it looks exactly like a third-party npm
//! specifier — so `is_external_import` (Task 5) would call it external and the
//! consumer↔definition edge would never exist. For component barrels that
//! surfaced as a false **"0 callers" on a live component** (#629).
//!
//! # Scope (deliberately small, mirroring the TS build)
//!
//! - `workspaces` from `package.json` (both the array and the
//!   `{ packages: [...] }` object forms), plus `pnpm-workspace.yaml`'s
//!   `packages:` list (a minimal line scanner — no YAML dependency);
//! - **one level** of `*` glob expansion (`packages/*`, `apps/*`);
//! - subpath resolution is directory-based (`@scope/ui/sub` → `<ui>/sub`); a
//!   member's `exports` map is not honored.
//!
//! # Deferred: ohpm (`oh-package.json5`)
//!
//! ArkTS/HarmonyOS modules declare siblings as `file:` deps in
//! `oh-package.json5`, and the TS build walks for them (bounded BFS) and records
//! their `main` as an ENTRY file. ArkTS is a **wave-2** language (Phase 8), so
//! that walk is not ported here. [`WorkspacePackages::entry_by_name`] stays on
//! the struct and stays **empty**: populating it from npm's `main` instead would
//! be a *new* behavior the TS build does not have, and it would silently add
//! edges the parity gate would then have to explain away.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::types::WorkspacePackages;

#[derive(Debug, Deserialize)]
struct RootManifest {
    workspaces: Option<Workspaces>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Workspaces {
    /// npm / bun: `"workspaces": ["packages/*"]`
    List(Vec<String>),
    /// yarn: `"workspaces": { "packages": [...], "nohoist": [...] }`
    Object { packages: Vec<String> },
}

#[derive(Debug, Deserialize)]
struct MemberManifest {
    name: Option<String>,
}

/// Load the workspace members declared at `project_root`.
///
/// `None` when the project declares no workspaces — the common single-package
/// case pays nothing and behaves exactly as before.
pub fn load_workspace_packages(project_root: &Path) -> Option<WorkspacePackages> {
    let mut by_name: HashMap<String, String> = HashMap::new();

    for pattern in read_workspace_globs(project_root) {
        for dir in expand_one_level_glob(project_root, &pattern) {
            if let Some(name) = read_package_name(&project_root.join(&dir)) {
                // **First declaration wins** — patterns are tried in order.
                by_name.entry(name).or_insert(dir);
            }
        }
    }

    if by_name.is_empty() {
        return None;
    }
    Some(WorkspacePackages {
        by_name,
        // ohpm only (wave 2) — see the module docs.
        entry_by_name: HashMap::new(),
    })
}

/// Rewrite a bare specifier to a repo-relative path, when it names a workspace
/// member.
///
/// The **longest** matching package name wins, so `@scope/ui/core` prefers a
/// `@scope/ui/core` member over a `@scope/ui` one when both exist.
pub fn resolve_workspace_import(import_path: &str, ws: &WorkspacePackages) -> Option<String> {
    let mut best: Option<&String> = None;
    for name in ws.by_name.keys() {
        let matches = import_path == name || import_path.starts_with(&format!("{name}/"));
        if matches && best.is_none_or(|b| name.len() > b.len()) {
            best = Some(name);
        }
    }
    let best = best?;
    let dir = ws.by_name.get(best)?;
    let subpath = &import_path[best.len()..]; // "" or "/widgets"

    // A bare member import resolves to the member's declared ENTRY file when its
    // manifest names one (ohpm `main`) — the caller's exact-path check then hits
    // it without extension/index guessing. Empty in v0 (see the module docs).
    if subpath.is_empty()
        && let Some(entry) = ws.entry_by_name.get(best)
    {
        return Some(entry.clone());
    }

    let joined = format!("{dir}{subpath}");
    // Collapse any `//` the join produced.
    Some(collapse_slashes(&joined))
}

fn collapse_slashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_slash = false;
    for ch in s.chars() {
        if ch == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        out.push(ch);
    }
    out
}

/// The glob patterns declared by `package.json` and `pnpm-workspace.yaml`.
fn read_workspace_globs(project_root: &Path) -> Vec<String> {
    let mut out = Vec::new();

    if let Ok(text) = std::fs::read_to_string(project_root.join("package.json"))
        && let Ok(manifest) = serde_json::from_str::<RootManifest>(&text)
    {
        match manifest.workspaces {
            Some(Workspaces::List(list)) => out.extend(list),
            Some(Workspaces::Object { packages }) => out.extend(packages),
            None => {}
        }
    }

    if let Ok(yaml) = std::fs::read_to_string(project_root.join("pnpm-workspace.yaml")) {
        out.extend(parse_pnpm_packages(&yaml));
    }

    out
}

/// The only shape pnpm actually uses:
///
/// ```yaml
/// packages:
///   - 'packages/*'
///   - "apps/*"
///   - tools/build
/// ```
///
/// A minimal line scanner, deliberately — a YAML dependency for four lines of
/// grammar is not worth its supply chain.
fn parse_pnpm_packages(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_packages = false;

    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("packages") && trimmed[8..].trim_start().starts_with(':') {
            in_packages = true;
            continue;
        }
        if !in_packages {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix('-') {
            let item = item.trim().trim_matches(['\'', '"']);
            if !item.is_empty() {
                out.push(item.to_string());
            }
            continue;
        }
        // A non-list, non-blank, non-indented line ends the block.
        if !trimmed.is_empty() && !line.starts_with([' ', '\t']) {
            in_packages = false;
        }
    }
    out
}

/// Expand one level of a `packages/*` glob into member directories.
///
/// Skips dotdirs and `node_modules`. Output is **sorted**: `read_dir` order is
/// filesystem-defined, and it would otherwise leak into which member wins a
/// first-declaration-wins tie.
fn expand_one_level_glob(project_root: &Path, pattern: &str) -> Vec<String> {
    let norm = pattern.replace('\\', "/");
    let norm = norm.trim_end_matches('/');

    let Some(star) = norm.find('*') else {
        return vec![norm.to_string()]; // an exact directory
    };

    let base = norm[..star].trim_end_matches('/');
    let Ok(entries) = std::fs::read_dir(project_root.join(base)) else {
        return Vec::new();
    };

    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && name != "node_modules")
        .map(|name| {
            if base.is_empty() {
                name
            } else {
                format!("{base}/{name}")
            }
        })
        .collect();
    out.sort();
    out
}

/// A member directory's declared package `name`.
fn read_package_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: MemberManifest = serde_json::from_str(&text).ok()?;
    manifest.name.filter(|n| !n.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn member(root: &Path, dir: &str, name: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("package.json"), format!(r#"{{"name": "{name}"}}"#)).unwrap();
    }

    #[test]
    fn npm_workspaces_array_and_glob_expansion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["packages/*", "tools/build"]}"#,
        )
        .unwrap();
        member(dir.path(), "packages/ui", "@scope/ui");
        member(dir.path(), "packages/core", "@scope/core");
        member(dir.path(), "tools/build", "buildtool");
        // Skipped by the walk.
        std::fs::create_dir_all(dir.path().join("packages/node_modules")).unwrap();
        std::fs::create_dir_all(dir.path().join("packages/.cache")).unwrap();

        let ws = load_workspace_packages(dir.path()).unwrap();
        assert_eq!(ws.by_name.get("@scope/ui").unwrap(), "packages/ui");
        assert_eq!(ws.by_name.get("@scope/core").unwrap(), "packages/core");
        assert_eq!(ws.by_name.get("buildtool").unwrap(), "tools/build");
        assert_eq!(ws.by_name.len(), 3, "node_modules and dotdirs are skipped");
    }

    #[test]
    fn yarn_object_form_and_pnpm_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": {"packages": ["apps/*"], "nohoist": ["**/x"]}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'libs/*'\n  - \"tools/cli\"\n\nother: 1\n",
        )
        .unwrap();
        member(dir.path(), "apps/web", "web");
        member(dir.path(), "libs/util", "@x/util");
        member(dir.path(), "tools/cli", "cli");

        let ws = load_workspace_packages(dir.path()).unwrap();
        assert_eq!(ws.by_name.get("web").unwrap(), "apps/web");
        assert_eq!(ws.by_name.get("@x/util").unwrap(), "libs/util");
        assert_eq!(ws.by_name.get("cli").unwrap(), "tools/cli");
    }

    #[test]
    fn the_longest_matching_package_name_wins() {
        let mut by_name = HashMap::new();
        by_name.insert("@scope/ui".to_string(), "packages/ui".to_string());
        by_name.insert("@scope/ui/core".to_string(), "packages/ui-core".to_string());
        let ws = WorkspacePackages {
            by_name,
            entry_by_name: HashMap::new(),
        };

        assert_eq!(
            resolve_workspace_import("@scope/ui/core/x", &ws).unwrap(),
            "packages/ui-core/x",
            "`@scope/ui/core` beats `@scope/ui`"
        );
        assert_eq!(
            resolve_workspace_import("@scope/ui/widgets", &ws).unwrap(),
            "packages/ui/widgets"
        );
        assert_eq!(
            resolve_workspace_import("@scope/ui", &ws).unwrap(),
            "packages/ui",
            "a bare member import resolves to its directory"
        );
        assert!(
            resolve_workspace_import("@other/pkg", &ws).is_none(),
            "a non-member specifier stays external"
        );
        assert!(
            resolve_workspace_import("@scope/uix", &ws).is_none(),
            "a PREFIX collision is not a match — `@scope/uix` is not `@scope/ui`"
        );
    }

    #[test]
    fn a_declared_entry_file_short_circuits_a_bare_import() {
        let mut by_name = HashMap::new();
        by_name.insert("data".to_string(), "core/data".to_string());
        let mut entry_by_name = HashMap::new();
        entry_by_name.insert("data".to_string(), "core/data/Index.ets".to_string());
        let ws = WorkspacePackages {
            by_name,
            entry_by_name,
        };

        assert_eq!(
            resolve_workspace_import("data", &ws).unwrap(),
            "core/data/Index.ets",
            "a BARE member import takes the declared entry file"
        );
        assert_eq!(
            resolve_workspace_import("data/sub", &ws).unwrap(),
            "core/data/sub",
            "a subpath import ignores the entry and resolves by directory"
        );
    }

    #[test]
    fn a_single_package_repo_declares_no_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "solo"}"#).unwrap();
        assert!(
            load_workspace_packages(dir.path()).is_none(),
            "no workspaces ⇒ None ⇒ the caller skips all workspace logic"
        );

        let empty = tempfile::tempdir().unwrap();
        assert!(
            load_workspace_packages(empty.path()).is_none(),
            "no package.json at all"
        );
    }
}
