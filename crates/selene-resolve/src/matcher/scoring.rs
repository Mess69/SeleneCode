//! The name-matcher's scoring: [`find_best_match`]'s weights,
//! [`prefer_call_site_file`], [`pick_closest_file_node`], the candidate language
//! gate, and the ubiquitous-name ceiling.
//!
//! Every weight here is copied verbatim from `maps/resolution.md`
//! §Confidence/scoring constants. **They are a contract, not a heuristic to
//! tune**: change one and you change which of several same-named symbols a call
//! binds to, silently, across every repo. A resolver that binds a reference to
//! the *wrong* target is worse than one that binds nothing.

use selene_core::{Language, Node, NodeKind, UnresolvedRef};

use crate::families::{crosses_known_family, same_language_family};

/// Above this many candidates, a name is **ubiquitous** and the matcher
/// **declines rather than guesses** (#999).
///
/// Picking one target among K same-named definitions by directory proximity is
/// both unreliable and O(K) per reference — the quadratic behind the "resolving
/// refs" wedge on theme/SDK-vendoring repos. The precise strategies
/// (qualified-name, import, class-name) have already run by this point; fuzzy
/// still follows, and it only ever resolves a *unique* candidate.
pub const DEFAULT_AMBIGUOUS_NAME_CEILING: usize = 500;

/// Env override for [`DEFAULT_AMBIGUOUS_NAME_CEILING`]. A positive integer;
/// anything else falls back to the default (a garbage env var degrades
/// resolution, it never fails a run).
pub const AMBIGUOUS_NAME_CEILING_ENV: &str = "SELENE_AMBIGUOUS_NAME_CEILING";

/// The configured ceiling.
pub fn ambiguous_name_ceiling() -> usize {
    std::env::var(AMBIGUOUS_NAME_CEILING_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_AMBIGUOUS_NAME_CEILING)
}

/// Drop candidates whose language the reference's kind forbids.
///
/// The **candidate-side** twin of the resolver's `gate_language` (which gates a
/// *result*). Same rules: `references`/`function_ref` must stay inside the
/// reference's own family; `imports` must not cross two *known* families;
/// everything else (`calls`, `extends`, …) passes, because cross-language
/// `calls` bridges are real.
pub fn apply_language_gate(candidates: Vec<Node>, r: &UnresolvedRef) -> Vec<Node> {
    let Some(ref_lang) = Language::from_wire(&r.language) else {
        return candidates;
    };

    match r.reference_kind.as_str() {
        "references" | "function_ref" => candidates
            .into_iter()
            .filter(|c| {
                Language::from_wire(&c.language)
                    .is_some_and(|cl| same_language_family(cl, ref_lang))
            })
            .collect(),
        "imports" => candidates
            .into_iter()
            .filter(|c| {
                Language::from_wire(&c.language)
                    .is_none_or(|cl| !crosses_known_family(cl, ref_lang))
            })
            .collect(),
        _ => candidates,
    }
}

/// Directory proximity: **15 points per shared leading segment, capped at 80**.
///
/// `ref_dirs` is the reference file's path minus its filename, pre-split — the
/// caller splits it **once** and scores every candidate against it. Re-splitting
/// per candidate was a measured hot spot (#915).
pub fn path_proximity_from_dirs(ref_dirs: &[&str], other: &str) -> i32 {
    let parts: Vec<&str> = other.split('/').collect();
    let other_dirs = &parts[..parts.len().saturating_sub(1)];

    let shared = ref_dirs
        .iter()
        .zip(other_dirs.iter())
        .take_while(|(a, b)| a == b)
        .count();

    ((shared as i32) * 15).min(80)
}

/// [`path_proximity_from_dirs`] for a single pair of paths.
pub fn path_proximity(from: &str, to: &str) -> i32 {
    let parts: Vec<&str> = from.split('/').collect();
    let dirs = &parts[..parts.len().saturating_sub(1)];
    path_proximity_from_dirs(dirs, to)
}

/// Move the candidates declared in the call site's **own file** to the front
/// (#1079).
///
/// A same-file definition is the strongest language-agnostic signal for which of
/// several same-named symbols a call means. Without it, resolution collapses onto
/// whichever was indexed first, so a call in `b/svc` wrongly targets `a/svc`.
///
/// It is a **stable partition, not a sort**: the same-file candidates keep their
/// relative order, and so do the rest. A no-op with fewer than 2 candidates, or
/// when none share the call site's file.
pub fn prefer_call_site_file(nodes: &[Node], call_site_file: &str) -> Vec<Node> {
    if nodes.len() < 2 {
        return nodes.to_vec();
    }
    let (same, other): (Vec<Node>, Vec<Node>) = nodes
        .iter()
        .cloned()
        .partition(|n| n.file_path == call_site_file);

    if same.is_empty() {
        return nodes.to_vec();
    }
    let mut out = same;
    out.extend(other);
    out
}

