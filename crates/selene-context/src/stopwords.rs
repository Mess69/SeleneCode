//! Query-symbol extraction, and the stopword list that makes it useful.
//!
//! # Why a stopword list at all
//!
//! An agent asks "how does the request layer handle incoming data". Every one of those words
//! is *also* a symbol name somewhere in a large repo — `Request`, `Layer`, `handle`, `Data`
//! — and searching for them returns thousands of unrelated hits that crowd out the two
//! symbols the agent actually needs. The list is **verbatim from the TS**
//! (`../codegraph/src/context/index.ts:105-130`), including the second half, which the TS
//! comment explains was added for exactly that reason: *"common English nouns/verbs that
//! match thousands of unrelated code symbols"*.
//!
//! Do not "improve" it. Every word in it was earned by a bad answer.
//!
//! # A stopword-only query yields ZERO terms
//!
//! …and that is **success-shaped**, not an error: the caller renders guidance ("try naming a
//! symbol"). It is the single most common way a first query fails, and answering it with an
//! `isError` is how the tool gets abandoned.

use std::sync::LazyLock;

use indexmap::IndexSet;
use regex::Regex;

/// The stopword list — **verbatim** from the TS. See the module docs before touching it.
pub const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "from",
    "this",
    "that",
    "have",
    "been",
    "will",
    "would",
    "could",
    "should",
    "does",
    "done",
    "make",
    "made",
    "use",
    "used",
    "using",
    "work",
    "works",
    "find",
    "found",
    "show",
    "call",
    "called",
    "calling",
    "get",
    "set",
    "add",
    "all",
    "any",
    "how",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "not",
    "but",
    "are",
    "was",
    "were",
    "has",
    "had",
    "its",
    "can",
    "did",
    "may",
    "also",
    "into",
    "than",
    "then",
    "them",
    "each",
    "other",
    "some",
    "such",
    "only",
    "same",
    "about",
    "after",
    "before",
    "between",
    "through",
    "during",
    "without",
    "again",
    "further",
    "once",
    "here",
    "there",
    "both",
    "just",
    "more",
    "most",
    "very",
    "being",
    "having",
    "doing",
    "system",
    "need",
    "needs",
    "want",
    "wants",
    "like",
    "look",
    "change",
    "changes",
    "changed",
    "changing",
    // "Common English nouns/verbs that match thousands of unrelated code symbols" (TS).
    "layer",
    "handle",
    "handles",
    "handling",
    "incoming",
    "outgoing",
    "data",
    "flow",
    "flows",
    "level",
    "levels",
    "request",
    "requests",
    "response",
    "responses",
    "implement",
    "implements",
    "implementation",
    "interface",
    "interfaces",
    "class",
    "classes",
    "method",
    "methods",
    "trigger",
    "triggers",
    "affected",
    "affect",
    "affects",
    "else",
    "code",
    "failing",
    "failed",
    "silently",
    "decide",
    "decides",
    "return",
    "returns",
    "returned",
    "take",
    "takes",
    "taken",
    "check",
    "checks",
    "checked",
    "create",
    "creates",
    "created",
    "read",
    "reads",
    "write",
    "writes",
    "written",
    "start",
    "starts",
    "stop",
    "stops",
    "run",
    "runs",
    "running",
    // ── Beyond the TS list. Each was earned by a bad answer, which is the only way a word
    //    gets in here. ──────────────────────────────────────────────────────────────────
    //
    // "become": from *"how does an unresolved reference **become** a graph edge"*. It is a pure
    // English copula — it names no code — but it survived extraction as a search term, was
    // handed its own concept group, and that group then competed for a root slot against
    // `edge`, the actual subject of the question. A word that can never match a symbol must
    // never cost a root.
    "become",
    "becomes",
];

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, exercised by the tests below
            Regex::new($pat).unwrap()
        })
    };
}

