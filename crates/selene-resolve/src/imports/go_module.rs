//! The `go.mod` `module` directive — [`load_go_module`].
//!
//! Without it, Go cross-package calls do not resolve at all: `is_external_import`
//! (Task 5) decides "local" by asking whether the import path starts with the
//! **module path**, so an unknown module path makes every in-module import
//! (`github.com/example/myproject/pkga`) look third-party, and resolution falls
//! through to name-matching-by-proximity — which finds a small fraction of the
//! real call sites, and mis-binds some of them (#388).
//!
//! # Documented non-feature (preserved)
//!
//! Only the **project-root** `go.mod` is read. Nested `go.mod` files (Go
//! workspaces, multi-module monorepos) are not resolved.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::types::GoModule;

/// `module <path>` — the first non-comment directive in any valid `go.mod`.
static MODULE_DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, covered by the tests below
    Regex::new(r"(?m)^\s*module\s+(\S+)\s*$").unwrap()
});

/// Line comments — stripped so a `// module foo` never false-matches.
static LINE_COMMENT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, covered by the tests below
    Regex::new(r"//[^\n]*").unwrap()
});

/// Read `<project_root>/go.mod`. `None` when it does not exist or declares no
/// `module` directive — a miss, never an error.
pub fn load_go_module(project_root: &Path) -> Option<GoModule> {
    let content = std::fs::read_to_string(project_root.join("go.mod")).ok()?;
    let stripped = LINE_COMMENT.replace_all(&content, "");
    let caps = MODULE_DIRECTIVE.captures(&stripped)?;
    let module_path = caps[1].trim_matches(['"', '\'']);
    if module_path.is_empty() {
        return None;
    }
    Some(GoModule {
        module_path: module_path.to_string(),
        // The root-relative directory holding the go.mod. Only the project root
        // is read (see the module docs), so it is always "".
        root_dir: String::new(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn with_go_mod(content: &str) -> Option<GoModule> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), content).unwrap();
        load_go_module(dir.path())
    }

    #[test]
    fn reads_the_module_directive() {
        let m = with_go_mod("module github.com/example/myproject\n\ngo 1.22\n").unwrap();
        assert_eq!(m.module_path, "github.com/example/myproject");
    }

    #[test]
    fn a_commented_out_module_line_does_not_false_match() {
        let m =
            with_go_mod("// module github.com/wrong/one\nmodule github.com/right/one\n\ngo 1.22\n")
                .unwrap();
        assert_eq!(m.module_path, "github.com/right/one");
    }

    #[test]
    fn quoting_is_stripped() {
        let m = with_go_mod("module \"github.com/example/q\"\n").unwrap();
        assert_eq!(m.module_path, "github.com/example/q");
    }

    #[test]
    fn absence_is_a_miss_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_go_module(dir.path()).is_none(), "no go.mod");
        assert!(
            with_go_mod("go 1.22\nrequire x v1.0.0\n").is_none(),
            "a go.mod with no module directive"
        );
    }
}
