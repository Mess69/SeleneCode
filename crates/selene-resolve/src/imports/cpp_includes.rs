//! C/C++ include search directories — [`load_cpp_include_dirs`].
//!
//! A C/C++ `#include "foo/bar.h"` is resolved by the compiler against a search
//! path we do not have. Two strategies, in order:
//!
//! 1. **`compile_commands.json`** (the Clang compilation database), looked for
//!    at the project root and in the usual build subdirectories. Its `-I` /
//!    `-isystem` flags *are* the real answer.
//! 2. **Heuristic probing** when no database exists: the convention directories
//!    (`include`, `src`, `lib`, `api`, `inc`) plus any top-level directory that
//!    actually contains headers.
//!
//! Results are repo-relative. Directories outside the project are dropped — they
//! cannot be looked up in the file index anyway (and `/usr/include` is not ours
//! to index).
//!
//! The TS build cached this in a **module-level** map keyed by root; here it is
//! ordinary resolver state (`maps/resolution.md` §Rust port notes — a global
//! mutable cache keyed by path is a lifetime bug waiting to happen).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// The include directories for `project_root`, repo-relative and **sorted**
/// (`read_dir` order is filesystem-defined; a resolver that tried them in a
/// different order on a different machine would bind a header to a different
/// file).
pub fn load_cpp_include_dirs(project_root: &Path) -> Vec<String> {
    load_from_compile_db(project_root).unwrap_or_else(|| load_heuristic(project_root))
}

/// `-I` / `-isystem` directories from a compilation database.
///
/// `None` (not an empty vec) when no database exists — that is what lets the
/// heuristic run. An empty vec means "a database exists and names no include
/// dirs", which is a real answer.
fn load_from_compile_db(project_root: &Path) -> Option<Vec<String>> {
    let db_path = [
        "compile_commands.json",
        "build/compile_commands.json",
        "cmake-build-debug/compile_commands.json",
        "cmake-build-release/compile_commands.json",
        "out/compile_commands.json",
    ]
    .iter()
    .map(|p| project_root.join(p))
    .find(|p| p.exists())?;

    let content = std::fs::read_to_string(&db_path).ok()?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&content).ok()?;

    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        // Each entry names the directory its command ran in; a relative `-I` is
        // relative to THAT, not to the project root.
        let base = entry
            .get("directory")
            .and_then(|d| d.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| project_root.to_path_buf());

        let args: Vec<String> = match entry.get("arguments").and_then(|a| a.as_array()) {
            Some(list) => list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            None => entry
                .get("command")
                .and_then(|c| c.as_str())
                .map(shlex_split)
                .unwrap_or_default(),
        };

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            let include_dir = if let Some(rest) = arg.strip_prefix("-I")
                && !rest.is_empty()
            {
                // `-I<dir>`, no space.
                Some(rest.to_string())
            } else if (arg == "-isystem" || arg == "-I") && i + 1 < args.len() {
                // `-isystem <dir>` / `-I <dir>`, space-separated.
                i += 1;
                Some(args[i].clone())
            } else {
                None
            };

            if let Some(dir) = include_dir {
                let abs = if Path::new(&dir).is_absolute() {
                    PathBuf::from(&dir)
                } else {
                    base.join(&dir)
                };
                if let Some(rel) = relativize(&abs, project_root) {
                    dirs.insert(rel);
                }
            }
            i += 1;
        }
    }

    Some(dirs.into_iter().collect())
}

/// The convention directories, plus any top-level directory holding headers.
fn load_heuristic(project_root: &Path) -> Vec<String> {
    const CONVENTION: [&str; 5] = ["include", "src", "lib", "api", "inc"];

    let Ok(entries) = std::fs::read_dir(project_root) else {
        return Vec::new();
    };

    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };

        if CONVENTION.contains(&name.to_lowercase().as_str()) {
            dirs.insert(name);
            continue;
        }

        // Any other top-level directory that actually contains headers.
        if let Ok(sub) = std::fs::read_dir(project_root.join(&name))
            && sub.flatten().any(|f| {
                f.file_name()
                    .to_str()
                    .map(|n| {
                        let lower = n.to_lowercase();
                        lower.ends_with(".h")
                            || lower.ends_with(".hpp")
                            || lower.ends_with(".hxx")
                            || lower.ends_with(".hh")
                    })
                    .unwrap_or(false)
            })
        {
            dirs.insert(name);
        }
    }

    dirs.into_iter().collect()
}

