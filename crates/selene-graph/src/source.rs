//! Source access — **the only place this workspace reads source text off disk for output**.
//!
//! # Read parity is the product
//!
//! The whole bet is that an agent answers a structural question **without opening a file**.
//! That only works if what we hand back is as good as what `Read` would have handed back —
//! and "as good as" includes the boring part: **an agent must be able to cite `file:line`
//! from our output exactly as if it had Read the file.** If our line numbers are off by one,
//! or padded differently, the agent's citation is wrong and the next tool call it makes is
//! `Read`, to check. So the numbering is byte-for-byte:
//!
//! ```text
//! 1\tconst a = 1
//! 2\t
//! 1000\t  const v998
//! ```
//!
//! `format!("{n}\t{line}")`. **No padding** — `1000` is not right-aligned against `1`. A
//! **trailing empty line is kept** (a file ending in `\n` has a final empty line, and `Read`
//! shows it). Both are asserted against the exact bytes.
//!
//! # `validate_path_within_root` (#527) is the ONE `isError` source in Phase 4
//!
//! Every disk read in this workspace goes through it. It is the only place a `selene-graph`
//! call *chooses* to fail, and it fails on exactly one thing: a path that escapes the
//! project root. Not "the file is missing" (that is `Ok(None)`), not "the offset is past the
//! end" (that is success-shaped guidance) — an actual traversal attempt.
//!
//! # Config leaves render KEYS ONLY (#383)
//!
//! A `.env` / `json` / `yaml` / `toml` / `properties` leaf is rendered as its **key list**,
//! never its values. `API_KEY=sk-live-abc` must never reach an agent's context window
//! through this crate. The same guard Phase 3 put on config *nodes* now covers config
//! *source*.

use std::path::{Path, PathBuf};

use selene_core::Node;
use selene_db::GraphStore;

use crate::error::{GraphError, Result};
use crate::query::{QueryManager, normalize_path};

/// Languages whose source is rendered as **keys only** — never values (#383).
pub const CONFIG_LEAF_LANGUAGES: &[&str] = &["json", "yaml", "toml", "properties", "env", "ini"];

/// The default `limit` for a file view. Ported verbatim from the agent's `Read`.
pub const DEFAULT_READ_LIMIT: usize = 2000;

/// The char budget for a file view.
///
/// ⚠ **38 000 predates the host's 24K/25K externalization cap**, so a file-view result *can*
/// exceed the inline cap and be externalized by the host. That is **known and accepted**: it
/// is what Read parity costs, and clipping to 24K here would make our output *worse* than
/// `Read` on exactly the files an agent most wants to see. Do not "fix" it.
pub const CHAR_BUDGET: usize = 38_000;

/// A slice of a file, as the agent's `Read` would have produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSlice {
    /// Project-relative path.
    pub path: String,
    /// The numbered text (`<n>\t<line>`).
    pub text: String,
    /// How many lines the file has in total.
    pub total_lines: usize,
    /// Whether the slice was cut short by `limit` or [`CHAR_BUDGET`].
    pub truncated: bool,
}

/// **#527 — refuse any path that escapes the project root.**
///
/// Canonicalizes both sides (so a symlink pointing out is caught, not just a literal `..`)
/// and compares. The one deliberate error in this crate.
pub fn validate_path_within_root(root: &Path, path: &str) -> Result<PathBuf> {
    let refuse = || GraphError::PathRefusal {
        path: path.to_string(),
    };

    // 1. Lexical, on the RAW path — before normalization.
    //
    //    ⚠ `normalize_path` STRIPS a leading `/` (it exists to turn the agent's four
    //    spellings into one relative path), so an absolute `/etc/passwd` normalizes to
    //    `etc/passwd` and sails straight through an is_absolute() check performed after it.
    //    The absoluteness test must therefore run FIRST, on what the caller actually wrote.
    //    (Caught by the escape test; it is the kind of thing that reads as correct in
    //    review and is a path-traversal hole in production.)
    let raw = path.replace('\\', "/");
    let is_absolute = raw.starts_with('/')
        // A Windows drive or UNC: `C:\…`, `\\server\share`.
        || raw.chars().nth(1) == Some(':')
        || raw.starts_with("//");
    if is_absolute || raw.split('/').any(|seg| seg == "..") {
        return Err(refuse());
    }

    let relative = normalize_path(path);

    // 2. Anchor on the CANONICAL root and build the candidate from it.
    //
    //    ⚠ The candidate is NOT canonicalized when it does not exist — and that distinction
    //    is the whole bug this shape fixes. `canonicalize()` fails on a missing file, and a
    //    fallback to the raw join then compares `/var/...` against a canonical
    //    `/private/var/...` (macOS symlinks its temp dir), so **a DELETED file came back as
    //    a PathRefusal** — an `isError` for the most ordinary fact there is. `get_code` is
    //    supposed to answer `Ok(None)` there. One isError early and the agent abandons the
    //    tool; this one would have fired on any file deleted since the last index.
    let canonical_root = root.canonicalize().map_err(|source| GraphError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let candidate = canonical_root.join(&relative);

    // 3. If it EXISTS, canonicalize it too — that is what catches a symlink pointing out of
    //    the tree, which no lexical check can see.
    if candidate.exists() {
        let canonical = candidate.canonicalize().map_err(|source| GraphError::Io {
            path: candidate.clone(),
            source,
        })?;
        if !canonical.starts_with(&canonical_root) {
            return Err(refuse());
        }
        return Ok(canonical);
    }

    Ok(candidate)
}

/// `<n>\t<line>` — **no padding**, trailing empty line kept.
///
/// This is the agent's `Read` format, byte for byte. See the module docs for why an off-by-
/// one or a padded number costs us the whole anti-Read bet.
pub fn number_lines(text: &str, start_line: usize) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    // `split('\n')` — NOT `lines()`. `lines()` drops the final empty element of a
    // newline-terminated file, and `Read` shows that line.
    for (i, line) in text.split('\n').enumerate() {
        out.push_str(&format!("{}\t{}\n", start_line + i, line));
    }
    out
}

