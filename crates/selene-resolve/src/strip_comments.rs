//! Comment/string blanking for the framework extractors (Task 11).
//!
//! # Why this exists
//!
//! Every framework extractor in Part B is **regex-over-source**, not AST-based
//! (the parity source is: `frameworks-synth.md` §Key algorithms — "All
//! extractors are regex-over-comment-stripped source"). Run naively, those
//! regexes happily match a route inside a comment:
//!
//! ```text
//! // app.get('/legacy', legacyHandler);   ← a commented-out route
//! ```
//!
//! …and the graph grows a route that does not exist. The TS suite pins this
//! ("extractors ignore commented-out routes"); [`strip_comments_for_regex`] is
//! what makes it true for the whole of Part B.
//!
//! # The contract: byte-length preserving
//!
//! The output has **exactly the same byte length** as the input, and every
//! `\n` sits at exactly the same byte offset. Comment *bodies* are blanked to
//! ASCII spaces; the comment delimiters themselves are kept.
//!
//! This is not an aesthetic choice. Extractors compute a route's **line** from
//! its match offset, and the line feeds the node id
//! (`node_id(file, kind, name, start_line)`). If blanking changed offsets, every
//! route id would shift. A multi-byte UTF-8 char inside a comment becomes *N*
//! space bytes, one per byte — never one space per char (that would shorten the
//! text and silently move every id after it).
//!
//! # Strings are SKIPPED, not blanked — a deliberate deviation from the map
//!
//! `frameworks-synth.md` describes the TS helper as blanking "comments and
//! string bodies". Taken literally that is unimplementable: **every route path
//! lives inside a string literal**. Blanking string bodies would turn
//! `router.get('/users', h)` into `router.get('      ', h)`, and every extractor
//! in Tasks 12–20 — all of which capture their path with `['"]([^'"]+)['"]` over
//! this function's output — would read an empty path. The TS build's route
//! extractors demonstrably do capture real paths from the stripped source, so
//! its string handling must be string-*awareness*, not blanking.
//!
//! So: this scanner **tracks** strings (a `//` or `#` inside `"http://x"` does
//! not start a comment) and leaves their contents **intact**. The property that
//! matters — "extractors ignore commented-out routes" — is fully preserved.
//!
//! The cost is a route-shaped *string literal* (`const doc = "app.get('/x', h)"`)
//! still matching. That is a rare, low-harm false positive; blanking strings to
//! avoid it would break every real route, which is not a trade.

use selene_core::Language;

/// The lexical syntax needed to find comments and strings in one language.
///
/// Deliberately small: this is not a lexer, it is a blanker. It needs to know
/// where a comment starts and ends, and where a string starts and ends, and
/// nothing else.
struct Syntax {
    /// Line-comment openers (`//`, `#`, `--`). Run to end of line.
    line: &'static [&'static str],
    /// Block-comment `(open, close)` pairs. Not nested (no language here
    /// nests them in a way that matters for route-finding).
    block: &'static [(&'static str, &'static str)],
    /// Multi-char string delimiters that must be tried **before** the
    /// single-char ones (Python's `"""`/`'''`): open and close are the same.
    long_strings: &'static [&'static str],
    /// Single-char string delimiters (`"`, `'`, `` ` ``).
    strings: &'static [char],
    /// Whether `\` escapes the next byte inside a string. (Ruby/Python/C-family:
    /// yes. Ignoring it would end a string early on `"a\"b"` and blank the code
    /// that follows.)
    escapes: bool,
}

const C_FAMILY: Syntax = Syntax {
    line: &["//"],
    block: &[("/*", "*/")],
    long_strings: &[],
    strings: &['"', '\''],
    escapes: true,
};

/// JS/TS add the template literal. (Template *interpolations* `${…}` are
/// blanked along with the rest of the body — an extractor cannot trust a
/// computed path anyway: "silent beats wrong".)
const JS_FAMILY: Syntax = Syntax {
    line: &["//"],
    block: &[("/*", "*/")],
    long_strings: &[],
    strings: &['"', '\'', '`'],
    escapes: true,
};

const PYTHON: Syntax = Syntax {
    line: &["#"],
    block: &[],
    // Tried first: `"""` must win over `"` or a docstring blanks as three
    // empty strings and its body leaks back in as live code.
    long_strings: &["\"\"\"", "'''"],
    strings: &['"', '\''],
    escapes: true,
};

