//! Path helpers (repo-relative, forward-slashed, lexical).

use std::path::{Component, Path, PathBuf};

// =============================================================================
// Path helpers (repo-relative, forward-slashed, lexical)
// =============================================================================

/// The directory holding `file` — `""` for a root-level file.
pub(super) fn parent_dir(file: &str) -> String {
    match file.rfind('/') {
        Some(i) => file[..i].to_string(),
        None => String::new(),
    }
}

/// Join `rel` onto `dir` and normalize `.`/`..` **lexically**.
///
/// Never touches the filesystem: every path here is a candidate we are about to
/// *look up* in the file index, and `canonicalize` would fail on the ones that
/// do not exist — which is most of them.
pub(super) fn join_rel(dir: &str, rel: &str) -> String {
    let joined = if dir.is_empty() {
        PathBuf::from(rel)
    } else {
        Path::new(dir).join(rel)
    };

    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn join_rel_normalizes_lexically() {
        assert_eq!(join_rel("src/lib", "./util"), "src/lib/util");
        assert_eq!(join_rel("src/lib", "../util"), "src/util");
        assert_eq!(join_rel("src/a/b", "../../c/d"), "src/c/d");
        assert_eq!(join_rel("", "./x"), "x");
    }

    #[test]
    fn parent_dir_of_a_root_file_is_empty() {
        assert_eq!(parent_dir("main.go"), "");
        assert_eq!(parent_dir("src/a/b.ts"), "src/a");
    }
}
