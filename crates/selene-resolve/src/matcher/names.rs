//! The four name strategies: [`match_by_file_path`], [`match_by_qualified_name`],
//! [`match_by_exact_name`], [`match_fuzzy`].
//!
//! Confidences are copied verbatim from `maps/resolution.md`
//! §Confidence/scoring constants. They are the numbers `resolve_one` compares
//! against 0.9 to decide whether to stop looking — so rounding one changes which
//! strategy wins, repo-wide.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, Node, NodeKind, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::matcher::scoring::{
    ambiguous_name_ceiling, apply_language_gate, find_best_match, path_proximity,
    pick_closest_file_node, prefer_call_site_file,
};
use crate::types::{ResolvedBy, ResolvedRef};

/// A bare filename ending in a short extension (`Foo.h`, `x.liquid`).
static FILE_EXTENSION: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, covered by the tests below
    Regex::new(r"\.[A-Za-z][A-Za-z0-9]{0,3}$").unwrap()
});

fn hit(r: &UnresolvedRef, target: &str, confidence: f64, by: ResolvedBy) -> ResolvedRef {
    ResolvedRef {
        // ⚠ The STORED ROW, unmutated — the keyed delete matches on it (#760).
        original: r.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: by,
    }
}

/// Strategy 0 — a **path-like** reference → a file node.
///
/// | case | confidence |
/// |---|---|
/// | exact `qualified_name` or `file_path` match | **0.95** |
/// | suffix match, disambiguated by `pick_closest_file_node` | **0.85** |
/// | a single file node with that basename | **0.70** |
///
/// Only runs when the name contains a `/` **or** ends in a short extension. A
/// bare name *without* an extension is a symbol, not a file, and belongs to the
/// symbol strategies — running this on it would bind `handler` to some
/// `handler.ts` file node.
pub fn match_by_file_path<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    let name = r.reference_name.as_str();
    if !name.contains('/') && !FILE_EXTENSION.is_match(name) {
        return None;
    }

    let file_name = name.rsplit('/').next()?;
    if file_name.is_empty() {
        return None;
    }

    let file_nodes: Vec<Node> = ctx
        .nodes_by_name(file_name)
        .into_iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    if file_nodes.is_empty() {
        return None;
    }

    // An exact path is unambiguous.
    if let Some(exact) = file_nodes
        .iter()
        .find(|n| n.qualified_name == name || n.file_path == name)
    {
        return Some(hit(r, &exact.id, 0.95, ResolvedBy::FilePath));
    }

    // A suffix match (`snippets/foo.liquid` → `src/snippets/foo.liquid`). When
    // several files share the basename — a `#include "X.h"` with a same-named
    // header on another platform — the one in the includer's own directory wins.
    let suffix: Vec<Node> = file_nodes
        .iter()
        .filter(|n| n.qualified_name.ends_with(name) || n.file_path.ends_with(name))
        .cloned()
        .collect();
    if !suffix.is_empty()
        && let Some(best) = pick_closest_file_node(&suffix, r)
    {
        return Some(hit(r, &best.id, 0.85, ResolvedBy::FilePath));
    }

    // A lone same-basename file, at lower confidence.
    if file_nodes.len() == 1 {
        return Some(hit(r, &file_nodes[0].id, 0.7, ResolvedBy::FilePath));
    }

    None
}

/// Strategy 1 — a **qualified** reference (`Foo::bar`, `Foo.bar`).
///
/// | case | confidence |
/// |---|---|
/// | a single exact `qualified_name` | **0.95** |
/// | several exact, one in the call site's own file | **0.95** |
/// | a suffix partial match (then `prefer_call_site_file`) | **0.85** |
///
/// # The yaml/properties exclusion (#1180)
///
/// A method call `service.process()` shares an exact qualified name with the
/// config key `service.process`. Config keys are bound to their code references
/// upstream by the framework resolvers (`@Value` → `references`); a **`calls`**
/// reference must never resolve to a yaml/properties constant — that is a wrong
/// edge *and* it hides the real callee. They are dropped from both candidate sets
/// so resolution falls through to method resolution.
pub fn match_by_qualified_name<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let name = r.reference_name.as_str();
    if !name.contains("::") && !name.contains('.') {
        return None;
    }

    let keep = |nodes: Vec<Node>| -> Vec<Node> {
        if r.reference_kind != "calls" {
            return nodes;
        }
        nodes
            .into_iter()
            .filter(|n| {
                !(n.kind == NodeKind::Constant
                    && (n.language == Language::Yaml.as_str()
                        || n.language == Language::Properties.as_str()))
            })
            .collect()
    };

    let candidates = keep(ctx.nodes_by_qualified_name(name));

    if candidates.len() == 1 {
        return Some(hit(r, &candidates[0].id, 0.95, ResolvedBy::QualifiedName));
    }

    // Several symbols share this exact qualified name (`Logger::log` in two files
    // — an ODR clash, or two translation units). Prefer the call site's own file;
    // otherwise the first-indexed definition wins and a call in `b/svc` targets
    // `a/svc` (#1079).
    if candidates.len() > 1 {
        let ordered = prefer_call_site_file(&candidates, &r.file_path);
        if ordered.first().is_some_and(|n| n.file_path == r.file_path) {
            return Some(hit(r, &ordered[0].id, 0.95, ResolvedBy::QualifiedName));
        }
    }

    // A partial (suffix) qualified match — again preferring the call site's file.
    let last = name.split([':', '.']).next_back()?;
    if last.is_empty() {
        return None;
    }
    let partial: Vec<Node> = keep(ctx.nodes_by_name(last))
        .into_iter()
        .filter(|n| n.qualified_name.ends_with(name))
        .collect();
    let chosen = prefer_call_site_file(&partial, &r.file_path);
    let chosen = chosen.first()?;
    Some(hit(r, &chosen.id, 0.85, ResolvedBy::QualifiedName))
}