/// Among several **file** nodes matching a bare include/import by basename, the
/// one closest to the referencing file.
///
/// Same directory first (as a *pool*, not a bonus), then directory proximity,
/// with **+5 for the same language family** as a tiebreak. A C/C++
/// `#include "X.h"` resolves relative to the including file — never to an
/// arbitrary same-named header on another platform.
pub fn pick_closest_file_node(candidates: &[Node], r: &UnresolvedRef) -> Option<Node> {
    let dir_of = |p: &str| -> String {
        match p.rfind('/') {
            Some(i) => p[..i].to_string(),
            None => String::new(),
        }
    };
    let ref_dir = dir_of(&r.file_path);

    let same_dir: Vec<&Node> = candidates
        .iter()
        .filter(|c| dir_of(&c.file_path) == ref_dir)
        .collect();
    let pool: Vec<&Node> = if same_dir.is_empty() {
        candidates.iter().collect()
    } else {
        same_dir
    };

    let ref_lang = Language::from_wire(&r.language);
    let mut best: Option<&Node> = None;
    let mut best_score = i32::MIN;

    for c in pool {
        let family_bonus = match (Language::from_wire(&c.language), ref_lang) {
            (Some(cl), Some(rl)) if same_language_family(cl, rl) => 5,
            _ => 0,
        };
        let score = path_proximity(&r.file_path, &c.file_path) + family_bonus;
        // Strictly greater: FIRST-WINS on ties (candidate order is insertion order).
        if score > best_score {
            best_score = score;
            best = Some(c);
        }
    }
    best.cloned()
}

