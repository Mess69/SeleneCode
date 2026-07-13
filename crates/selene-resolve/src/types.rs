//! The resolver's own record types: [`ResolvedRef`], [`ResolvedBy`],
//! [`ResolutionResult`] + [`ResolutionStats`], and the import-side data the
//! [`crate::ResolutionContext`] hands to the strategies ([`ImportMapping`],
//! [`ReExport`], [`AliasMap`], [`GoModule`], [`WorkspacePackages`]).
//!
//! `UnresolvedRef` is **re-used verbatim** from `selene_core` — it is the row
//! `selene-extract` writes and `selene-db` stores, and redefining it here would
//! create a second truth about the one record whose identity the whole batch
//! loop keys on.
//!
//! The four *loader-backed* structs at the bottom ([`AliasMap`],
//! [`AliasPattern`], [`GoModule`], [`WorkspacePackages`]) are declared here,
//! in Task 2, because the [`crate::ResolutionContext`] trait returns them and
//! the trait must be complete for Parts B and C to compile against. Their
//! **loaders** (`load_project_aliases`, `load_go_module`,
//! `load_workspace_packages`) arrive in Task 4, in `src/imports/`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use selene_core::UnresolvedRef;

// =============================================================================
// ResolvedBy / ResolvedRef
// =============================================================================

/// Which strategy bound a reference. **The wire strings are a contract**: they
/// are persisted in `Edge.metadata.resolvedBy`, read by explore/node output,
/// by edge resurrection (`#1240`), and by the Part C parity gate — which diffs
/// them, because an edge that binds the right target *by the wrong strategy* is
/// a pipeline-order regression that is invisible today and mis-binds tomorrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResolvedBy {
    /// `exact-match` — [`crate::matcher`]'s name match (Task 7).
    ExactMatch,
    /// `import` — bound through an import mapping (Tasks 5/6).
    Import,
    /// `qualified-name` — matched on `qualified_name` (Task 7).
    QualifiedName,
    /// `framework` — claimed by a framework resolver (Part B).
    Framework,
    /// `fuzzy` — lowercase fallback, unique-or-drop (Task 7).
    Fuzzy,
    /// `instance-method` — receiver-type inference, validated on the type (Task 8).
    InstanceMethod,
    /// `file-path` — a path-shaped reference bound to a file node (Task 7).
    FilePath,
    /// `function-ref` — a function used as a value (Task 10).
    FunctionRef,
}

impl ResolvedBy {
    /// The wire string persisted in `Edge.metadata.resolvedBy`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolvedBy::ExactMatch => "exact-match",
            ResolvedBy::Import => "import",
            ResolvedBy::QualifiedName => "qualified-name",
            ResolvedBy::Framework => "framework",
            ResolvedBy::Fuzzy => "fuzzy",
            ResolvedBy::InstanceMethod => "instance-method",
            ResolvedBy::FilePath => "file-path",
            ResolvedBy::FunctionRef => "function-ref",
        }
    }
}

/// A reference, bound.
///
/// # `original` is mandatory, and it is not bookkeeping
///
/// `original` is **the stored row, unmutated**. `GraphStore::delete_resolved`
/// keys the row deletion on `(from_node_id, reference_name)`, so a strategy
/// that returns a *synthetic* or *rewritten* reference here no-ops the delete:
/// the row stays pending, the offset-0 batch loop re-reads it forever, and the
/// run explodes (CodeGraph `#760`: 5M edges / 1.4 GB before the non-progress
/// guard caught it). Two places in this crate are specifically at risk and must
/// return `original: ref.clone()` — the Go bare-name chain fallback (Task 9)
/// and `match_function_ref` (Task 10) — and every framework `resolve()` in
/// Part B constructs it too.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRef {
    /// The stored row this binding came from — **never** a mutated copy.
    pub original: UnresolvedRef,
    /// The id of the node the reference binds to.
    pub target_node_id: String,
    /// `0.0..=1.0`. **0.9 is the return-immediately threshold** in
    /// `resolve_one`; the final pick is max-confidence, first-wins on ties.
    /// `f64` throughout — the constants (0.95 / 0.92 / 0.9 / 0.85 / …) are
    /// load-bearing and must not be rounded by a narrower type.
    pub confidence: f64,
    /// The strategy that bound it.
    pub resolved_by: ResolvedBy,
}

// =============================================================================
// Results
// =============================================================================

/// Per-pass counters. `by_method` is keyed by [`ResolvedBy::as_str`] (plus the
/// batched pass's `callback-synthesis` key, added in Part C) — a **`BTreeMap`,
/// not a `HashMap`**, because it is rendered into output and diffed by the
/// parity gate, and `HashMap` iteration order would make both nondeterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionStats {
    /// References processed.
    pub total: usize,
    /// References bound to a target.
    pub resolved: usize,
    /// References that found no target.
    pub unresolved: usize,
    /// Count per strategy.
    pub by_method: BTreeMap<String, usize>,
}

/// The outcome of a resolution pass.
#[derive(Debug, Clone, Default)]
pub struct ResolutionResult {
    /// Bound references.
    pub resolved: Vec<ResolvedRef>,
    /// References that found no target (kept for the failed-ref retry pipeline).
    pub unresolved: Vec<UnresolvedRef>,
    /// Counters.
    pub stats: ResolutionStats,
}

impl ResolutionResult {
    /// Record a binding (and its `by_method` tally).
    pub fn push_resolved(&mut self, r: ResolvedRef) {
        *self.by_method_entry(r.resolved_by.as_str()).or_insert(0) += 1;
        self.stats.total += 1;
        self.stats.resolved += 1;
        self.resolved.push(r);
    }

