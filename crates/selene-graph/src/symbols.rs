//! Symbol resolution — **two lookups that are deliberately not the same**.
//!
//! # The divergence. Port it; do not "fix" it.
//!
//! `find_all_symbols` (the callers/callees/impact path) and `find_symbol_matches` (the
//! node-mode path) answer the same-shaped question and answer it *differently*:
//!
//! | | qualified query with **no exact match** |
//! |---|---|
//! | [`QueryManager::find_all_symbols`] | falls back to the **best fuzzy hit** |
//! | [`QueryManager::find_symbol_matches`] | returns **`[]`** (#173) |
//!
//! That asymmetry is real, it is in the TS build, and #173 fixed **node mode only**. The
//! temptation to unify them is exactly why this comment exists: unifying *changes
//! callers/callees behavior* — a `callers("Nope.gone")` that today answers with the nearest
//! thing would start answering with nothing, and the two tools would silently swap
//! failure modes. `the_two_lookups_diverge_on_a_qualified_miss` pins it in both directions.
//!
//! # ⚠ The store's FTS is AND-semantics (SurrealDB), TS's was OR (SQLite FTS5)
//!
//! A tokenized query (`["Alpha", "gone"]`) returns **nothing** here if *any* term matches
//! nothing, where SQLite would have returned the `Alpha` hits. It matters for exactly one
//! thing: `find_all_symbols`'s fuzzy fallback can only fire when the candidate set is
//! non-empty, so a query with an unknown term now falls back to `[]` rather than to a
//! near-miss. That is *narrower*, never wrong — and the colon-fallback below is what
//! rescues the common qualified-lookup case. Recorded because the difference is invisible
//! until you write a test that depends on it (this one did).
//!
//! # Why node-mode enumerates instead of using FTS
//!
//! A **bare** name in `find_symbol_matches` goes through `get_nodes_by_name` — a full
//! enumeration, deliberately *not* FTS. FTS applies a relevance cut, and a cut drops
//! **overloads**: three `handle` methods in three classes are three answers, and an agent
//! shown one of them is being lied to. Enumeration is O(matches), which is small precisely
//! because the name is exact.

use indexmap::IndexMap;
use selene_core::{Node, NodeKind};
use selene_db::GraphStore;

use crate::error::Result;
use crate::query::QueryManager;

/// Rust path roots that are **not** part of a symbol's name — stripped before matching.
pub const RUST_PATH_PREFIXES: [&str; 3] = ["crate", "super", "self"];

/// The FTS cut for `find_all_symbols`. Ported verbatim.
const FTS_LIMIT: usize = 50;

/// Symbols that share a definition site — the #764 grouping.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolGroup {
    /// The file the definition lives in.
    pub file_path: String,
    /// The qualified name of the definition.
    pub qualified_name: String,
    /// Every node at that site (a class and its methods, an overload set).
    pub nodes: Vec<Node>,
}

/// Does `node` answer `query`?
///
/// Two ways, either sufficient:
/// 1. the `::`-joined qualified name **ends with** the query (`a::b::c` answers `b::c`), or
/// 2. the query names a **path segment** of the node's file.
///
/// `crate::`/`super::`/`self::` are stripped from the query first — they are addressing,
/// not identity.
pub fn matches_symbol(node: &Node, query: &str) -> bool {
    let q = strip_rust_path_prefixes(query);
    if q.is_empty() {
        return false;
    }

    // --- 1. the qualified name -------------------------------------------------
    // Normalize both sides to `::` so `Foo.bar` and `Foo::bar` are the same question.
    let needle = q.replace('.', "::");
    let qualified = node.qualified_name.replace('.', "::");

    if qualified == needle || qualified.ends_with(&format!("::{needle}")) {
        return true;
    }
    if node.name == needle {
        return true;
    }

    // --- 2. the file path ------------------------------------------------------
    // ⚠ Against the RAW query, never the `::`-normalized needle. `auth.ts` normalizes to
    // `auth::ts`, which matches no path on earth — and every file-segment lookup in the
    // product would silently answer nothing. (My own unit test caught this; it is the
    // reason the dot-normalization is scoped to the qualified-name check above.)
    let path = node.file_path.replace('\\', "/");
    path.split('/').any(|seg| seg == q) || path.contains(&format!("/{}", q.replace("::", "/")))
}