/// A repo-relative, forward-slashed path — or `None` when `abs` sits outside
/// the project (a system include dir, or a `..` escape).
fn relativize(abs: &Path, project_root: &Path) -> Option<String> {
    let abs = normalize(abs);
    let root = normalize(project_root);
    let rel = abs.strip_prefix(&root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() { None } else { Some(s) }
}

/// Lexical `.`/`..` normalization (no filesystem access — the path may not
/// exist, and `canonicalize` would fail on it).
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

/// A minimal shlex split for a compiler command line.
///
/// Quoted paths with spaces are real (`-I "C:/Program Files/x/include"`), and a
/// naive `split_whitespace` would shred them into two useless args.
///
/// **Known limit, matching the TS build exactly:** a quote is only honored at the
/// START of a token. `-I"a b/include"` (quote mid-token) splits at the space, the
/// same way `import-resolver.ts:shlexSplit` does. The two spellings that matter
/// in a real `compile_commands.json` — `-I "a b"` and `"-Ia b"` — both work.
/// Escapes are honored inside double quotes only (TS's single-quote branch copies
/// bytes verbatim); this is parity, not preference.
fn shlex_split(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let mut arg = String::new();
        match chars[i] {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    // `\"` inside a double-quoted arg is an escaped quote.
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    arg.push(chars[i]);
                    i += 1;
                }
                i += 1; // the closing quote
            }
            '\'' => {
                // Single quotes are LITERAL — no escape processing (TS parity).
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    arg.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            _ => {
                while i < chars.len() && !chars[i].is_whitespace() {
                    arg.push(chars[i]);
                    i += 1;
                }
            }
        }
        out.push(arg);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn shlex_handles_quoted_paths_with_spaces() {
        // The two spellings that actually occur in a compile_commands.json.
        assert_eq!(
            shlex_split(r#"clang -I "a b/include" -isystem 'c d' -Ix -c f.c"#),
            vec![
                "clang",
                "-I",
                "a b/include",
                "-isystem",
                "c d",
                "-Ix",
                "-c",
                "f.c"
            ]
        );
        assert_eq!(
            shlex_split(r#"clang "-Ia b/include" -c f.c"#),
            vec!["clang", "-Ia b/include", "-c", "f.c"]
        );
        // A quote MID-token is NOT honored. The TS build does the same
        // (`shlexSplit` only enters quote mode at a token's first char), and
        // diverging here would resolve headers the TS build does not — a parity
        // deviation that buys nothing, since both real spellings work above.
        assert_eq!(
            shlex_split(r#"clang -I"a b" -c f.c"#),
            vec!["clang", "-I\"a", "b\"", "-c", "f.c"],
            "parity with import-resolver.ts's shlexSplit, limitation included"
        );
        assert_eq!(shlex_split("  "), Vec::<String>::new());
    }

    #[test]
    fn compile_db_arguments_and_command_forms_both_parse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("build")).unwrap();
        std::fs::write(
            dir.path().join("build/compile_commands.json"),
            format!(
                r#"[
                  {{"directory": "{root}", "arguments": ["cc", "-Iinclude", "-isystem", "vendor/inc", "-c", "a.c"]}},
                  {{"directory": "{root}", "command": "cc -I \"third party/h\" -I/usr/include -c b.c"}}
                ]"#,
                root = dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let dirs = load_cpp_include_dirs(dir.path());
        assert!(dirs.contains(&"include".to_string()), "`-Idir` (no space)");
        assert!(dirs.contains(&"vendor/inc".to_string()), "`-isystem dir`");
        assert!(
            dirs.contains(&"third party/h".to_string()),
            "a quoted path with a space survives the shlex split"
        );
        assert!(
            !dirs.iter().any(|d| d.contains("usr")),
            "a system dir outside the project is dropped — it cannot be looked \
             up in the file index anyway"
        );
        assert_eq!(
            dirs,
            {
                let mut sorted = dirs.clone();
                sorted.sort();
                sorted
            },
            "sorted — read order must not vary by machine"
        );
    }

    #[test]
    fn the_heuristic_finds_convention_dirs_and_header_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["include", "src", "vendor", "docs"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        // `vendor` holds a header ⇒ it is an include dir.
        std::fs::write(dir.path().join("vendor/thing.hpp"), "").unwrap();
        // `docs` holds no headers ⇒ it is not.
        std::fs::write(dir.path().join("docs/readme.md"), "").unwrap();

        let dirs = load_cpp_include_dirs(dir.path());
        assert!(dirs.contains(&"include".to_string()));
        assert!(dirs.contains(&"src".to_string()));
        assert!(dirs.contains(&"vendor".to_string()));
        assert!(!dirs.contains(&"docs".to_string()));
    }

    #[test]
    fn a_compile_db_wins_over_the_heuristic_even_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("include")).unwrap();
        std::fs::write(dir.path().join("compile_commands.json"), "[]").unwrap();

        assert!(
            load_cpp_include_dirs(dir.path()).is_empty(),
            "a database that names no include dirs is a real answer — the \
             heuristic must not second-guess it"
        );
    }

    #[test]
    fn a_malformed_compile_db_falls_back_to_the_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("include")).unwrap();
        std::fs::write(dir.path().join("compile_commands.json"), "{not json").unwrap();

        assert_eq!(
            load_cpp_include_dirs(dir.path()),
            vec!["include".to_string()],
            "a broken database degrades to the heuristic, never to an error"
        );
    }
}
