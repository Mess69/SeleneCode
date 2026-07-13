//! `QueryManager<S>` — **the** query surface every upper layer talks to.
//!
//! # Deliberately thin
//!
//! Traversal already lives in SurrealQL (the locked SurrealQL-max decision): recursive
//! `.{1..n}` walks, shortest-path, impact radius. So this is not a graph engine — it is the
//! seam that turns a `GraphStore` into the vocabulary `selene-context` and `selene-mcp`
//! speak, plus the three things the store genuinely does not know:
//!
//! - **where the project root is** (the store holds paths, not a root),
//! - **what the project is called** (`project_name_tokens`, #720),
//! - **what the source text says** (the DB holds coordinates; bodies live on disk — Task 4).
//!
//! # `S: GraphStore`, never `dyn`
//!
//! `GraphStore` uses RPITIT (`async fn` in trait), so it is not object-safe. The type
//! parameter is threaded through every layer above this one. Do not reach for
//! `Box<dyn GraphStore>`; it does not exist and the trait's own docs say why.
//!
//! # Nothing here is an error unless it is a malfunction
//!
//! `is_indexed()` on an empty store is `Ok(false)`, not `Err`. A file with no dependents is
//! `Ok(vec![])`. See `error.rs`: the set of things allowed to be an error is enumerated,
//! and "the answer is nothing" is not in it.

use std::path::{Path, PathBuf};

use selene_db::{GraphStats, GraphStore};

use crate::error::Result;

/// One row of `files()` — the shape the `files` tool and the budget tiers consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Project-relative path.
    pub path: String,
    /// Wire language string (`"typescript"`).
    pub language: String,
    /// How many nodes this file contributes to the graph.
    pub node_count: u64,
}

/// The query surface.
pub struct QueryManager<S: GraphStore> {
    store: S,
    root: PathBuf,
}

impl<S: GraphStore> QueryManager<S> {
    /// Wrap a store and the project root it was indexed from.
    pub fn new(store: S, root: PathBuf) -> Self {
        Self { store, root }
    }

    /// The project root every path in the graph is relative to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The store, for the layers that need a primitive this surface does not wrap.
    pub fn store(&self) -> &S {
        &self.store
    }

    // =========================================================================
    // Stats, files, project metadata
    // =========================================================================

    /// Node/edge/file counts and the per-kind histogram.
    pub async fn stats(&self) -> Result<GraphStats> {
        Ok(self.store.stats().await?)
    }

    /// How many files are indexed. **This is what drives the explore budget tiers**
    /// (Task 8), so it is a first-class method rather than `stats().files`.
    pub async fn file_count(&self) -> Result<u64> {
        Ok(self.store.stats().await?.files)
    }

    /// Every indexed file, **sorted by path**, with its language and node count.
    ///
    /// # One round-trip
    ///
    /// The spike (Task 1) established that `FileRecord` already carries `node_count`, so
    /// this is a pure map over `all_files()` — *not* the O(files) fan-out of one
    /// `get_nodes_by_file` per file that the plan feared. If a future `FileRecord` ever
    /// drops the count, this becomes a fan-out and the cost note comes back with it.
    pub async fn files(&self) -> Result<Vec<FileInfo>> {
        let mut out: Vec<FileInfo> = self
            .store
            .all_files()
            .await?
            .into_iter()
            .map(|f| FileInfo {
                path: f.path,
                language: f.language,
                node_count: u64::from(f.node_count),
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path)); // determinism: the output is rendered
        Ok(out)
    }

    /// Files that import/reference `path` (who breaks if this changes).
    pub async fn file_dependents(&self, path: &str) -> Result<Vec<String>> {
        Ok(self
            .store
            .dependent_file_paths(&normalize_path(path))
            .await?)
    }

    /// Files `path` imports/references (what this needs).
    pub async fn file_dependencies(&self, path: &str) -> Result<Vec<String>> {
        Ok(self
            .store
            .dependency_file_paths(&normalize_path(path))
            .await?)
    }

    /// **Is anything indexed at all?**
    ///
    /// This is the not-indexed seam, and its contract is exactly two states:
    ///
    /// - `Ok(false)` — a `.selene/` exists and holds **zero files**. The caller renders
    ///   success-shaped guidance ("run `selene index`").
    /// - `Ok(true)` — there is a graph.
    ///
    /// "There is no `.selene/` at all" is **not** this method's job: the caller's walk-up
    /// already failed, and that is `selene-mcp`'s to report. Either way, **never `Err`** —
    /// an un-indexed project is the single most common first contact an agent has with this
    /// tool, and answering it with an error is how the tool gets abandoned.
    pub async fn is_indexed(&self) -> Result<bool> {
        Ok(self.file_count().await? > 0)
    }

