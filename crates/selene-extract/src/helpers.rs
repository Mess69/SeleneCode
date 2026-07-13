//! Shared AST helpers ported from the TS `tree-sitter-helpers.ts` (the leaf
//! module the core extractor and every language config lean on): node text,
//! field access, and the #780 docstring capture/cleanup whose output is
//! user-visible in MCP — **byte parity with TS matters**.
//!
//! (`generateNodeId` from the same TS file already lives in
//! `selene_core::ids` — Task 2.)

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::Node;

/// The text a node spans, as a byte slice of `source` (`node.byte_range()` —
/// tree-sitter offsets are bytes, Task 1 spike).
pub fn get_node_text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

/// Child node by field name (thin alias for parity with the TS helper set).
pub fn get_child_by_field<'t>(node: Node<'t>, field: &str) -> Option<Node<'t>> {
    node.child_by_field_name(field)
}

/// Node types that *wrap* a declaration so a leading comment is a sibling of
/// the wrapper, not of the emitted (inner) declaration node. Before looking
/// for a preceding comment we climb out through these. In the common case a
/// wrapper holds one declaration; when it holds several (`const a = 1,
/// b = 2;` puts two `variable_declarator`s under one `lexical_declaration`),
/// every declarator receives the wrapper's leading comment — TS-parity, and
/// arguably desirable (the comment documents the whole statement). (#780)
static DOCSTRING_WRAPPER_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "export_statement",     // JS/TS: export class/function/const ...
        "decorated_definition", // Python: @decorator over def/class
        "lexical_declaration",  // JS/TS: const/let x = () => {}
        "variable_declaration", // JS/TS: var x = ...
        "variable_declarator",  // JS/TS: the `x = () => {}` inside the declaration
        "ambient_declaration",  // TS: declare ...
    ])
});

/// Grammar node kinds that count as doc comments when they immediately
/// precede a declaration (as named siblings).
const COMMENT_KINDS: [&str; 4] = [
    "comment",
    "line_comment",
    "block_comment",
    "documentation_comment",
];

/// Strip comment-syntax markers from a raw comment so the stored docstring
/// is just the prose. Covers the marker styles across every supported
/// language: C-family line/block comments and their doc variants, Rust/
/// Swift/Kotlin `///`/`//!` doc lines, hash lines (Python/Ruby/shell),
/// Erlang `%` lines, Lua/Luau line and long-bracket comments, and Pascal
/// brace and paren-star comments. (#780)
///
/// Paired block delimiters are stripped only when the comment OPENS with
/// one, so a line comment that merely happens to END with a closing
/// delimiter is never truncated. Per-line markers are anchored at line
/// start, so they're safe to apply to any comment.
///
/// Known negligible `\s` divergence vs the TS original (complete set
/// difference): JS `\s` includes U+FEFF (BOM) which Rust's does not, and
/// Rust `\s` includes U+0085 (NEL) which JS's does not — a marker followed
/// by one of those exact characters strips differently. No known
/// real-corpus case on either side.
pub fn clean_comment_markers(raw: &str) -> String {
    // Paired block delimiters — applied once (anchored), only when the
    // comment opens with the matching style. Order and patterns mirror the
    // TS `cleanCommentMarkers` byte-for-byte.
    static C_OPEN: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"^/\*+!?").unwrap()
    });
    static C_CLOSE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"\*+/$").unwrap()
    });
    static LUA_OPEN: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"^--\[=*\[").unwrap()
    });
    static LUA_CLOSE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"\]=*\]$").unwrap()
    });
    static PAS_PAREN_OPEN: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"^\(\*").unwrap()
    });
    static PAS_PAREN_CLOSE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"\*\)$").unwrap()
    });
    static PAS_BRACE_OPEN: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"^\{").unwrap()
    });
    static PAS_BRACE_CLOSE: LazyLock<Regex> = LazyLock::new(|| {
        #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
        Regex::new(r"\}$").unwrap()
    });
    // Per-line markers (multiline-anchored, replace-all — the TS `gm` set).
    static LINE_MARKERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
            r"(?m)^//[/!]?\s?", // //, and Rust/Swift doc lines /// //!
            r"(?m)^--\s?",      // Lua/Luau line comments
            r"(?m)^#\s?",       // Python/Ruby/shell line comments
            r"(?m)^%+\s?",      // Erlang line comments (% / %% / %%%)
            r"(?m)^\s*\*\s?",   // block-comment continuation (* foo) — \s verbatim from TS
        ]
        .iter()
        .map(|p| {
            #[allow(clippy::unwrap_used)] // literal patterns, compile-time known good
            Regex::new(p).unwrap()
        })
        .collect()
    });

    let mut c = raw.trim().to_string();
    if c.starts_with("/*") {
        c = C_OPEN.replace(&c, "").into_owned();
        c = C_CLOSE.replace(&c, "").into_owned();
    } else if c.starts_with("--[") {
        c = LUA_OPEN.replace(&c, "").into_owned();
        c = LUA_CLOSE.replace(&c, "").into_owned();
    } else if c.starts_with("(*") {
        c = PAS_PAREN_OPEN.replace(&c, "").into_owned();
        c = PAS_PAREN_CLOSE.replace(&c, "").into_owned();
    } else if c.starts_with('{') {
        c = PAS_BRACE_OPEN.replace(&c, "").into_owned();
        c = PAS_BRACE_CLOSE.replace(&c, "").into_owned();
    }
    for re in LINE_MARKERS.iter() {
        c = re.replace_all(&c, "").into_owned();
    }
    c.trim().to_string()
}

