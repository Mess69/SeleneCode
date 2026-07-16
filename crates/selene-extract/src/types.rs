//! Extraction output shapes + constants (plan §File structure: `types.rs`).

use selene_core::{Edge, Language, Node};
use serde::{Deserialize, Serialize};

use crate::ExtractionError;

/// Files larger than this are skipped with a [`crate::ErrorCode::SizeExceeded`]
/// warning (carried verbatim from the TS `MAX_FILE_SIZE`, Global Constraints).
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// A reference the extractor could see but not resolve to a node id —
/// handed to `selene-resolve` (name matching, imports, frameworks).
/// `reference_kind` is an `EdgeKind` wire string or the special
/// `"function_ref"` marker (FN_REF_SPECS, Task 15a).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedReference {
    /// Node id the reference originates from.
    pub from_node_id: String,
    /// The referenced name as written in source.
    pub reference_name: String,
    /// `EdgeKind` wire string, or `"function_ref"`.
    pub reference_kind: String,
    /// 1-based line of the reference site, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// 0-based column of the reference site, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// File the reference sits in (rewritten by block-delegating extractors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Language of the referencing file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
}

/// Everything one extraction pass produced. Errors are collected here, never
/// thrown (Global Constraints) — a failed file still yields a result with
/// whatever was extracted before the failure.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedReference>,
    pub errors: Vec<ExtractionError>,
    /// Wall time of the pass. The ONLY non-deterministic output field
    /// (Global Constraints allow it alongside `Node.updated_at`).
    pub duration_ms: u64,
}