/// The best of several same-named candidates — **the scoring table**.
///
/// | signal | delta |
/// |---|---|
/// | same file | **+100** |
/// | directory proximity | **+15/shared leading segment, capped at 80** |
/// | same language | **+50** · different language | **−80** |
/// | `calls` ref → `function`/`method` target | **+25** |
/// | `instantiates` ref → `class`/`struct`/`interface` | **+25** |
/// | `decorates` ref → `function`/`method` | **+25**; → `class`/`interface` | **+15** |
/// | target `is_exported` | **+10** |
/// | same file, line distance | **+ max(0, 20 − distance/10)** |
///
/// **First-wins on ties** (the comparison is strictly `>`), and candidate order
/// is the store's insertion order — so the tie-break is stable, not arbitrary.
///
/// # The cross-language short-circuit is not just an optimization
///
/// When *any* same-language candidate exists, cross-language ones are skipped
/// **entirely**. That is provably the same winner — a same-language candidate
/// scores at least +50, while a cross-language one maxes out at +35 (−80
/// language, +80 proximity, +25 kind, +10 exported; it can never be in the same
/// file) — and it cuts the candidate set to same-language size on mixed
/// front-end/back-end repos (#915). When *all* candidates are cross-language (a
/// legitimate cross-language `calls` bridge), nothing is skipped.
pub fn find_best_match(candidates: &[Node], r: &UnresolvedRef) -> Option<Node> {
    let ref_parts: Vec<&str> = r.file_path.split('/').collect();
    let ref_dirs = &ref_parts[..ref_parts.len().saturating_sub(1)];

    let has_same_language = candidates.iter().any(|c| c.language == r.language);

    let mut best: Option<&Node> = None;
    let mut best_score = i32::MIN;

    for c in candidates {
        if has_same_language && c.language != r.language {
            continue;
        }

        let mut score = 0i32;

        if c.file_path == r.file_path {
            score += 100;
        }

        score += path_proximity_from_dirs(ref_dirs, &c.file_path);

        if c.language == r.language {
            score += 50;
        } else {
            score -= 80;
        }

        match r.reference_kind.as_str() {
            "calls" if matches!(c.kind, NodeKind::Function | NodeKind::Method) => score += 25,
            "instantiates"
                if matches!(
                    c.kind,
                    NodeKind::Class | NodeKind::Struct | NodeKind::Interface
                ) =>
            {
                score += 25
            }
            "decorates" => {
                if matches!(c.kind, NodeKind::Function | NodeKind::Method) {
                    score += 25;
                } else if matches!(c.kind, NodeKind::Class | NodeKind::Interface) {
                    // A class decorator (Python `@SomeClass`, a Java annotation
                    // interface) is real, but a function is the likelier target.
                    score += 15;
                }
            }
            _ => {}
        }

        if c.is_exported == Some(true) {
            score += 10;
        }

        // A closer definition in the SAME file wins over a distant one.
        if c.file_path == r.file_path
            && c.start_line > 0
            && let Some(ref_line) = r.line
        {
            let distance = (c.start_line as i32 - ref_line as i32).abs();
            score += (20 - distance / 10).max(0);
        }

        if score > best_score {
            best_score = score;
            best = Some(c);
        }
    }

    best.cloned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use selene_core::RefStatus;

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, lang: Language) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            qualified_name: name.into(),
            file_path: file.into(),
            language: lang.as_str().into(),
            start_line: 10,
            end_line: 20,
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
            updated_at: 0,
        }
    }

    fn re(name: &str, kind: &str, file: &str, lang: Language) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: "function:caller".into(),
            reference_name: name.into(),
            reference_kind: kind.into(),
            line: Some(12),
            column: Some(0),
            candidates: vec![],
            file_path: file.into(),
            language: lang.as_str().into(),
            status: RefStatus::Pending,
            name_tail: name.into(),
        }
    }

    #[test]
    fn path_proximity_is_15_per_shared_segment_capped_at_80() {
        assert_eq!(
            path_proximity("a/b/x.ts", "a/b/y.ts"),
            30,
            "2 shared segments"
        );
        assert_eq!(
            path_proximity("a/b/x.ts", "a/c/y.ts"),
            15,
            "1 shared segment"
        );
        assert_eq!(path_proximity("a/x.ts", "z/y.ts"), 0, "nothing shared");
        assert_eq!(
            path_proximity("a/b/c/d/e/f/x.ts", "a/b/c/d/e/f/y.ts"),
            80,
            "6 shared segments would be 90 — the cap is 80"
        );
        // The filename itself is never a "segment".
        assert_eq!(path_proximity("x.ts", "y.ts"), 0);
    }

    #[test]
    fn same_file_beats_everything_else() {
        let same = node(
            "a",
            NodeKind::Function,
            "f",
            "src/a.ts",
            Language::Typescript,
        );
        let near = node(
            "b",
            NodeKind::Function,
            "f",
            "src/b.ts",
            Language::Typescript,
        );
        let r = re("f", "calls", "src/a.ts", Language::Typescript);

        let best = find_best_match(&[near.clone(), same.clone()], &r).unwrap();
        assert_eq!(best.id, "a", "+100 for the same file dwarfs proximity");
    }

    #[test]
    fn a_same_language_candidate_always_beats_a_cross_language_one() {
        // The cross-language candidate is in the SAME DIRECTORY; the
        // same-language one is far away. Language still wins.
        let cross = node("go", NodeKind::Function, "f", "src/x/a.go", Language::Go);
        let same = node(
            "ts",
            NodeKind::Function,
            "f",
            "far/away/b.ts",
            Language::Typescript,
        );
        let r = re("f", "calls", "src/x/caller.ts", Language::Typescript);

        let best = find_best_match(&[cross, same], &r).unwrap();
        assert_eq!(
            best.id, "ts",
            "when any same-language candidate exists, cross-language ones are \
             skipped entirely — provably the same winner"
        );
    }

    #[test]
    fn an_all_cross_language_set_still_resolves() {
        // A legitimate cross-language `calls` bridge: nothing is skipped.
        let go = node("go", NodeKind::Function, "f", "src/a.go", Language::Go);
        let r = re("f", "calls", "src/caller.ts", Language::Typescript);
        assert_eq!(find_best_match(&[go], &r).unwrap().id, "go");
    }

    #[test]
    fn kind_bias_favors_the_right_target_per_reference_kind() {
        let func = node(
            "fn",
            NodeKind::Function,
            "Foo",
            "src/a.ts",
            Language::Typescript,
        );
        let class = node(
            "cls",
            NodeKind::Class,
            "Foo",
            "src/b.ts",
            Language::Typescript,
        );

        // `new Foo()` must prefer the CLASS — without the bias a same-named
        // function in another module outscores it.
        let inst = re("Foo", "instantiates", "src/caller.ts", Language::Typescript);
        assert_eq!(
            find_best_match(&[func.clone(), class.clone()], &inst)
                .unwrap()
                .id,
            "cls"
        );

        // A call prefers the function.
        let call = re("Foo", "calls", "src/caller.ts", Language::Typescript);
        assert_eq!(
            find_best_match(&[class.clone(), func.clone()], &call)
                .unwrap()
                .id,
            "fn"
        );

        // A decorator prefers a function (+25) over a class (+15) — but a class
        // decorator still resolves.
        let dec = re("Foo", "decorates", "src/caller.ts", Language::Typescript);
        assert_eq!(
            find_best_match(&[class.clone(), func.clone()], &dec)
                .unwrap()
                .id,
            "fn"
        );
        assert_eq!(find_best_match(&[class], &dec).unwrap().id, "cls");
    }

    #[test]
    fn ties_keep_the_first_candidate() {
        // Two identical candidates in the same file: the scores tie exactly.
        let a = node(
            "first",
            NodeKind::Function,
            "f",
            "src/a.ts",
            Language::Typescript,
        );
        let b = node(
            "second",
            NodeKind::Function,
            "f",
            "src/a.ts",
            Language::Typescript,
        );
        let r = re("f", "calls", "src/a.ts", Language::Typescript);
        assert_eq!(
            find_best_match(&[a, b], &r).unwrap().id,
            "first",
            "first-wins: the comparison is strictly `>` and candidate order is \
             the store's insertion order"
        );
    }

    #[test]
    fn prefer_call_site_file_is_a_stable_partition() {
        let a = node(
            "a",
            NodeKind::Function,
            "f",
            "other.ts",
            Language::Typescript,
        );
        let b = node(
            "b",
            NodeKind::Function,
            "f",
            "here.ts",
            Language::Typescript,
        );
        let c = node(
            "c",
            NodeKind::Function,
            "f",
            "other2.ts",
            Language::Typescript,
        );

        let out = prefer_call_site_file(&[a.clone(), b.clone(), c.clone()], "here.ts");
        assert_eq!(
            out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a", "c"],
            "the same-file candidate moves to the front; the others keep their order"
        );

        // No same-file candidate ⇒ untouched.
        let out = prefer_call_site_file(&[a.clone(), c.clone()], "here.ts");
        assert_eq!(
            out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"]
        );

        // Fewer than 2 ⇒ untouched.
        assert_eq!(prefer_call_site_file(&[a], "here.ts").len(), 1);
    }

    /// A `#include "X.h"` must resolve to the header next door, not to an
    /// arbitrary same-named header on another platform.
    #[test]
    fn pick_closest_file_node_prefers_the_same_directory_pool() {
        let apple = node(
            "apple",
            NodeKind::File,
            "X.h",
            "apple/code/X.h",
            Language::C,
        );
        let windows = node(
            "windows",
            NodeKind::File,
            "X.h",
            "windows/code/X.h",
            Language::C,
        );
        let r = re("X.h", "imports", "apple/code/main.c", Language::C);

        let best = pick_closest_file_node(&[windows, apple], &r).unwrap();
        assert_eq!(best.id, "apple");
    }

    #[test]
    fn the_language_gate_filters_candidates_by_reference_kind() {
        let go = node("go", NodeKind::Function, "f", "a.go", Language::Go);
        let ts = node("ts", NodeKind::Function, "f", "a.ts", Language::Typescript);

        // `references` must stay inside the family.
        let refs = re("f", "references", "caller.ts", Language::Typescript);
        let kept = apply_language_gate(vec![go.clone(), ts.clone()], &refs);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "ts");

        // `calls` is ungated — cross-language bridges are real.
        let calls = re("f", "calls", "caller.ts", Language::Typescript);
        assert_eq!(apply_language_gate(vec![go, ts], &calls).len(), 2);
    }

    #[test]
    fn the_ceiling_env_override_parses_or_falls_back() {
        // SAFETY: this is the only test in the crate touching this variable, and
        // it restores it before returning.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(AMBIGUOUS_NAME_CEILING_ENV);
            assert_eq!(ambiguous_name_ceiling(), DEFAULT_AMBIGUOUS_NAME_CEILING);
            std::env::set_var(AMBIGUOUS_NAME_CEILING_ENV, "50");
            assert_eq!(ambiguous_name_ceiling(), 50);
            std::env::set_var(AMBIGUOUS_NAME_CEILING_ENV, "0");
            assert_eq!(ambiguous_name_ceiling(), DEFAULT_AMBIGUOUS_NAME_CEILING);
            std::env::set_var(AMBIGUOUS_NAME_CEILING_ENV, "nonsense");
            assert_eq!(ambiguous_name_ceiling(), DEFAULT_AMBIGUOUS_NAME_CEILING);
            std::env::remove_var(AMBIGUOUS_NAME_CEILING_ENV);
        }
    }
}