/// Strategy 3 — an **exact name** match.
///
/// | case | confidence |
/// |---|---|
/// | a single candidate | **0.9** (cross-language: **0.5**) |
/// | more than `AMBIGUOUS_NAME_CEILING` candidates | **declines** (#999) |
/// | else `find_best_match`, proximity ≥ 30 | **0.7** |
/// | else | **0.4** |
///
/// # `import`-kind nodes are excluded (#915)
///
/// An `import` node is an import *statement*, not a definition, so a reference
/// resolving to a sibling file's import is a meaningless edge — import →
/// definition is `resolve_via_import`'s job, never name-matching's. Excluding
/// them also removes a quadratic: a ubiquitous package (`react`, Python
/// `logging`) is re-declared as an `import` node in *every* file that imports it,
/// so K unresolved refs each scored K same-named import candidates — O(K²) per
/// package, and the dominant cost of "resolving refs" on large repos.
///
/// # The cross-language single-candidate branch stays
///
/// It is mostly unreachable for `references` (the gate already filtered), but it
/// is **live for `calls`**, which is ungated — a legitimate cross-language bridge.
pub fn match_by_exact_name<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let candidates: Vec<Node> = apply_language_gate(ctx.nodes_by_name(&r.reference_name), r)
        .into_iter()
        .filter(|n| n.kind != NodeKind::Import)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    if candidates.len() == 1 {
        let cross_language = candidates[0].language != r.language;
        let confidence = if cross_language { 0.5 } else { 0.9 };
        return Some(hit(
            r,
            &candidates[0].id,
            confidence,
            ResolvedBy::ExactMatch,
        ));
    }

    // The ubiquitous-name ceiling (#999): DECLINE rather than guess.
    //
    // ⚠ The comparison is against the **gated, import-filtered candidate count**,
    // not the store's raw `count_nodes_named`. That is deliberate and it is what
    // the TS source does (`name-matcher.ts:382`). The raw count includes the
    // `import`-kind nodes this strategy just excluded — and a package like
    // `react` is re-declared as an import node in hundreds of files. Gating on the
    // raw count would decline names that are ubiquitous only *as imports* and have
    // exactly one real definition, silently deleting those edges. `count_nodes_named`
    // is the honest node-count primitive and Part B/C may use it; it is NOT this
    // gate.
    if candidates.len() > ambiguous_name_ceiling() {
        return None;
    }

    let best = find_best_match(&candidates, r)?;
    // A match from a distant/unrelated module gets a lower confidence.
    let proximity = path_proximity(&r.file_path, &best.file_path);
    let confidence = if proximity >= 30 { 0.7 } else { 0.4 };
    Some(hit(r, &best.id, confidence, ResolvedBy::ExactMatch))
}

/// Strategy 4 — the **fuzzy** last resort: a case-insensitive name lookup.
///
/// Kinds are restricted to `{function, method, class}`, the language gate
/// applies, and same-language candidates are preferred. **Unique or nothing**:
/// `0.5` (cross-language `0.3`), and more than one candidate resolves to *no
/// edge at all*. A fuzzy match that guesses between two candidates is exactly the
/// wrong-edge failure this whole crate is shaped to avoid.
pub fn match_fuzzy<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    let lower = r.reference_name.to_lowercase();

    let callable: Vec<Node> = apply_language_gate(
        ctx.nodes_by_lower_name(&lower)
            .into_iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Class
                )
            })
            .collect(),
        r,
    );

    let same_language: Vec<Node> = callable
        .iter()
        .filter(|n| n.language == r.language)
        .cloned()
        .collect();
    let final_candidates = if same_language.is_empty() {
        callable
    } else {
        same_language
    };

    if final_candidates.len() != 1 {
        return None;
    }
    let only = &final_candidates[0];
    let confidence = if only.language != r.language {
        0.3
    } else {
        0.5
    };
    Some(hit(r, &only.id, confidence, ResolvedBy::Fuzzy))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The extension guard is deliberately **short** (1–4 chars). It exists to
    /// let a bare `Foo.h` / `a.ts` reach the file matcher, and to keep
    /// `service.process` (a qualified method call) OUT of it — binding a call to
    /// a file node would be a wrong edge that hides the real callee.
    ///
    /// A long extension like `.liquid` therefore does NOT qualify on its own: a
    /// `snippets/drawer-menu.liquid` reference reaches the file matcher through
    /// its **slash**, not its extension. That is the TS behavior exactly, and it
    /// is the conservative direction — the guard admits fewer things, and the
    /// symbol strategies (which cannot invent a file edge) own the rest.
    #[test]
    fn the_file_extension_guard_admits_short_extensions_only() {
        assert!(FILE_EXTENSION.is_match("Foo.h"));
        assert!(FILE_EXTENSION.is_match("a.ts"));
        assert!(FILE_EXTENSION.is_match("x.yaml"), "4 chars is the limit");

        assert!(
            !FILE_EXTENSION.is_match("x.liquid"),
            "6 chars — a BARE `x.liquid` is not file-matched; a path-shaped \
             `snippets/x.liquid` still is, via its slash"
        );
        assert!(!FILE_EXTENSION.is_match("handler"));
        assert!(
            !FILE_EXTENSION.is_match("service.process"),
            "a qualified method call is not a file — treating it as one would \
             bind the call to a file node and hide the real callee"
        );
        assert!(!FILE_EXTENSION.is_match("obj.method"));
    }
}