    /// Record a miss.
    pub fn push_unresolved(&mut self, r: UnresolvedRef) {
        self.stats.total += 1;
        self.stats.unresolved += 1;
        self.unresolved.push(r);
    }

    fn by_method_entry(
        &mut self,
        key: &str,
    ) -> std::collections::btree_map::Entry<'_, String, usize> {
        self.stats.by_method.entry(key.to_string())
    }
}

// =============================================================================
// Import-side data (loaders arrive in Task 4)
// =============================================================================

/// One binding introduced by an import statement.
///
/// `local_name` is the name as used in the importing file; `exported_name` is
/// the name in the exporting module (they differ under `import { a as b }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMapping {
    /// The name bound in the importing file (`b` in `import { a as b }`).
    pub local_name: String,
    /// The name exported by the source module (`a` in `import { a as b }`).
    pub exported_name: String,
    /// The module specifier, verbatim (`./utils`, `@/lib/x`, `com.foo.Bar`).
    pub source: String,
    /// `import x from 'm'`.
    pub is_default: bool,
    /// `import * as x from 'm'` (and Go/C-include whole-module bindings).
    pub is_namespace: bool,
    /// The repo-relative file `source` resolved to, once known (Task 5).
    pub resolved_path: Option<String>,
}

/// A re-export in a barrel file — the hop `find_exported_symbol` chases
/// (`REEXPORT_MAX_DEPTH = 8`, Task 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReExport {
    /// `export { a as b } from './x'` — **the rename is followed**.
    Named {
        /// The name this barrel exports (`b`).
        exported_name: String,
        /// The name in the source module (`a`).
        original_name: String,
        /// The source module specifier.
        source: String,
    },
    /// `export * from './x'` — chased last, after every named re-export.
    Wildcard {
        /// The source module specifier.
        source: String,
    },
}

/// One `compilerOptions.paths` entry, pre-split around its single `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasPattern {
    /// The text before the `*` (or the whole key, when there is no wildcard).
    pub prefix: String,
    /// The text after the `*`.
    pub suffix: String,
    /// Whether the key carried a `*` at all (literal keys sort first).
    pub has_wildcard: bool,
    /// The replacement templates, in declaration order.
    pub replacements: Vec<String>,
}

/// A loaded `tsconfig.json`/`jsconfig.json` alias table (Task 4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AliasMap {
    /// `compilerOptions.baseUrl`, resolved to an **absolute** path.
    pub base_url: PathBuf,
    /// Patterns, sorted longer-prefix-first, literal-before-wildcard.
    pub patterns: Vec<AliasPattern>,
}

/// A `go.mod` `module` directive (Task 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoModule {
    /// The module path (`github.com/foo/bar`).
    pub module_path: String,
    /// The directory holding the `go.mod`, repo-relative.
    pub root_dir: String,
}

/// npm/yarn/bun/pnpm workspace members (Task 4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspacePackages {
    /// Package name → its directory, repo-relative.
    pub by_name: std::collections::HashMap<String, String>,
    /// Package name → its declared entry file, when it declares one.
    pub entry_by_name: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_by_wire_strings_are_the_contract() {
        // maps/resolution.md §Wire/contract surfaces — these strings are
        // persisted in edge metadata and diffed by the Part C parity gate.
        assert_eq!(ResolvedBy::ExactMatch.as_str(), "exact-match");
        assert_eq!(ResolvedBy::Import.as_str(), "import");
        assert_eq!(ResolvedBy::QualifiedName.as_str(), "qualified-name");
        assert_eq!(ResolvedBy::Framework.as_str(), "framework");
        assert_eq!(ResolvedBy::Fuzzy.as_str(), "fuzzy");
        assert_eq!(ResolvedBy::InstanceMethod.as_str(), "instance-method");
        assert_eq!(ResolvedBy::FilePath.as_str(), "file-path");
        assert_eq!(ResolvedBy::FunctionRef.as_str(), "function-ref");
    }

    #[test]
    fn stats_tally_by_method_deterministically() {
        use selene_core::RefStatus;
        let row = |name: &str| UnresolvedRef {
            from_node_id: "function:a".into(),
            reference_name: name.into(),
            reference_kind: "calls".into(),
            line: None,
            column: None,
            candidates: vec![],
            file_path: "src/a.ts".into(),
            language: "typescript".into(),
            status: RefStatus::Pending,
            name_tail: name.into(),
        };
        let mut out = ResolutionResult::default();
        out.push_resolved(ResolvedRef {
            original: row("a"),
            target_node_id: "function:t".into(),
            confidence: 0.9,
            resolved_by: ResolvedBy::Import,
        });
        out.push_resolved(ResolvedRef {
            original: row("b"),
            target_node_id: "function:t".into(),
            confidence: 0.7,
            resolved_by: ResolvedBy::ExactMatch,
        });
        out.push_resolved(ResolvedRef {
            original: row("c"),
            target_node_id: "function:t".into(),
            confidence: 0.9,
            resolved_by: ResolvedBy::Import,
        });
        out.push_unresolved(row("d"));

        assert_eq!(out.stats.total, 4);
        assert_eq!(out.stats.resolved, 3);
        assert_eq!(out.stats.unresolved, 1);
        // BTreeMap ⇒ a stable, sorted rendering, every run.
        let rendered: Vec<(&str, usize)> = out
            .stats
            .by_method
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        assert_eq!(rendered, vec![("exact-match", 1), ("import", 2)]);
    }
}