/// `crate::db::Store` → `db::Store`.
fn strip_rust_path_prefixes(query: &str) -> String {
    let mut q = query.trim();
    loop {
        let mut stripped = false;
        for prefix in RUST_PATH_PREFIXES {
            for sep in ["::", "."] {
                let with_sep = format!("{prefix}{sep}");
                if let Some(rest) = q.strip_prefix(&with_sep) {
                    q = rest;
                    stripped = true;
                }
            }
        }
        if !stripped {
            break;
        }
    }
    q.to_string()
}

/// A generated file's symbols sort **last**: they are real, but they are never what the
/// agent meant.
fn is_generated(path: &str) -> bool {
    let p = path.replace('\\', "/");
    p.contains("/generated/")
        || p.contains("/gen/")
        || p.contains(".generated.")
        || p.contains(".g.")
        || p.contains("/node_modules/")
        || p.contains("/target/")
        || p.ends_with(".pb.go")
        || p.ends_with("_pb2.py")
}

impl<S: GraphStore> QueryManager<S> {
    /// **The callers/callees/impact lookup.** FTS, then exact-name preference — and a
    /// **fuzzy fallback on a qualified miss** (see the module docs: this is the divergence).
    pub async fn find_all_symbols(&self, name: &str) -> Result<Vec<Node>> {
        let mut results = self.fts(name).await?;

        // The colon fallback: `Foo::bar` found nothing, so try `bar`. A qualified name the
        // index does not carry qualified is still findable by its tail.
        if results.is_empty()
            && name.contains(':')
            && let Some(tail) = name.rsplit([':']).next()
            && !tail.is_empty()
        {
            results = self.fts(tail).await?;
        }
        // (Wave 2, Phase 8: the nix option-path special case
        // `^[a-z][\w'-]*(?:\.[\w'-]+)+$` goes here.)

        let exact: Vec<Node> = results.iter().filter(|n| n.name == name).cloned().collect();

        // ⚠ THE DIVERGENCE. `> 1` returns the overload set; `<= 1` falls through to the
        // best fuzzy hit — EVEN FOR A QUALIFIED MISS. `find_symbol_matches` does not
        // (#173 fixed node mode only). Unifying these changes callers/callees behavior.
        if exact.len() > 1 {
            return Ok(exact);
        }
        if exact.len() == 1 {
            return Ok(exact);
        }
        Ok(results.into_iter().take(1).collect())
    }

    /// **The node-mode lookup.** A bare name enumerates; a qualified name filters, and a
    /// qualified miss is **`[]`** (#173) — never a fuzzy consolation prize.
    pub async fn find_symbol_matches(&self, symbol: &str) -> Result<Vec<Node>> {
        let qualified = symbol.contains('.') || symbol.contains("::");

        let mut nodes = if qualified {
            let candidates = self.fts(symbol).await?;
            let matched: Vec<Node> = candidates
                .into_iter()
                .filter(|n| matches_symbol(n, symbol))
                .collect();

            // #173 — NO FALLBACK. The agent asked for `Foo.handle`; answering with some
            // other `handle` is a wrong answer wearing the right name, and node mode is
            // where the agent reads code.
            if matched.is_empty() {
                return Ok(Vec::new());
            }
            matched
        } else {
            // Full enumeration, NOT FTS: the cut drops overloads (see the module docs).
            self.store().get_nodes_by_name(symbol).await?
        };

        // Generated files last; otherwise stable (deterministic output).
        nodes.sort_by_key(|n| is_generated(&n.file_path));
        Ok(nodes)
    }

