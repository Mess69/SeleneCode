//! Language-family gates — `maps/resolution.md` §`resolveOne` pipeline
//! ("Language gates") and §`matchReference`.
//!
//! The family table itself is [`selene_core::Language::family`] (decision D1).
//! This module is the **gate logic** built on it, and the distinction between
//! the two predicates below is load-bearing:
//!
//! - [`same_language_family`] — "could these two be the same program?" Used to
//!   gate `references` and `function_ref` results: a Python reference must not
//!   bind to a same-named Go symbol.
//! - [`crosses_known_family`] — "do these two sit in *different, known*
//!   families?" Used to gate `imports` results (and, in Part B,
//!   framework results for `references`/`imports`).
//!
//! They are **not** each other's negation, and assuming they are is a real bug.
//! For two singleton languages (python, ruby) `same_language_family` is `false`
//! **and** `crosses_known_family` is also `false` — an unfamilied language
//! neither matches another language nor crosses a known boundary. That is
//! exactly why an `imports` reference in Python survives the import gate while a
//! Python `references` reference cannot bind into Go.

use selene_core::Language;

/// Are `a` and `b` the same language, or two members of one family?
///
/// (`typescript`/`tsx` yes; `java`/`kotlin` yes; `python`/`ruby` no.)
pub fn same_language_family(a: Language, b: Language) -> bool {
    if a == b {
        return true;
    }
    match (a.family(), b.family()) {
        (Some(fa), Some(fb)) => fa == fb,
        _ => false,
    }
}

/// Does `l` belong to one of the five known families at all?
pub fn is_known_language_family(l: Language) -> bool {
    l.family().is_some()
}

/// Do `a` and `b` sit in **different, known** families?
///
/// A language with no family entry never "crosses" — see the module docs. This
/// is the predicate the `imports` gate uses, and the asymmetry with
/// [`same_language_family`] is deliberate.
pub fn crosses_known_family(a: Language, b: Language) -> bool {
    match (a.family(), b.family()) {
        (Some(fa), Some(fb)) => fa != fb,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_core::Language::*;

    #[test]
    fn same_family_within_a_family() {
        assert!(same_language_family(Java, Kotlin));
        assert!(same_language_family(Kotlin, Scala));
        assert!(same_language_family(Typescript, Tsx));
        assert!(same_language_family(Javascript, Jsx));
        assert!(same_language_family(Typescript, Javascript));
        assert!(same_language_family(C, Cpp));
        assert!(same_language_family(CSharp, Razor));
        assert!(same_language_family(Swift, Objc));
    }

    #[test]
    fn identical_singletons_are_the_same_family() {
        // `a == b` short-circuits BEFORE the family lookup — without that, two
        // Python refs would fail their own gate.
        assert!(same_language_family(Python, Python));
        assert!(same_language_family(Ruby, Ruby));
        assert!(same_language_family(Go, Go));
    }

    #[test]
    fn different_families_are_not_the_same_family() {
        assert!(!same_language_family(Java, Typescript));
        assert!(!same_language_family(C, CSharp));
        assert!(!same_language_family(Python, Go));
    }

    #[test]
    fn crossing_requires_two_known_families() {
        assert!(crosses_known_family(Java, Typescript)); // jvm vs web
        assert!(crosses_known_family(C, CSharp)); // c vs dotnet
        assert!(crosses_known_family(Swift, Cpp)); // apple vs c
        assert!(!crosses_known_family(Java, Kotlin)); // one family
        assert!(!crosses_known_family(Typescript, Tsx)); // one family
    }

    /// THE truth-table case the module docs call out: two singleton languages
    /// are neither "the same family" NOR "crossing a known family". Both
    /// predicates return false, and that is why `imports` refs in unfamilied
    /// languages survive their gate.
    #[test]
    fn singletons_neither_match_nor_cross() {
        assert!(!same_language_family(Python, Ruby));
        assert!(!crosses_known_family(Python, Ruby));

        // Half-known is still not a crossing: an unfamilied language never crosses.
        assert!(!crosses_known_family(Python, Java));
        assert!(!crosses_known_family(Java, Python));
        assert!(!same_language_family(Python, Java));
    }

    #[test]
    fn known_family_membership() {
        assert!(is_known_language_family(Java));
        assert!(is_known_language_family(Cpp));
        assert!(!is_known_language_family(Python));
        assert!(!is_known_language_family(Rust));
    }
}
