//! # selene-core
//!
//! Shared domain types for SeleneCode's code-intelligence graph: the node and
//! edge kinds, provenance, the [`Node`] / [`Edge`] records that every other
//! crate reads and writes, and the extraction row records ([`FileRecord`],
//! [`UnresolvedRef`], [`RefStatus`]) that `selene-extract` produces and
//! `selene-db` stores.
//!
//! Ported faithfully from the CodeGraph TypeScript implementation
//! (`src/types.ts`). These wire strings are the contract shared by extractors,
//! resolvers, and the graph store — they must not drift.
//!
//! See `docs/specs/2026-07-11-rust-graph-db-migration-design.md` §4 (data model).

use serde::{Deserialize, Serialize};
use std::str::FromStr;

mod ids;
pub use ids::{EXTRACTION_VERSION, file_node_id, hash_content, node_id};

// =============================================================================
// NodeKind (22)
// =============================================================================

/// Kind of a node in the knowledge graph.
///
/// Serializes to the exact snake_case strings used by the graph store and the
/// search query parser (e.g. `EnumMember` <-> `"enum_member"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Module,
    Class,
    Struct,
    Interface,
    Trait,
    Protocol,
    Function,
    Method,
    Property,
    Field,
    Variable,
    Constant,
    Enum,
    EnumMember,
    TypeAlias,
    Namespace,
    Parameter,
    Import,
    Export,
    Route,
    Component,
}

impl NodeKind {
    /// Every node kind, in declaration order.
    pub const ALL: [NodeKind; 22] = [
        NodeKind::File,
        NodeKind::Module,
        NodeKind::Class,
        NodeKind::Struct,
        NodeKind::Interface,
        NodeKind::Trait,
        NodeKind::Protocol,
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Property,
        NodeKind::Field,
        NodeKind::Variable,
        NodeKind::Constant,
        NodeKind::Enum,
        NodeKind::EnumMember,
        NodeKind::TypeAlias,
        NodeKind::Namespace,
        NodeKind::Parameter,
        NodeKind::Import,
        NodeKind::Export,
        NodeKind::Route,
        NodeKind::Component,
    ];

    /// The canonical wire string (identical to the serde representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::Class => "class",
            NodeKind::Struct => "struct",
            NodeKind::Interface => "interface",
            NodeKind::Trait => "trait",
            NodeKind::Protocol => "protocol",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Property => "property",
            NodeKind::Field => "field",
            NodeKind::Variable => "variable",
            NodeKind::Constant => "constant",
            NodeKind::Enum => "enum",
            NodeKind::EnumMember => "enum_member",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Namespace => "namespace",
            NodeKind::Parameter => "parameter",
            NodeKind::Import => "import",
            NodeKind::Export => "export",
            NodeKind::Route => "route",
            NodeKind::Component => "component",
        }
    }
}

impl FromStr for NodeKind {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        NodeKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| Error::UnknownNodeKind(s.to_owned()))
    }
}

// =============================================================================
// EdgeKind (12)
// =============================================================================

/// Kind of an edge (relationship) between two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Parent contains child (file→class, class→method).
    Contains,
    /// Function/method calls another.
    Calls,
    /// File imports from another.
    Imports,
    /// File exports a symbol.
    Exports,
    /// Class/interface extends another.
    Extends,
    /// Class implements interface.
    Implements,
    /// Generic reference to another symbol.
    References,
    /// Variable/parameter has type.
    TypeOf,
    /// Function returns type.
    Returns,
    /// Creates instance of class.
    Instantiates,
    /// Method overrides parent method.
    Overrides,
    /// Decorator applied to symbol.
    Decorates,
}

impl EdgeKind {
    /// Every edge kind, in declaration order.
    pub const ALL: [EdgeKind; 12] = [
        EdgeKind::Contains,
        EdgeKind::Calls,
        EdgeKind::Imports,
        EdgeKind::Exports,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::References,
        EdgeKind::TypeOf,
        EdgeKind::Returns,
        EdgeKind::Instantiates,
        EdgeKind::Overrides,
        EdgeKind::Decorates,
    ];

    /// The canonical wire string (identical to the serde representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Calls => "calls",
            EdgeKind::Imports => "imports",
            EdgeKind::Exports => "exports",
            EdgeKind::Extends => "extends",
            EdgeKind::Implements => "implements",
            EdgeKind::References => "references",
            EdgeKind::TypeOf => "type_of",
            EdgeKind::Returns => "returns",
            EdgeKind::Instantiates => "instantiates",
            EdgeKind::Overrides => "overrides",
            EdgeKind::Decorates => "decorates",
        }
    }
}

impl FromStr for EdgeKind {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        EdgeKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| Error::UnknownEdgeKind(s.to_owned()))
    }
}