/// `handleLogin`, `HttpServer` — camelCase / PascalCase, 2+ chars.
static CAMEL: LazyLock<Regex> = re!(r"\b([A-Z][a-z]+(?:[A-Z][a-z]*)*|[a-z]+(?:[A-Z][a-z]*)+)\b");
/// `hash_password` — snake_case, 3+ chars.
static SNAKE: LazyLock<Regex> = re!(r"(?i)\b([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\b");
/// `MAX_RETRIES` — SCREAMING_SNAKE.
static SCREAMING: LazyLock<Regex> = re!(r"\b([A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)\b");
/// `REST`, `HTTP`, `LRU` — an all-caps acronym, 2+ chars.
static ACRONYM: LazyLock<Regex> = re!(r"\b([A-Z]{2,})\b");
/// `app.isPackaged` — dotted; **both** the whole path and each part are terms.
static DOTTED: LazyLock<Regex> = re!(r"\b([a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)+)\b");
/// `undo`, `render`, `parse` — a plain lowercase identifier, 3+ chars.
static LOWERCASE: LazyLock<Regex> = re!(r"\b([a-z][a-z0-9]{2,})\b");

/// The six patterns, minus the stopwords — **insertion-ordered** (the order reaches ranking,
/// so it is an `IndexSet`, never a `HashSet`).
pub fn extract_symbols_from_query(query: &str) -> Vec<String> {
    let mut symbols: IndexSet<String> = IndexSet::new();

    for caps in CAMEL.captures_iter(query) {
        if let Some(m) = caps.get(1)
            && m.as_str().len() >= 2
        {
            symbols.insert(m.as_str().to_string());
        }
    }
    for caps in SNAKE.captures_iter(query) {
        if let Some(m) = caps.get(1)
            && m.as_str().len() >= 3
        {
            symbols.insert(m.as_str().to_string());
        }
    }
    for caps in SCREAMING.captures_iter(query) {
        if let Some(m) = caps.get(1) {
            symbols.insert(m.as_str().to_string());
        }
    }
    for caps in ACRONYM.captures_iter(query) {
        if let Some(m) = caps.get(1) {
            symbols.insert(m.as_str().to_string());
        }
    }
    for caps in DOTTED.captures_iter(query) {
        if let Some(m) = caps.get(1) {
            // BOTH the whole dotted path and its parts: `app.isPackaged` is a symbol, and so
            // are `app` and `isPackaged`.
            symbols.insert(m.as_str().to_string());
            for part in m.as_str().split('.') {
                if part.len() >= 2 {
                    symbols.insert(part.to_string());
                }
            }
        }
    }
    for caps in LOWERCASE.captures_iter(query) {
        if let Some(m) = caps.get(1) {
            symbols.insert(m.as_str().to_string());
        }
    }

    symbols
        .into_iter()
        .filter(|s| !STOPWORDS.contains(&s.to_lowercase().as_str()))
        .collect()
}

/// The terms the FTS passes search with — the same extraction, and the same stopword filter.
pub fn extract_search_terms(query: &str) -> Vec<String> {
    extract_symbols_from_query(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_patterns_are_extracted() {
        let terms = extract_symbols_from_query(
            "handleLogin hash_password MAX_RETRIES REST app.isPackaged render",
        );
        for want in [
            "handleLogin",
            "hash_password",
            "MAX_RETRIES",
            "REST",
            "app.isPackaged",
            "isPackaged", // a dotted path contributes its PARTS too
            "render",
        ] {
            assert!(
                terms.contains(&want.to_string()),
                "missing {want}: {terms:?}"
            );
        }
    }

    /// The list is the difference between an answer and a thousand unrelated hits.
    #[test]
    fn stopwords_are_dropped_including_the_code_shaped_ones() {
        let terms = extract_symbols_from_query(
            "how does the request layer handle incoming data and return the response",
        );
        assert!(
            terms.is_empty(),
            "every word here is ALSO a symbol name in a large repo — that is exactly why the \
             second half of the list exists: {terms:?}"
        );
    }

    /// A stopword-only query yields zero terms, and that must stay **success-shaped**
    /// upstream.
    #[test]
    fn a_stopword_only_query_yields_zero_terms_not_an_error() {
        assert!(extract_symbols_from_query("how does this work").is_empty());
        assert!(extract_symbols_from_query("").is_empty());
    }

    #[test]
    fn extraction_order_is_stable() {
        let a = extract_symbols_from_query("parseConfig loadConfig saveConfig");
        let b = extract_symbols_from_query("parseConfig loadConfig saveConfig");
        assert_eq!(a, b, "IndexSet, not HashSet — the order reaches ranking");
        assert_eq!(a, vec!["parseConfig", "loadConfig", "saveConfig"]);
    }
}
