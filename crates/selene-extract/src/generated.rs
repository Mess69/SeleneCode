//! Generated-file detection for symbol-disambiguation down-ranking — a pure
//! path-suffix classifier, ported verbatim from the TS
//! `generated-detection.ts` (the suffix list is a contract: dropping
//! `.pb.go` regresses the cosmos-sdk trace endpoint to the gRPC stub).
//!
//! NOT a hard filter: generated nodes stay in the graph and remain
//! reachable; downstream ranking just puts them last when a hand-written
//! implementation shares the name. Path-only — never reads content.

use std::sync::LazyLock;

use regex::Regex;

/// The suffix contract, verbatim from TS (`GENERATED_PATTERNS`). Note
/// `^mock_[^/]+\.go$` is (deliberately) anchored to the WHOLE path — it only
/// fires for a repo-root `mock_*.go`, exactly as in TS where the regex ran
/// against the full relative path.
static GENERATED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Go — protobuf / gRPC / pulsar
        r"\.pb\.go$",
        r"\.pulsar\.go$",
        r"_grpc\.pb\.go$",
        // Go — mockgen output: default `mock_<src>.go`; renamed `*_mock.go`
        // / `*_mocks.go` conventions (cosmos-sdk uses `expected_*_mocks.go`).
        r"_mock\.go$",
        r"_mocks\.go$",
        r"^mock_[^/]+\.go$",
        // TypeScript / JavaScript — Apollo/GraphQL codegen, Prisma, Hasura,
        // ts-proto, gRPC-web, swagger-codegen.
        r"\.generated\.[jt]sx?$",
        r"\.gen\.[jt]sx?$",
        r"\.pb\.[jt]s$",
        r"_pb\.[jt]s$",
        r"_grpc_pb\.[jt]s$",
        // Minified bundles vendored into a repo.
        r"\.min\.m?js$",
        // Python — protobuf / gRPC / openapi-codegen
        r"_pb2(_grpc)?\.py$",
        r"_pb2\.pyi$",
        // C++ — protobuf
        r"\.pb\.(cc|h)$",
        // C# — protobuf / gRPC
        r"\.g\.cs$",
        r"Grpc\.cs$",
        // Java — protobuf / gRPC
        r"OuterClass\.java$",
        r"Grpc\.java$",
        // Swift — protobuf
        r"\.pb\.swift$",
        // Dart — build_runner / freezed / json_serializable / chopper
        r"\.g\.dart$",
        r"\.freezed\.dart$",
        r"\.pb\.dart$",
        r"\.pbgrpc\.dart$",
        r"\.chopper\.dart$",
        // Rust — in-tree generated files often use `*.generated.rs`.
        r"\.generated\.rs$",
    ]
    .iter()
    .map(|p| {
        #[allow(clippy::unwrap_used)] // literal patterns, compile-time known good
        Regex::new(p).unwrap()
    })
    .collect()
});

/// Whether `path` looks like a tool-generated source file based on its
/// filename. The result is a relevance hint for disambiguation, not a hard
/// claim.
pub fn is_generated_file(path: &str) -> bool {
    GENERATED_PATTERNS.iter().any(|p| p.is_match(path))
}