const RUBY: Syntax = Syntax {
    line: &["#"],
    block: &[("=begin", "=end")],
    long_strings: &[],
    strings: &['"', '\''],
    escapes: true,
};

/// PHP takes `//`, `#`, and `/* */`.
const PHP: Syntax = Syntax {
    line: &["//", "#"],
    block: &[("/*", "*/")],
    long_strings: &[],
    strings: &['"', '\''],
    escapes: true,
};

/// The syntax table. A language absent here is **not blanked at all**
/// ([`strip_comments_for_regex`] returns the input unchanged) — which is the
/// safe default: a framework extractor for a language we have no syntax for
/// simply behaves as it did before, rather than having its source corrupted by
/// a wrong guess.
fn syntax_for(lang: Language) -> Option<Syntax> {
    Some(match lang {
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => JS_FAMILY,
        Language::Python => PYTHON,
        Language::Ruby => RUBY,
        Language::Php => PHP,
        Language::Java
        | Language::Kotlin
        | Language::Go
        | Language::Rust
        | Language::C
        | Language::Cpp
        | Language::CSharp
        | Language::Swift
        | Language::Scala => C_FAMILY,
        _ => return None,
    })
}

/// Blank every comment body and string body in `content`, preserving byte
/// length and every newline's byte offset. See the module docs for why both
/// properties are load-bearing.
///
/// An unterminated comment or string (a truncated file) blanks to end of input
/// rather than erroring — extraction is best-effort and never throws.
pub fn strip_comments_for_regex(content: &str, lang: Language) -> String {
    let Some(syn) = syntax_for(lang) else {
        return content.to_string();
    };

    let src = content.as_bytes();
    let mut out = src.to_vec();
    let mut i = 0usize;

    // Blank `[from, to)` to spaces, keeping newlines where they are.
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in out.iter_mut().take(to).skip(from) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };

    'scan: while i < src.len() {
        // --- line comments -------------------------------------------------
        for opener in syn.line {
            if src[i..].starts_with(opener.as_bytes()) {
                let end = src[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(src.len(), |p| i + p);
                // Keep the opener, blank the body.
                blank(&mut out, i + opener.len(), end);
                i = end;
                continue 'scan;
            }
        }

        // --- block comments ------------------------------------------------
        for (open, close) in syn.block {
            if src[i..].starts_with(open.as_bytes()) {
                let body = i + open.len();
                let end = find(src, close.as_bytes(), body).map_or(src.len(), |p| p);
                blank(&mut out, body, end);
                i = (end + close.len()).min(src.len());
                continue 'scan;
            }
        }

        // --- long strings (python triple-quotes) — BEFORE single-char ------
        //
        // SKIPPED, not blanked. A Python triple-quoted string used as a
        // docstring may well contain a commented-out route; leaving it intact is
        // consistent with every other string (see the module docs).
        for delim in syn.long_strings {
            if src[i..].starts_with(delim.as_bytes()) {
                let body = i + delim.len();
                let end = find(src, delim.as_bytes(), body).map_or(src.len(), |p| p);
                i = (end + delim.len()).min(src.len());
                continue 'scan;
            }
        }

        // --- single-char strings -------------------------------------------
        //
        // Skipped, contents preserved — this is what keeps `"http://x"` from
        // starting a line comment while leaving every route path readable.
        let ch = src[i];
        if syn.strings.iter().any(|&c| c as u8 == ch) {
            let mut j = i + 1;
            while j < src.len() {
                if syn.escapes && src[j] == b'\\' {
                    j += 2; // skip the escaped byte, whatever it is
                    continue;
                }
                if src[j] == ch {
                    break;
                }
                // A raw newline ends a single-/double-quoted string in every
                // language here. Without this, one stray apostrophe (`don't` in
                // a comment we already skipped, or in prose) would swallow the
                // rest of the file as "inside a string" and every later comment
                // would go unblanked.
                if src[j] == b'\n' && ch != b'`' {
                    break;
                }
                j += 1;
            }
            let end = j.min(src.len());
            i = if end < src.len() && src[end] == ch {
                end + 1
            } else {
                end
            };
            continue 'scan;
        }

        i += 1;
    }

    // Every byte we rewrote is an ASCII space and every byte we kept is
    // original, so the result is still valid UTF-8. If that ever stops holding,
    // fall back to the untouched source rather than panicking (errors are
    // collected, never thrown).
    String::from_utf8(out).unwrap_or_else(|_| content.to_string())
}

/// First occurrence of `needle` in `hay` at or after `from`.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}