/// The docstring/comment run preceding `node` in `source`: climbs out of
/// `DOCSTRING_WRAPPER_TYPES` wrappers, then collects the unbroken chain of
/// preceding named comment siblings (source order), cleans each with
/// [`clean_comment_markers`], joins with `\n`, trims. `None` when there is
/// no preceding comment at all. (#780)
pub fn get_preceding_docstring(node: Node<'_>, source: &str) -> Option<String> {
    // Climb out of any wrapper(s) so a comment preceding the WHOLE construct
    // (export-, decorator-, or const-arrow-wrapped) is reachable as a
    // sibling. The emitted node's own previous named sibling is empty
    // (export/const) or a decorator (Python) in those cases — without this
    // the docstring was dropped. (#780)
    let mut anchor = node;
    while let Some(parent) = anchor.parent() {
        if DOCSTRING_WRAPPER_TYPES.contains(parent.kind()) {
            anchor = parent;
        } else {
            break;
        }
    }

    let mut comments: Vec<&str> = Vec::new();
    let mut sibling = anchor.prev_named_sibling();
    while let Some(s) = sibling {
        if COMMENT_KINDS.contains(&s.kind()) {
            comments.push(get_node_text(s, source));
            sibling = s.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    // Collected nearest-first; restore source order (the TS `unshift`).
    comments.reverse();
    let joined = comments
        .into_iter()
        .map(clean_comment_markers)
        .collect::<Vec<_>>()
        .join("\n");
    Some(joined.trim().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tree_sitter::{Language as TsLanguage, Parser, Tree};

    use super::*;

    fn parse(lang: &TsLanguage, src: &str) -> Tree {
        let mut parser = Parser::new();
        parser.set_language(lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    /// First node of `kind`, depth-first.
    fn find_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        children.into_iter().find_map(|c| find_kind(c, kind))
    }

    /// First node of `kind` whose text contains `needle`.
    fn find_kind_containing<'t>(
        node: Node<'t>,
        kind: &str,
        needle: &str,
        source: &str,
    ) -> Option<Node<'t>> {
        if node.kind() == kind && get_node_text(node, source).contains(needle) {
            return Some(node);
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        children
            .into_iter()
            .find_map(|c| find_kind_containing(c, kind, needle, source))
    }

    #[test]
    fn get_node_text_is_a_byte_slice() {
        let src = "fn f() { let s = \"héllo\"; }";
        let tree = parse(&tree_sitter_rust::LANGUAGE.into(), src);
        let s = find_kind(tree.root_node(), "string_literal").unwrap();
        assert_eq!(get_node_text(s, src), "\"héllo\"");
    }

    #[test]
    fn get_child_by_field_hits_and_misses() {
        let src = "fn helper() {}";
        let tree = parse(&tree_sitter_rust::LANGUAGE.into(), src);
        let f = find_kind(tree.root_node(), "function_item").unwrap();
        assert_eq!(
            get_node_text(get_child_by_field(f, "name").unwrap(), src),
            "helper"
        );
        assert!(get_child_by_field(f, "no_such_field").is_none());
    }

    // -------------------------------------------------------------------------
    // extraction.test.ts: 'captures docstrings for export- and const-wrapped
    // declarations (#780)' — helper-level port (called on the node the walker
    // emits; the wrapper climb is what's under test).
    // -------------------------------------------------------------------------
    #[test]
    fn ts_export_and_const_wrapped_docstrings() {
        let src = "\n// plain class control\nclass Ledger {}\n\n// exported class\nexport class Invoice {}\n\n// export default\nexport default function settle() { return true; }\n\n// exported arrow const\nexport const refund = (amount: number) => amount;\n\n// non-export arrow const\nconst audit = (amount: number) => amount;\n";
        let lang: TsLanguage = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let tree = parse(&lang, src);
        let root = tree.root_node();

        let doc = |kind: &str, needle: &str| {
            let n = find_kind_containing(root, kind, needle, src).unwrap();
            get_preceding_docstring(n, src)
        };
        assert_eq!(
            doc("class_declaration", "Ledger").as_deref(),
            Some("plain class control"),
            "control (unwrapped) still works"
        );
        assert_eq!(
            doc("class_declaration", "Invoice").as_deref(),
            Some("exported class")
        );
        assert_eq!(
            doc("function_declaration", "settle").as_deref(),
            Some("export default")
        );
        assert_eq!(
            doc("variable_declarator", "refund").as_deref(),
            Some("exported arrow const")
        );
        assert_eq!(
            doc("variable_declarator", "audit").as_deref(),
            Some("non-export arrow const")
        );
    }

    // -------------------------------------------------------------------------
    // extraction.test.ts: 'does not mis-attribute a class comment to an
    // uncommented member (#780)'
    // -------------------------------------------------------------------------
    #[test]
    fn ts_no_misattribution_to_uncommented_member() {
        let src = "\n// Comment for Box\nexport class Box {\n  noComment() {}\n  // own comment\n  withComment() {}\n}\n";
        let lang: TsLanguage = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let tree = parse(&lang, src);
        let root = tree.root_node();

        let boxed = find_kind_containing(root, "class_declaration", "Box", src).unwrap();
        assert_eq!(
            get_preceding_docstring(boxed, src).as_deref(),
            Some("Comment for Box")
        );
        let no_comment = find_kind_containing(root, "method_definition", "noComment", src).unwrap();
        assert_eq!(
            get_preceding_docstring(no_comment, src),
            None,
            "no over-walk"
        );
        let with_comment =
            find_kind_containing(root, "method_definition", "withComment", src).unwrap();
        assert_eq!(
            get_preceding_docstring(with_comment, src).as_deref(),
            Some("own comment")
        );
    }

    // -------------------------------------------------------------------------
    // extraction.test.ts: 'captures docstrings for decorated Python
    // declarations, stripping `#` (#780)'
    // -------------------------------------------------------------------------
    #[test]
    fn python_decorated_declarations_strip_hash() {
        let src = "# decorated function\n@app.route(\"/x\")\ndef py_handler():\n    return 1\n\n\n# plain function control\ndef py_plain():\n    return 1\n\n\n# decorated class\n@dataclass\nclass PyModel:\n    pass\n";
        let tree = parse(&tree_sitter_python::LANGUAGE.into(), src);
        let root = tree.root_node();

        let handler = find_kind_containing(root, "function_definition", "py_handler", src).unwrap();
        assert_eq!(
            get_preceding_docstring(handler, src).as_deref(),
            Some("decorated function")
        );
        let plain = find_kind_containing(root, "function_definition", "py_plain", src).unwrap();
        assert_eq!(
            get_preceding_docstring(plain, src).as_deref(),
            Some("plain function control")
        );
        let model = find_kind_containing(root, "class_definition", "PyModel", src).unwrap();
        assert_eq!(
            get_preceding_docstring(model, src).as_deref(),
            Some("decorated class")
        );
    }

    // -------------------------------------------------------------------------
    // extraction.test.ts: 'cleans comment markers across language styles
    // (#780)' — Rust + C via real parses (their grammars are in v0); Lua and
    // Pascal styles via direct clean_comment_markers unit tests below (their
    // grammars are wave-2, not yet pinned).
    // -------------------------------------------------------------------------
    #[test]
    fn rust_doc_line_marker_stripped_on_real_parse() {
        let src = "/// rust doc line\nfn rs_fn() {}\n";
        let tree = parse(&tree_sitter_rust::LANGUAGE.into(), src);
        let f = find_kind(tree.root_node(), "function_item").unwrap();
        assert_eq!(
            get_preceding_docstring(f, src).as_deref(),
            Some("rust doc line")
        );
    }

    #[test]
    fn c_block_comment_clean_on_real_parse() {
        let src = "/* c block */\nvoid c_fn(void) {}\n";
        let tree = parse(&tree_sitter_c::LANGUAGE.into(), src);
        let f = find_kind(tree.root_node(), "function_definition").unwrap();
        assert_eq!(get_preceding_docstring(f, src).as_deref(), Some("c block"));
    }

    #[test]
    fn clean_comment_markers_all_styles() {
        // C-family
        assert_eq!(clean_comment_markers("// plain line"), "plain line");
        assert_eq!(clean_comment_markers("/// rust doc line"), "rust doc line");
        assert_eq!(
            clean_comment_markers("//! rust inner doc"),
            "rust inner doc"
        );
        assert_eq!(clean_comment_markers("/* c block */"), "c block");
        assert_eq!(
            clean_comment_markers("/**\n * line1\n * line2\n */"),
            "line1\nline2"
        );
        assert_eq!(clean_comment_markers("/*! doxygen bang */"), "doxygen bang");
        // Lua / Luau
        assert_eq!(clean_comment_markers("-- lua line"), "lua line");
        assert_eq!(clean_comment_markers("--[[ lua block ]]"), "lua block");
        assert_eq!(clean_comment_markers("--[=[ lua long ]=]"), "lua long");
        // Pascal
        assert_eq!(clean_comment_markers("{ pascal brace }"), "pascal brace");
        assert_eq!(clean_comment_markers("(* pascal paren *)"), "pascal paren");
        // Hash + Erlang lines
        assert_eq!(clean_comment_markers("# python line"), "python line");
        assert_eq!(clean_comment_markers("%% erlang line"), "erlang line");
        // A line comment that merely ENDS with a closing delimiter is never
        // truncated (open-delimiter gate).
        assert_eq!(clean_comment_markers("// ends with */"), "ends with */");
    }
}