    /// The project's name, tokenized — the #720 exclusion list.
    ///
    /// Explore biases toward PascalCase symbol overlap; a token that names the **whole
    /// repo** has no discriminative value (in `selene`, every query "matches" `selene`), so
    /// ranking subtracts these.
    ///
    /// **Derived from the root directory name** (spike, Task 1): it is the one identifier
    /// every ecosystem has. A Rust repo has no `package.json`; a Python repo may have no
    /// manifest at all; and a manifest's `name` can disagree with the checkout. A
    /// manifest-based name, if ever wanted, is a *fallback on top* — never a replacement.
    pub async fn project_name_tokens(&self) -> Result<Vec<String>> {
        Ok(tokenize_project_name(
            self.root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        ))
    }

    // =========================================================================
    // Task 3 fills these (symbol resolution) — see the sequencing table.
    // Task 4 fills adjacency + source access.
    //
    // They are named here, in the one `impl` block all three tasks extend, so that the
    // shape of the surface is visible from the start and two tasks cannot invent two
    // different spellings of the same method.
    // =========================================================================
}

/// **Path normalization (#426)** — every path-taking method funnels through this.
///
/// The agent writes paths four ways and means one thing: `./src/a.ts`, `src/a.ts`,
/// `src\a.ts`, and (for the root) `/`, `.`, or `""`. Normalizing at *this* layer rather
/// than in the `files` tool is deliberate — every caller shares it, so a second caller
/// cannot get a subtly different answer for the same path.
///
/// The root cases collapse to `""`, which is what "no filter" means to the store.
pub fn normalize_path(path: &str) -> String {
    let unix = path.replace('\\', "/");
    let trimmed = unix.trim();

    if matches!(trimmed, "/" | "." | "") {
        return String::new();
    }

    let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
    stripped.trim_start_matches('/').to_string()
}

/// Split a project name into ranking-exclusion tokens: on `[-_ .]` **and** camelCase
/// boundaries, lowercased, deduped, order-stable.
///
/// `"my-cool_App.v2"` → `["my", "cool", "app", "v2"]`; `"SeleneCode"` → `["selene", "code"]`.
pub fn tokenize_project_name(name: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();

    let push = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty() {
            let lower = current.to_lowercase();
            if !tokens.contains(&lower) {
                tokens.push(lower);
            }
            current.clear();
        }
    };

    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, '-' | '_' | ' ' | '.') {
            push(&mut current, &mut tokens);
            continue;
        }
        // A camelCase boundary: a capital preceded by a lowercase/digit.
        if c.is_uppercase() && i > 0 && chars[i - 1].is_lowercase_or_digit() && !current.is_empty()
        {
            push(&mut current, &mut tokens);
        }
        current.push(*c);
    }
    push(&mut current, &mut tokens);

    tokens
}

/// A tiny extension so the camelCase-boundary test reads as one thought.
trait CharExt {
    fn is_lowercase_or_digit(&self) -> bool;
}
impl CharExt for char {
    fn is_lowercase_or_digit(&self) -> bool {
        self.is_lowercase() || self.is_ascii_digit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #426 — the four spellings the agent uses, and the three that mean "the root".
    #[test]
    fn path_normalization_collapses_the_agents_four_spellings() {
        assert_eq!(normalize_path("./src/a.ts"), "src/a.ts");
        assert_eq!(normalize_path("src/a.ts"), "src/a.ts");
        assert_eq!(
            normalize_path("src\\a.ts"),
            "src/a.ts",
            "windows separators"
        );
        assert_eq!(normalize_path("/src/a.ts"), "src/a.ts", "leading slash");

        for root in ["/", ".", "", "  "] {
            assert_eq!(
                normalize_path(root),
                "",
                "{root:?} means THE ROOT, which is the empty filter"
            );
        }
    }

    #[test]
    fn project_name_tokens_split_on_separators_and_camel_case() {
        assert_eq!(tokenize_project_name("SeleneCode"), vec!["selene", "code"]);
        assert_eq!(
            tokenize_project_name("my-cool_App.v2"),
            vec!["my", "cool", "app", "v2"]
        );
        assert_eq!(tokenize_project_name("selene"), vec!["selene"]);
        assert!(tokenize_project_name("").is_empty());
    }

    /// The token list is an **exclusion** list, so a duplicate would double-penalize.
    #[test]
    fn project_name_tokens_are_deduped_and_order_stable() {
        assert_eq!(
            tokenize_project_name("code-Code_CODE"),
            vec!["code"],
            "the same token three ways is one token"
        );
        assert_eq!(
            tokenize_project_name("b-a-c"),
            vec!["b", "a", "c"],
            "first-seen order, not sorted — the order is observable in ranking"
        );
    }
}
