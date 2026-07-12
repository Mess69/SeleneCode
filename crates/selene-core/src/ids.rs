//! Contract constants + id/hash helpers shared by extraction, storage, sync
//! and MCP status: the node-id formula, the file-node-id exception, the file
//! content hash, and [`EXTRACTION_VERSION`].
//!
//! These are **byte-for-byte wire contracts** (roadmap "never drift" list;
//! `extraction-core.md` §9): node ids are persisted in `.selene/`, embedded in
//! edges, and key-matched by prefix downstream (`file:`, `class:`, …). The
//! golden tests below pin the exact output bytes — the TS suite never did,
//! which this port fixes (`extraction-core.md` §Test coverage; the #899
//! edge-reattachment protocol depends on stable ids).

use sha2::{Digest, Sha256};

use crate::NodeKind;

/// Extraction engine output-shape version for the **Rust** engine.
///
/// Starts at **1**: the TS lineage counter (24) versioned the `.codegraph/`
/// store, which no Rust binary ever reads — `.selene/` is a disjoint store,
/// so the version space restarts (maintainer decision, 2026-07-12).
///
/// **Bump rule:** ANY change to extraction output shape — node/edge/ref
/// emission, id inputs, docstring cleanup, qualified-name spelling — bumps
/// this. A stored version older than the engine's yields "re-index
/// recommended" guidance, **never** a hard error.
pub const EXTRACTION_VERSION: u32 = 1;

/// The id of a code-symbol node:
/// `"<kind>:" + hex(sha256("{file_path}:{kind}:{name}:{line}"))[..32]`,
/// where `<kind>` is the [`NodeKind::as_str`] wire string and `line` is the
/// 1-based start line.
///
/// The `kind:` prefix is load-bearing — downstream code key-matches on id
/// prefixes. The hash input embeds the **start line**, not the qualified
/// name (the TS-era doc comment claiming "file path + qualified name" was
/// stale — see `extraction-core.md` §9): identical input bytes must yield
/// identical ids, and a symbol moving to another line is a different id.
///
/// File nodes are the exception — see [`file_node_id`].
pub fn node_id(file_path: &str, kind: NodeKind, name: &str, start_line: u32) -> String {
    let kind = kind.as_str();
    let digest = Sha256::digest(format!("{file_path}:{kind}:{name}:{start_line}"));
    let hex = format!("{digest:x}");
    format!("{kind}:{}", &hex[..32])
}

/// The id of a file node: the **unhashed** literal `file:<path>`
/// (`extraction-core.md` §9 exception to [`node_id`]'s hashed form).
pub fn file_node_id(file_path: &str) -> String {
    format!("file:{file_path}")
}

/// sha256 hex (lowercase, 64 chars) of the full file text. Backs the
/// sync-phase change detection (`FileRecord.content_hash`).
pub fn hash_content(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Golden BYTE pin, computed out-of-band:
    /// `printf '%s' "src/utils.ts:function:calc:10" | shasum -a 256`
    /// = 13e5299c6652e2e79cd4f674ca9b075f6b58da9aef1d4bb30961a46264e54aaa
    /// → first 32 hex chars, prefixed `function:`.
    #[test]
    fn node_id_golden_bytes() {
        assert_eq!(
            node_id("src/utils.ts", NodeKind::Function, "calc", 10),
            "function:13e5299c6652e2e79cd4f674ca9b075f"
        );
    }

    #[test]
    fn node_id_shape_and_determinism() {
        let id = node_id("a.rs", NodeKind::Method, "m", 1);
        assert!(id.starts_with("method:"), "kind prefix is load-bearing");
        assert_eq!(id.len(), "method:".len() + 32, "32 hex chars after prefix");
        assert_eq!(id, node_id("a.rs", NodeKind::Method, "m", 1));
        // Any input component changing changes the id.
        assert_ne!(id, node_id("b.rs", NodeKind::Method, "m", 1));
        assert_ne!(id, node_id("a.rs", NodeKind::Function, "m", 1));
        assert_ne!(id, node_id("a.rs", NodeKind::Method, "n", 1));
        assert_ne!(id, node_id("a.rs", NodeKind::Method, "m", 2));
    }

    /// The file-node exception: UNHASHED literal, no truncation, no digest.
    #[test]
    fn file_node_id_is_unhashed_literal() {
        assert_eq!(file_node_id("a/b.ts"), "file:a/b.ts");
        assert_eq!(file_node_id(""), "file:");
    }

    /// Golden vectors, computed out-of-band with `shasum -a 256`:
    /// - `printf 'hello world\n'` (real trailing newline) →
    ///   a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447
    /// - "" → e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    ///   (the well-known SHA-256 empty-input digest).
    #[test]
    fn hash_content_golden_bytes() {
        assert_eq!(
            hash_content("hello world\n"),
            "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"
        );
        assert_eq!(
            hash_content(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn extraction_version_is_one() {
        assert_eq!(EXTRACTION_VERSION, 1, "Rust engine restarts the counter");
    }
}