    /// Group nodes by their definition site (#764), preserving **first-seen order** —
    /// grouped output ordering is observable, so it is an `IndexMap`, never a `HashMap`.
    ///
    /// ⚠ **On watch as a potential inert seam.** This method only *means* anything if the
    /// callers/callees surfaces (Task 18) render the groups. If Task 18 ships a flat list,
    /// grouping is computed, discarded, and never noticed — the fifth instance of the bug
    /// class this project has already paid for four times. **Verify at Task 18**; if it is
    /// still uncalled at Task 13's ledger pass, it is dead and one of the two must change.
    pub async fn group_by_definition(&self, nodes: Vec<Node>) -> Vec<SymbolGroup> {
        let mut groups: IndexMap<(String, String), SymbolGroup> = IndexMap::new();

        for node in nodes {
            let key = (node.file_path.clone(), node.qualified_name.clone());
            groups
                .entry(key)
                .or_insert_with(|| SymbolGroup {
                    file_path: node.file_path.clone(),
                    qualified_name: node.qualified_name.clone(),
                    nodes: Vec::new(),
                })
                .nodes
                .push(node);
        }

        groups.into_values().collect()
    }

    /// One FTS pass, at the ported limit.
    ///
    /// **The query is TOKENIZED first**, and that is not cosmetic: the store's FTS index is
    /// over symbol *names*, so a qualified query handed over whole (`"Beta.handle"`) matches
    /// nothing at all — there is no node named `Beta.handle`. Splitting on `.` / `::` asks
    /// the question the index can answer ("anything called `Beta`, anything called
    /// `handle`") and lets [`matches_symbol`] do the qualification. Without this, every
    /// qualified lookup in the product returns empty and node mode looks broken.
    ///
    /// The store's raw relevance score is dropped here on purpose — ranking belongs to
    /// `selene-context` (Task 5), and a second, incompatible score leaking upward is how a
    /// ranking pipeline ends up with two truths. Order is preserved.
    async fn fts(&self, query: &str) -> Result<Vec<Node>> {
        let terms: Vec<String> = query
            .split(['.', ':'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        Ok(self
            .store()
            .search_fts(&terms, &[] as &[NodeKind], &[], FTS_LIMIT, 0)
            .await?
            .into_iter()
            .map(|c| c.node)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_core::Language;

    fn node(name: &str, qualified: &str, file: &str) -> Node {
        Node {
            id: format!("n:{qualified}"),
            kind: NodeKind::Method,
            name: name.to_string(),
            qualified_name: qualified.to_string(),
            file_path: file.to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: None,
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: vec![],
            type_parameters: vec![],
            return_type: None,
            route_method: None,
            route_path: None,
            framework: None,
            updated_at: 0,
        }
    }

    #[test]
    fn matches_symbol_takes_a_qualified_suffix_in_either_spelling() {
        let n = node("handle", "Server::handle", "src/server.rs");
        assert!(matches_symbol(&n, "Server::handle"));
        assert!(
            matches_symbol(&n, "Server.handle"),
            "`.` and `::` are one question"
        );
        assert!(matches_symbol(&n, "handle"));
        assert!(!matches_symbol(&n, "Client::handle"));
    }

    #[test]
    fn rust_path_prefixes_are_addressing_not_identity() {
        let n = node("Store", "db::Store", "src/db.rs");
        for q in ["crate::db::Store", "super::db::Store", "self::db::Store"] {
            assert!(matches_symbol(&n, q), "{q} must strip to `db::Store`");
        }
    }

    #[test]
    fn a_file_path_segment_is_a_match() {
        let n = node("login", "login", "src/services/auth.ts");
        assert!(matches_symbol(&n, "auth.ts"), "a path segment answers too");
    }

    #[test]
    fn generated_paths_are_recognized() {
        assert!(is_generated("src/generated/api.ts"));
        assert!(is_generated("proto/user.pb.go"));
        assert!(is_generated("app/models_pb2.py"));
        assert!(!is_generated("src/services/auth.ts"));
    }
}