// =============================================================================
// Provenance & Visibility
// =============================================================================

/// How an edge (or node) was created.
///
/// `Heuristic` marks synthesized edges (dynamic-dispatch bridges: callback,
/// EventEmitter, React re-render, JSX child, ORM descriptor). Keeping these
/// tagged is load-bearing — the graph surfaces them inline so an agent can see
/// a flow was bridged rather than statically proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    /// Extracted directly from the tree-sitter AST.
    TreeSitter,
    /// Derived from a SCIP index.
    Scip,
    /// Synthesized by a resolver/synthesizer (dynamic dispatch).
    Heuristic,
}

/// Visibility modifier on a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
}

// =============================================================================
// Node & Edge records
// =============================================================================

/// A node in the knowledge graph representing a code symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// Unique identifier: `"<kind>:" + hex(sha256("{filePath}:{kind}:{name}:{startLine}"))[..32]`
    /// (see [`node_id`]) — the hash input is path/kind/name/**start line**,
    /// NOT the qualified name. File nodes are the unhashed literal
    /// `file:<path>` ([`file_node_id`]).
    pub id: String,
    /// Type of code element.
    pub kind: NodeKind,
    /// Simple name (e.g. `calculateTotal`).
    pub name: String,
    /// Fully qualified name (e.g. `MathHelper::calculateTotal`) — scope
    /// segments joined by `::`, **never** a file-path component (the
    /// extraction contract, `extraction-core.md` §9: paths in qualified
    /// names pollute FTS).
    pub qualified_name: String,
    /// File path relative to project root.
    pub file_path: String,
    /// Programming language (an extensible registry owned by `selene-extract`).
    pub language: String,
    /// Starting line (1-indexed).
    pub start_line: u32,
    /// Ending line (1-indexed).
    pub end_line: u32,
    /// Starting column (0-indexed).
    pub start_column: u32,
    /// Ending column (0-indexed).
    pub end_column: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_exported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_async: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_static: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_abstract: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_parameters: Vec<String>,
    /// Normalized return/result type name for a function/method (bare class
    /// name, smart-pointer pointee unwrapped). Enables chained-receiver type
    /// inference; `None` where a language/symbol does not capture it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,

    /// When the node was last updated (unix millis).
    pub updated_at: i64,
}

/// An edge representing a relationship between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    /// Type of relationship.
    pub kind: EdgeKind,
    /// Extra context. For synthesized (`Heuristic`) edges, carries
    /// `synthesizedBy` / `registeredAt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Line where the relationship occurs (e.g. call site).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column where the relationship occurs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// How this edge was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

// =============================================================================
// Extraction row records (produced by selene-extract, stored by selene-db)
// =============================================================================
//
// These live here — not in the storage crate — because `selene-extract`
// (Phase 2) *produces* them: an extraction result is `(Vec<Node>, Vec<Edge>,
// Vec<UnresolvedRef>, FileRecord)`, and the extractor must not depend on the
// storage crate to name its own output. `selene-db` re-exports all three at
// its crate root, so store-side code and tests keep their existing paths.

/// A tracked source file: the unit of incremental re-indexing.
///
/// `path` is the store's primary key. `content_hash` lets a caller skip
/// re-extracting a file whose content hasn't changed. `node_count` is a
/// denormalized count of the nodes currently attributed to this file.
/// `errors` carries structured extraction diagnostics (parse errors, etc.) —
/// an opaque JSON array, not interpreted by the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecord {
    /// Path relative to the project root; primary key.
    pub path: String,
    /// Content hash used to detect an unchanged file and skip re-extraction.
    pub content_hash: String,
    /// Programming language, as classified by `selene-extract`.
    pub language: String,
    /// File size in bytes.
    pub size: u64,
    /// Filesystem modification time (unix millis).
    pub modified_at: i64,
    /// When this file was last (re-)indexed (unix millis).
    pub indexed_at: i64,
    /// Number of nodes currently attributed to this file.
    pub node_count: u32,
    /// Structured extraction diagnostics for this file (opaque to the store).
    pub errors: Vec<serde_json::Value>,
}

/// The resolution status of an [`UnresolvedRef`].
///
/// Serializes to the wire strings `"pending"` / `"failed"` (ported verbatim
/// from the CodeGraph TS schema's `unresolved_refs.status` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefStatus {
    /// Not yet attempted, or eligible for another resolution pass.
    Pending,
    /// Resolution was attempted and did not succeed; kept for the store's
    /// bounded retry pipeline (`retryable_failed`) rather than dropped.
    Failed,
}

