//! Extraction error records: collected, never thrown (Global Constraints —
//! every extractor returns partial results plus `ExtractionError`s; the TS
//! catch-all semantics, `extraction-langs.md` §Error semantics).

use serde::{Deserialize, Serialize};

/// How bad an [`ExtractionError`] is. `Warning` = the file was (partially)
/// skipped by design (e.g. unsupported language); `Error` = something
/// genuinely failed mid-extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// Machine-readable error class. Serializes to the snake_case wire strings
/// the TS store recorded in `files.errors` (`extraction-langs.md` §Wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Detected language has no registered grammar/extractor (wave-2
    /// languages in v0) — a **warning** + skip, matching the TS
    /// missing-grammar semantics.
    UnsupportedLanguage,
    /// Building/loading a parser failed (a malfunction, not a bad file).
    ParserError,
    /// The source failed to parse.
    ParseError,
    /// The file could not be read.
    ReadError,
    /// The path escapes the project root (security refusal).
    PathTraversal,
    /// The file exceeds [`crate::MAX_FILE_SIZE`].
    SizeExceeded,
}

/// One collected extraction problem. Extraction never throws: files produce
/// partial results plus zero or more of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionError {
    /// Human-readable description.
    pub message: String,
    /// Error vs warning (see [`Severity`]).
    pub severity: Severity,
    /// Machine-readable class (see [`ErrorCode`]).
    pub code: ErrorCode,
    /// The file the problem belongs to, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}
