//! Git hooks — keep the index fresh after `git commit`/`merge`/`checkout` even with no daemon.
//!
//! A daemon with its FileWatcher keeps the graph current while it runs; git hooks cover the other
//! case — you commit, the index updates for the next agent session. Each hook runs `selene sync` in
//! the background so it never slows the git operation down.
//!
//! # Marker-fenced, and we only ever touch our own block
//!
//! The three hook files (`post-commit`, `post-merge`, `post-checkout`) are often the user's, with
//! their own lines. We insert exactly one block fenced by [`MARKER_BEGIN`]/[`MARKER_END`] and, on
//! removal, take only that block — deleting the file solely when nothing but a shebang remains.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Byte-exact fences. Detection and stripping both key on [`MARKER_BEGIN`].
pub const MARKER_BEGIN: &str = "# >>> selene sync hook >>>";
pub const MARKER_END: &str = "# <<< selene sync hook <<<";

/// The three hooks that should refresh the index.
const HOOKS: &[&str] = &["post-commit", "post-merge", "post-checkout"];

/// What happened to one hook file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookResult {
    pub path: PathBuf,
    pub action: &'static str, // "installed" | "updated" | "unchanged" | "removed" | "not-found"
}

/// The git hooks directory for `root`, honoring `core.hooksPath` and worktrees. `None` when `root`
/// is not a git repository (git-hooks are simply skipped there).
pub fn hooks_dir(root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rel.is_empty() {
        return None;
    }
    let p = PathBuf::from(&rel);
    Some(if p.is_absolute() { p } else { root.join(p) })
}

/// The marked block that runs `selene sync` in the background. `binary` is selene's absolute path.
fn block(binary: &Path) -> String {
    let bin = binary.to_string_lossy();
    format!(
        "{MARKER_BEGIN}\n\
         # Keeps the SeleneCode index fresh after git operations. Managed by `selene`; edits here are overwritten.\n\
         if [ -x \"{bin}\" ]; then ( \"{bin}\" sync >/dev/null 2>&1 & ) >/dev/null 2>&1; fi\n\
         {MARKER_END}"
    )
}

/// Remove any existing selene block from `text`, returning the remainder (trailing ws trimmed).
fn strip_block(text: &str) -> String {
    let Some(begin) = text.find(MARKER_BEGIN) else {
        // Trim consistently whether or not a block was present, so install is idempotent.
        return text.trim_end().to_string();
    };
    // End of block = end of the MARKER_END line (include its newline if present).
    let after_marker = match text[begin..].find(MARKER_END) {
        Some(rel) => begin + rel + MARKER_END.len(),
        None => text.len(),
    };
    let mut end = after_marker;
    if text[end..].starts_with('\n') {
        end += 1;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..begin]);
    out.push_str(&text[end..]);
    out.trim_end().to_string()
}

/// Is this hook file managed by selene?
pub fn has_block(text: &str) -> bool {
    text.contains(MARKER_BEGIN)
}

/// The content a hook file should have after installing our block into `existing` (or `None` for a
/// file that does not exist yet).
fn install_content(existing: Option<&str>, binary: &Path) -> String {
    let block = block(binary);
    match existing {
        None => format!("#!/bin/sh\n{block}\n"),
        Some(text) => {
            let base = strip_block(text);
            let trimmed = base.trim();
            // A file that is empty or just our own shebang should reproduce the new-file form
            // exactly — otherwise re-installing over our block would grow a blank line each time.
            if trimmed.is_empty() || trimmed == "#!/bin/sh" {
                format!("#!/bin/sh\n{block}\n")
            } else {
                format!("{base}\n\n{block}\n")
            }
        }
    }
}

/// Install (or refresh) the selene sync hooks under `root`. No-op (empty vec) outside a git repo.
pub fn install(root: &Path, binary: &Path) -> Result<Vec<HookResult>> {
    let Some(dir) = hooks_dir(root) else {
        return Ok(Vec::new());
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;

    let mut out = Vec::new();
    for name in HOOKS {
        let path = dir.join(name);
        let existing = std::fs::read_to_string(&path).ok();
        let content = install_content(existing.as_deref(), binary);
        let action = match &existing {
            Some(cur) if *cur == content => "unchanged",
            Some(_) => "updated",
            None => "installed",
        };
        if action != "unchanged" {
            std::fs::write(&path, &content).with_context(|| format!("write {}", path.display()))?;
            make_executable(&path);
        }
        out.push(HookResult { path, action });
    }
    Ok(out)
}

/// Remove the selene block from every managed hook under `root`. A file left with only a shebang is
/// deleted; one with other content is rewritten.
pub fn remove(root: &Path) -> Result<Vec<HookResult>> {
    let Some(dir) = hooks_dir(root) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for name in HOOKS {
        let path = dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            out.push(HookResult { path, action: "not-found" });
            continue;
        };
        if !has_block(&text) {
            out.push(HookResult { path, action: "not-found" });
            continue;
        }
        let remainder = strip_block(&text);
        if remainder.trim().is_empty() || remainder.trim() == "#!/bin/sh" {
            std::fs::remove_file(&path).with_context(|| format!("rm {}", path.display()))?;
        } else {
            std::fs::write(&path, format!("{remainder}\n"))
                .with_context(|| format!("write {}", path.display()))?;
            make_executable(&path);
        }
        out.push(HookResult { path, action: "removed" });
    }
    Ok(out)
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn bin() -> PathBuf {
        PathBuf::from("/abs/selene")
    }

    #[test]
    fn install_into_a_new_file_adds_a_shebang_and_the_block() {
        let c = install_content(None, &bin());
        assert!(c.starts_with("#!/bin/sh\n"));
        assert!(c.contains(MARKER_BEGIN) && c.contains(MARKER_END));
        assert!(c.contains("/abs/selene\" sync"));
    }

    #[test]
    fn install_preserves_the_users_existing_hook() {
        let user = "#!/bin/sh\necho hello\n";
        let c = install_content(Some(user), &bin());
        assert!(c.contains("echo hello"), "the user's line survives");
        assert!(c.contains(MARKER_BEGIN));
    }

    #[test]
    fn reinstall_is_idempotent() {
        let once = install_content(None, &bin());
        let twice = install_content(Some(&once), &bin());
        assert_eq!(once, twice, "installing over our own block does not duplicate it");
    }

    #[test]
    fn strip_removes_only_our_block() {
        let text = format!("#!/bin/sh\necho hi\n\n{}\n", block(&bin()));
        let stripped = strip_block(&text);
        assert!(stripped.contains("echo hi"));
        assert!(!stripped.contains(MARKER_BEGIN));
    }
}