/// Render a config file as its **keys**, never its values (#383).
fn config_keys_only(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        // `KEY=value`, `key: value`, `"key": value` — take everything left of the separator.
        if let Some(idx) = trimmed.find(['=', ':']) {
            let key = trimmed[..idx].trim().trim_matches(['"', '\'', ',']);
            if !key.is_empty() {
                out.push_str(key);
                out.push('\n');
            }
        }
    }
    out
}

fn is_config_leaf(language: &str) -> bool {
    CONFIG_LEAF_LANGUAGES.contains(&language)
}

impl<S: GraphStore> QueryManager<S> {
    /// The source text of a node — sliced `[start_line, end_line]` **inclusive** off disk.
    ///
    /// The DB holds coordinates, not bodies (spike, Task 1). A node whose file has since
    /// been deleted is `Ok(None)`, **never** `Err`: the file being gone is an ordinary,
    /// recoverable fact, and the tool layer renders it as guidance.
    pub async fn get_code(&self, node_id: &str) -> Result<Option<String>> {
        let Some(node) = self.store().get_node(node_id).await? else {
            return Ok(None);
        };
        self.code_of(&node)
    }

    /// [`Self::get_code`] for a node you already hold.
    pub fn code_of(&self, node: &Node) -> Result<Option<String>> {
        let path = validate_path_within_root(self.root(), &node.file_path)?; // #527

        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None); // deleted since indexing — a fact, not an error
        };

        // #383: a config leaf yields its keys, never its values.
        if is_config_leaf(&node.language) {
            return Ok(Some(config_keys_only(&text)));
        }

        let lines: Vec<&str> = text.split('\n').collect();
        let start = (node.start_line.max(1) as usize) - 1;
        let end = (node.end_line.max(node.start_line) as usize).min(lines.len());
        if start >= lines.len() {
            return Ok(None);
        }

        // INCLUSIVE — the spike proved `end_line` names the last line of the body, and an
        // exclusive slice would cut the closing line off every function in the product.
        Ok(Some(lines[start..end].join("\n")))
    }

    /// A file view with **`Read` parity**: 1-based `offset`, `limit` lines, `<n>\t<line>`.
    ///
    /// An `offset` past the end is **success-shaped** — a `FileSlice` whose `text` says so —
    /// because "you asked past the end" is guidance, not a malfunction.
    pub async fn read_file_slice(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<FileSlice> {
        let normalized = normalize_path(path);
        let disk = validate_path_within_root(self.root(), path)?; // #527

        let Ok(text) = std::fs::read_to_string(&disk) else {
            return Ok(FileSlice {
                path: normalized,
                text: String::new(),
                total_lines: 0,
                truncated: false,
            });
        };

        let lines: Vec<&str> = text.split('\n').collect();
        let total_lines = lines.len();
        let start = offset.max(1) - 1; // 1-based, like Read
        let limit = if limit == 0 {
            DEFAULT_READ_LIMIT
        } else {
            limit
        };

        if start >= total_lines {
            return Ok(FileSlice {
                path: normalized,
                text: String::new(),
                total_lines,
                truncated: false,
            });
        }

        let end = (start + limit).min(total_lines);
        let mut body = lines[start..end].join("\n");

        let mut truncated = end < total_lines;
        if body.len() > CHAR_BUDGET {
            let cut = body
                .char_indices()
                .take_while(|(i, _)| *i < CHAR_BUDGET)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            body.truncate(cut);
            truncated = true;
        }

        Ok(FileSlice {
            path: normalized,
            text: number_lines(&body, start + 1),
            total_lines,
            truncated,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The exact bytes. This is the assertion the whole anti-Read bet rests on.
    #[test]
    fn line_numbering_has_no_padding_and_keeps_the_trailing_empty_line() {
        let numbered = number_lines("const a = 1\nconst b = 2\n", 1);
        assert_eq!(
            numbered, "1\tconst a = 1\n2\tconst b = 2\n3\t\n",
            "a file ending in a newline HAS a final empty line, and `Read` shows it"
        );

        // No padding: `1000` is not right-aligned against `999`.
        let wide = number_lines("  const v998\n", 1000);
        assert_eq!(wide, "1000\t  const v998\n1001\t\n");
        assert!(
            !wide.contains(" 1000"),
            "if we pad and Read does not, the agent's file:line citation is wrong — and \
             its next tool call is Read, to check"
        );
    }

    /// #383 — the value never appears. Not once, not truncated, not "redacted".
    #[test]
    fn a_config_leaf_renders_keys_and_never_values() {
        let rendered = config_keys_only("API_KEY=sk-live-abc\n# a comment\nDEBUG=true\n");
        assert_eq!(rendered, "API_KEY\nDEBUG\n");
        assert!(
            !rendered.contains("sk-live-abc"),
            "a secret that reaches an agent's context window has left the machine"
        );
    }

    #[test]
    fn path_refusal_catches_the_obvious_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        for escape in ["../../etc/passwd", "/etc/passwd", "src/../../outside.rs"] {
            assert!(
                validate_path_within_root(root, escape).is_err(),
                "{escape} must be REFUSED — this is the one error this crate chooses to \
                 return"
            );
        }

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn main() {}\n").unwrap();
        assert!(validate_path_within_root(root, "src/a.rs").is_ok());
        assert!(
            validate_path_within_root(root, "./src/a.rs").is_ok(),
            "the normalizer runs first (#426)"
        );
    }
}