impl RefStatus {
    /// The canonical wire string — identical to the serde representation and
    /// to what the store persists in the `status` column. Queries bind this
    /// instead of restringing `'pending'`/`'failed'` literals (the same
    /// discipline as [`NodeKind::as_str`]/[`EdgeKind::as_str`]).
    pub fn as_str(&self) -> &'static str {
        match self {
            RefStatus::Pending => "pending",
            RefStatus::Failed => "failed",
        }
    }
}

/// A reference (call, import, type use, …) whose target symbol could not be
/// resolved at extraction time, held for a later cross-file resolution pass.
///
/// `(from_node_id, reference_name)` is the natural key the store's
/// `delete_resolved`/`mark_failed` operations match on.
/// `file_path`/`language` are denormalized from the source node so the
/// resolver can batch by file without a join. `name_tail` is the last
/// dot/`::`-separated segment of `reference_name`, used to index failed
/// retries by their most specific segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedRef {
    /// Id of the node that holds this unresolved reference.
    pub from_node_id: String,
    /// The name being referenced (possibly qualified).
    pub reference_name: String,
    /// What kind of reference this is (e.g. `"call"`, `"type"`,
    /// `"function_ref"` — a superset of [`EdgeKind`]; never itself an edge
    /// kind once resolved).
    pub reference_kind: String,
    /// Line of the reference site, if known.
    pub line: Option<u32>,
    /// Column of the reference site, if known.
    pub column: Option<u32>,
    /// Candidate target nodes gathered so far (opaque JSON, scored upstream).
    pub candidates: Vec<serde_json::Value>,
    /// File path of the referencing node (denormalized).
    pub file_path: String,
    /// Language of the referencing node (denormalized).
    pub language: String,
    /// Current resolution status.
    pub status: RefStatus,
    /// Last segment of `reference_name` (e.g. `"calculateTotal"` out of
    /// `"MathHelper.calculateTotal"`).
    pub name_tail: String,
}

// =============================================================================
// Errors
// =============================================================================

/// Errors surfaced by the graph.
///
/// **Invariant (PRD §8.2):** only [`Error::PathRefusal`] is a genuine "stop
/// trying" error that a tool surface should mark `isError: true`. Every other
/// variant is a recoverable/expected condition that MUST be returned as
/// success-shaped guidance — one or two `isError` responses and an agent
/// abandons the tool entirely.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Security refusal — the ONLY variant that should surface as `isError`.
    #[error("path refused: {0}")]
    PathRefusal(String),
    /// Project has no index yet — recoverable; return guidance, never `isError`.
    #[error("project not indexed: {0}")]
    NotIndexed(String),
    /// Symbol not found — recoverable.
    #[error("symbol not found: {0}")]
    SymbolNotFound(String),
    /// File not in the index — recoverable.
    #[error("file not in index: {0}")]
    FileNotIndexed(String),
    /// Unknown node-kind wire string.
    #[error("unknown node kind: {0}")]
    UnknownNodeKind(String),
    /// Unknown edge-kind wire string.
    #[error("unknown edge kind: {0}")]
    UnknownEdgeKind(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, Error>;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn kind_counts_match_the_data_model() {
        assert_eq!(NodeKind::ALL.len(), 22);
        assert_eq!(EdgeKind::ALL.len(), 12);
    }

    #[test]
    fn node_kind_roundtrips_through_its_wire_string() {
        for k in NodeKind::ALL {
            assert_eq!(NodeKind::from_str(k.as_str()).unwrap(), k);
        }
    }

    #[test]
    fn edge_kind_roundtrips_through_its_wire_string() {
        for k in EdgeKind::ALL {
            assert_eq!(EdgeKind::from_str(k.as_str()).unwrap(), k);
        }
    }

    #[test]
    fn as_str_matches_serde_representation() {
        assert_eq!(NodeKind::EnumMember.as_str(), "enum_member");
        assert_eq!(NodeKind::TypeAlias.as_str(), "type_alias");
        assert_eq!(EdgeKind::TypeOf.as_str(), "type_of");
        // serde and as_str must agree
        let json = serde_json::to_string(&NodeKind::EnumMember).unwrap();
        assert_eq!(json, "\"enum_member\"");
        let prov = serde_json::to_string(&Provenance::TreeSitter).unwrap();
        assert_eq!(prov, "\"tree-sitter\"");
    }

    #[test]
    fn unknown_kind_is_an_error() {
        assert!(NodeKind::from_str("nope").is_err());
        assert!(EdgeKind::from_str("nope").is_err());
    }

    /// `RefStatus::as_str` must stay identical to the serde wire strings the
    /// store persists in the `status` column — store queries bind `as_str()`,
    /// so a drift between the two would silently match nothing.
    #[test]
    fn ref_status_as_str_matches_serde_representation() {
        for status in [RefStatus::Pending, RefStatus::Failed] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
        }
    }
}
