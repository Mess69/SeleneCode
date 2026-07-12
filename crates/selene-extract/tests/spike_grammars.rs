//! Phase 2 Task 1 spike: tree-sitter 0.26 native API + the 13 pinned v0
//! grammars (12 crates — `tree-sitter-typescript` carries TS and TSX).
//! Kept as a grammar-parity smoke test: if a grammar bump ever changes the
//! node-type names the language configs (Tasks 6–14) depend on, this file
//! fails before the configs silently drift.
//!
//! Discovery log lives next to each assertion; divergences from
//! `extraction-langs.md` §Per-language highlights are marked `DIVERGENCE`.
//!
//! # Divergences from the TS-era node-type names (verified 2026-07-12)
//!
//! - **Kotlin (kotlin-ng 1.1.0 — different lineage than the WASM grammar TS
//!   used):** identifiers are `identifier`, NOT `simple_identifier` — the
//!   map's `nameField: 'simple_identifier'` becomes `identifier` in Task 11.
//!   `function_value_parameters`, `class_declaration`, `companion_object`
//!   carry over unchanged; properties are `property_declaration`, ctor params
//!   live under `primary_constructor`/`class_parameters`/`class_parameter`.
//! - **Kotlin `fun interface`:** kotlin-ng parses it CLEANLY (a
//!   `class_declaration` with a bodiless `function_declaration` member; no
//!   ERROR node) — the TS 2-pattern ERROR-node recovery is DROPPED in
//!   Task 11 ([`kotlin_fun_interface_probe`]).
//! - **PHP (verified, brief asked):** `function_definition` (free fn),
//!   `method_declaration`, `class_declaration`, `namespace_definition`;
//!   names are `name` nodes, variables `variable_name`; the `<?php` opener
//!   is a `php_tag` node (the `php` LanguageFn, mixed HTML mode).
//! - **Go (verified, brief asked):** `method_declaration` exists as a
//!   distinct top-level node kind (alongside `function_declaration`), so
//!   `methodsAreTopLevel` routing carries over as-is.
//! - **C# preprocessor:** tree-sitter-c-sharp 0.23.5 parses `#if/#else/
//!   #endif` natively (named `preproc_if`/`preproc_else` nodes, no ERROR).
//!   The TS-parity line-blanking pre-parse still applies: post-blanking BOTH
//!   branch members surface as ordinary `property_declaration`s (asserted),
//!   which is the member-visibility behavior extraction wants.
//! - Everything else matched the map's names exactly: TS
//!   (`method_definition`, `public_field_definition`, `lexical_declaration`,
//!   `arrow_function`), Python (`function_definition`, `decorator`), Rust
//!   (`function_item`, `impl_item`, `trait_item`, `function_signature_item`,
//!   `scoped_identifier`), Java (`object_creation_expression`), C
//!   (`init_declarator`, `type_qualifier`), C++ (`qualified_identifier`,
//!   `namespace_definition`), Ruby (`call`, `body_statement`).
//!
//! # API notes (tree-sitter 0.26.11)
//!
//! - Grammar crates export `LanguageFn` consts (`LANGUAGE`, or
//!   `LANGUAGE_TYPESCRIPT`/`LANGUAGE_TSX`, `LANGUAGE_PHP`/`LANGUAGE_PHP_ONLY`);
//!   `.into()` converts to `tree_sitter::Language`. All load on core 0.26
//!   (ABI window 13–15) — no version conflicts across the 12 pinned crates.
//! - `Node::child(i)`/`named_child(i)` take `u32` but `child_count()` returns
//!   `usize` — index loops need casts; cursor walks avoid the mismatch.
//! - Cancellation: `parse_with_options(read, old_tree, Some(ParseOptions::
//!   new().progress_callback(&mut f)))` where `f: FnMut(&ParseState) ->
//!   ControlFlow<()>`; `Break(())` aborts and the call returns `None`;
//!   `ParseState` exposes `current_byte_offset()`/`has_error()`. Workable for
//!   `SELENE_PARSE_TIMEOUT_MS` as a deadline check in the callback; parser is
//!   reusable after `reset()` ([`cancellation_parse_with_options_progress_callback`]).
//! - Rows and columns are 0-based; ALL offsets/columns are BYTES (multi-byte
//!   UTF-8 asserted in [`positions_are_zero_based_rows_cols_and_byte_offsets`]).
//! - Kind-id precompute for Task 5: `Language::id_for_node_kind(kind, true)`
//!   → stable non-zero `u16` (0 = unknown kind), `Node::kind_id()` compares
//!   as an integer — cheap to build per-grammar ID sets once.
//!
//! # BOM decision (probe: [`bom_prefixed_source_probe`])
//!
//! A leading U+FEFF (3 bytes) parses cleanly in Python, C# and PHP — no
//! ERROR node; rows are unaffected; the first line's byte columns (and
//! `start_byte`) shift by 3. Decision: **KEEP** (parse the bytes as-is, no
//! strip): node ids embed lines, not columns, so ids are unaffected; byte
//! ranges keep matching the on-disk content (a strip would desync every
//! offset against the file); the +3 first-line column skew is accepted and
//! documented. PHP note: BOM before `<?php` becomes leading `text` in mixed
//! HTML mode — the php tag still opens PHP parsing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use tree_sitter::{Language, ParseOptions, Parser, Point, Tree};

/// Build a parser for `lang` and parse `src`, asserting the parse produced a
/// tree (tree-sitter only returns `None` on cancellation/timeout — a plain
/// `parse` never does).
fn parse(lang: &Language, src: &str) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(lang).unwrap();
    parser.parse(src, None).unwrap()
}

/// Every node kind in the tree (named nodes only — the extraction configs
/// match named node types), plus `"ERROR"` if any error node is present.
fn named_kinds(tree: &Tree) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cursor = tree.walk();
    let mut reached_root = false;
    while !reached_root {
        let node = cursor.node();
        if node.is_named() {
            out.insert(node.kind().to_string());
        }
        if node.is_error() {
            out.insert("ERROR".to_string());
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
        }
    }
    out
}

/// Number of nodes of exactly `kind` in the tree (cursor walk — note the
/// 0.26 API quirk: `Node::child(i)` takes `u32` while `child_count()`
/// returns `usize`, so index loops need casts; cursor walks avoid it).
fn count_kind(tree: &Tree, kind: &str) -> usize {
    let mut count = 0;
    let mut cursor = tree.walk();
    let mut reached_root = false;
    while !reached_root {
        if cursor.node().kind() == kind {
            count += 1;
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
        }
    }
    count
}

/// Parse + assert every kind in `expected` is present; dump the full
/// inventory on `--nocapture` so grammar drift is diagnosable from output.
fn assert_kinds(label: &str, lang: &Language, src: &str, expected: &[&str]) -> BTreeSet<String> {
    let tree = parse(lang, src);
    let kinds = named_kinds(&tree);
    eprintln!("[{label}] kinds: {kinds:?}");
    for want in expected {
        assert!(
            kinds.contains(*want),
            "[{label}] expected node kind '{want}' missing; inventory: {kinds:?}"
        );
    }
    kinds
}

fn ts_lang() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

// =============================================================================
// The 13 grammars: node-type names the configs depend on
// =============================================================================

#[test]
fn typescript_node_types() {
    let src = r#"
export class Greeter {
  name: string = "hi";
  greet = (who: string) => `hello ${who}`;
  method(x: number): number { return this.helper(x); }
}
const doubler = (a: number) => a * 2;
"#;
    let kinds = assert_kinds(
        "typescript",
        &ts_lang(),
        src,
        &[
            "method_definition",
            "public_field_definition",
            "lexical_declaration",
            "arrow_function",
            "export_statement",
            "call_expression",
            "class_declaration",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn tsx_node_types() {
    let src = r#"
const App = () => <div className="x">{msg}</div>;
export default function Page() { return <App />; }
"#;
    let kinds = assert_kinds(
        "tsx",
        &tree_sitter_typescript::LANGUAGE_TSX.into(),
        src,
        &[
            "jsx_element",
            "jsx_self_closing_element",
            "arrow_function",
            "lexical_declaration",
            "function_declaration",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn javascript_node_types() {
    let src = r#"
class A {
  m() { return make(); }
}
const f = () => new A();
f();
"#;
    let kinds = assert_kinds(
        "javascript",
        &tree_sitter_javascript::LANGUAGE.into(),
        src,
        &[
            "method_definition",
            "lexical_declaration",
            "arrow_function",
            "call_expression",
            "new_expression",
            "class_declaration",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn python_node_types() {
    let src = r#"
@decorator
def foo(x):
    return bar(x)

class C:
    def method(self):
        pass
"#;
    let kinds = assert_kinds(
        "python",
        &tree_sitter_python::LANGUAGE.into(),
        src,
        &[
            "function_definition",
            "decorator",
            "class_definition",
            "call",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn rust_node_types() {
    let src = r#"
use std::collections::HashMap;
trait T {
    fn required(&self);
}
struct S;
impl T for S {
    fn required(&self) { helper(); }
}
fn helper() {}
"#;
    let kinds = assert_kinds(
        "rust",
        &tree_sitter_rust::LANGUAGE.into(),
        src,
        &[
            "function_item",
            "impl_item",
            "trait_item",
            "scoped_identifier",
            "struct_item",
            "use_declaration",
            "call_expression",
            "function_signature_item",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn go_node_types() {
    let src = r#"
package main

type Point struct{ X int }
type Reader interface{ Read() }

func (p *Point) Move(dx int) { p.X += dx }
func New() Point { return Point{X: 1} }
"#;
    let kinds = assert_kinds(
        "go",
        &tree_sitter_go::LANGUAGE.into(),
        src,
        &[
            "method_declaration",
            "composite_literal",
            "type_spec",
            "struct_type",
            "interface_type",
            "function_declaration",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn java_node_types() {
    let src = r#"
package com.example;

public class Foo {
    public Foo make() { return new Foo(); }
}
"#;
    let kinds = assert_kinds(
        "java",
        &tree_sitter_java::LANGUAGE.into(),
        src,
        &[
            "object_creation_expression",
            "class_declaration",
            "method_declaration",
            "package_declaration",
            "scoped_identifier",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn kotlin_node_types() {
    let src = r#"
class Repo(val name: String) {
    companion object {
        val DEFAULT = Repo("x")
    }
    fun fetch(id: Int): String {
        return name + id
    }
}
"#;
    // DIVERGENCE: kotlin-ng names identifiers `identifier`, not the WASM
    // lineage's `simple_identifier` (see the module-doc divergence log).
    let kinds = assert_kinds(
        "kotlin",
        &tree_sitter_kotlin_ng::LANGUAGE.into(),
        src,
        &[
            "identifier",
            "function_value_parameters",
            "class_declaration",
            "companion_object",
            "function_declaration",
            "property_declaration",
            "primary_constructor",
        ],
    );
    assert!(!kinds.contains("ERROR"));
    assert!(
        !kinds.contains("simple_identifier"),
        "kotlin-ng does not emit simple_identifier — Task 11 must use `identifier`"
    );
}

#[test]
fn c_node_types() {
    let src = r#"
const int MAX = 10;
struct point { int x; };
typedef struct point point_t;
enum color { RED };
int add(int a, int b) { return a + b; }
"#;
    let kinds = assert_kinds(
        "c",
        &tree_sitter_c::LANGUAGE.into(),
        src,
        &[
            "init_declarator",
            "struct_specifier",
            "enum_specifier",
            "type_definition",
            "function_definition",
            "type_qualifier",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn cpp_node_types() {
    let src = r#"
namespace ns {
class Widget {
 public:
  int size() const;
};
int Widget::size() const { return 1; }
}
"#;
    let kinds = assert_kinds(
        "cpp",
        &tree_sitter_cpp::LANGUAGE.into(),
        src,
        &[
            "namespace_definition",
            "qualified_identifier",
            "class_specifier",
            "function_definition",
            "field_declaration",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn csharp_node_types_and_preprocessor_blanking() {
    // Raw form: what the grammar does with directives untouched.
    let raw = r#"
namespace App {
    public class Service {
#if DEBUG
        public int Mode => 1;
#else
        public int Mode => 2;
#endif
        public void Run() { var s = new Service(); }
    }
}
"#;
    let lang: Language = tree_sitter_c_sharp::LANGUAGE.into();
    let raw_kinds = assert_kinds(
        "csharp-raw",
        &lang,
        raw,
        &[
            "class_declaration",
            "method_declaration",
            "object_creation_expression",
        ],
    );
    // Task 1 review rider (Minor 2): the raw-parse observation is a pinned
    // fact, not a printout — this class-member-list `#if` parses natively as
    // `preproc_if` with no ERROR (the #237 misparse needs the enum-member
    // shape). If a grammar bump changes either, this fails loudly instead of
    // silently shifting the blanker's rationale.
    assert!(
        raw_kinds.contains("preproc_if"),
        "raw C# directives must surface as preproc_if nodes, got: {raw_kinds:?}"
    );
    assert!(
        !raw_kinds.contains("ERROR"),
        "raw class-member-list #if must parse without ERROR"
    );

    // Blanked form: the extractor's pre-parse blanks each directive line with
    // equal-length spaces (newlines kept) so BOTH branches survive as members.
    let blanked: String = raw
        .lines()
        .map(|l| {
            if l.trim_start().starts_with('#') {
                " ".repeat(l.len())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tree = parse(&lang, &blanked);
    let kinds = named_kinds(&tree);
    eprintln!("[csharp-blanked] kinds: {kinds:?}");
    assert!(!kinds.contains("ERROR"), "blanked C# must parse clean");
    // Both #if branches must be visible as members post-blanking.
    let mode_count = blanked.matches("int Mode").count();
    assert_eq!(mode_count, 2);
    assert_eq!(
        count_kind(&tree, "property_declaration"),
        2,
        "post-blanking both #if/#else members must parse as property_declaration"
    );
}

#[test]
fn php_node_types() {
    let src = r#"<?php
namespace App;
class Svc {
    public function run(): self { return $this; }
}
function helper() { return 1; }
"#;
    let kinds = assert_kinds(
        "php",
        &tree_sitter_php::LANGUAGE_PHP.into(),
        src,
        &[
            "function_definition",
            "method_declaration",
            "class_declaration",
            "namespace_definition",
        ],
    );
    assert!(!kinds.contains("ERROR"));
}

#[test]
fn ruby_node_types() {
    let src = r#"
module M
  class C
    include Comparable
    def m(x)
      helper(x)
    end
  end
end
"#;
    let kinds = assert_kinds(
        "ruby",
        &tree_sitter_ruby::LANGUAGE.into(),
        src,
        &["call", "body_statement", "class", "module", "method"],
    );
    assert!(!kinds.contains("ERROR"));
}

// =============================================================================
// Walker mechanics
// =============================================================================

#[test]
fn field_names_children_order_and_kind_ids() {
    let lang: Language = tree_sitter_rust::LANGUAGE.into();
    let src = "fn helper(a: u32) -> u32 { a }\nfn second() {}\n";
    let tree = parse(&lang, src);
    let root = tree.root_node();

    // child_by_field_name
    let first_fn = root.named_child(0).unwrap();
    assert_eq!(first_fn.kind(), "function_item");
    let name = first_fn.child_by_field_name("name").unwrap();
    assert_eq!(&src[name.byte_range()], "helper");

    // named_children iteration order = source order
    let order: Vec<&str> = {
        let mut c = root.walk();
        root.named_children(&mut c).map(|n| n.kind()).collect()
    };
    assert_eq!(order, vec!["function_item", "function_item"]);
    let names: Vec<String> = {
        let mut c = root.walk();
        root.named_children(&mut c)
            .map(|n| src[n.child_by_field_name("name").unwrap().byte_range()].to_string())
            .collect()
    };
    assert_eq!(names, vec!["helper", "second"]);

    // kind-id precompute (Task 5 candidate): id_for_node_kind gives a stable
    // u16 per grammar; node.kind_id() compares as an integer.
    let fn_id = lang.id_for_node_kind("function_item", true);
    assert_ne!(fn_id, 0);
    assert_eq!(first_fn.kind_id(), fn_id);
    assert_eq!(lang.id_for_node_kind("no_such_kind", true), 0);
}

#[test]
fn positions_are_zero_based_rows_cols_and_byte_offsets() {
    let lang: Language = tree_sitter_python::LANGUAGE.into();
    // 'é' is 2 bytes in UTF-8: byte columns after it exceed char columns.
    let src = "s = \"héllo\"\ndef f():\n    pass\n";
    let tree = parse(&lang, src);
    let root = tree.root_node();

    // Python wraps a top-level assignment in expression_statement; the
    // `assignment` node (with its `right` field) is one level down.
    let stmt = root.named_child(0).unwrap();
    assert_eq!(stmt.kind(), "expression_statement");
    assert_eq!(stmt.start_position(), Point { row: 0, column: 0 });
    let assign = stmt.named_child(0).unwrap();
    assert_eq!(assign.kind(), "assignment");

    let def = root.named_child(1).unwrap();
    assert_eq!(def.kind(), "function_definition");
    assert_eq!(def.start_position().row, 1, "rows are 0-based");
    assert_eq!(def.start_position().column, 0, "columns are 0-based");

    // byte offsets: line 0 is 12 bytes + '\n' = def starts at byte 13
    // ("s = \"héllo\"" = 11 chars but 12 bytes because of é).
    assert_eq!(src.lines().next().unwrap().len(), 12);
    assert_eq!(def.start_byte(), 13, "offsets are BYTES, not chars");
    // The string node's end column is a byte column too.
    let string_node = assign
        .child_by_field_name("right")
        .expect("assignment has right field");
    assert_eq!(&src[string_node.byte_range()], "\"héllo\"");
    assert_eq!(
        string_node.end_position().column,
        12,
        "end column counts bytes (é = 2)"
    );
}

#[test]
fn cancellation_parse_with_options_progress_callback() {
    // tree-sitter 0.26 cancellation: ParseOptions::progress_callback returning
    // ControlFlow::Break(()) aborts the parse and parse_with_options returns
    // None. This is the per-parse safety-net mechanism for
    // SELENE_PARSE_TIMEOUT_MS (a deadline check inside the callback).
    let lang: Language = tree_sitter_javascript::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).unwrap();

    // Large enough that the parser reports progress at least once mid-parse.
    let big_src = "const x = [1,2,3];\n".repeat(50_000);
    let bytes = big_src.as_bytes();

    let mut calls = 0u32;
    let mut cancel = |_state: &tree_sitter::ParseState| {
        calls += 1;
        ControlFlow::Break(())
    };
    let mut read = |byte: usize, _pos: Point| {
        if byte < bytes.len() {
            &bytes[byte..]
        } else {
            &[]
        }
    };
    let cancelled = parser.parse_with_options(
        &mut read,
        None,
        Some(ParseOptions::new().progress_callback(&mut cancel)),
    );
    assert!(cancelled.is_none(), "Break(()) must cancel the parse");
    assert!(calls > 0, "progress callback must have fired");

    // After a cancelled parse the same parser must be reusable (reset).
    parser.reset();
    let ok = parser.parse("const y = 1;", None);
    assert!(ok.is_some(), "parser must be reusable after cancellation");
}

// =============================================================================
// Plan-review probe fixtures
// =============================================================================

#[test]
fn kotlin_fun_interface_probe() {
    // TS used a different-lineage WASM Kotlin grammar that misparsed
    // `fun interface` with ERROR nodes (extraction-langs.md: 2 ERROR-node
    // recovery patterns). Task 11's recovery-vs-drop decision depends on
    // whether kotlin-ng reproduces the misparse.
    let src = r#"
fun interface Transformer {
    fun transform(x: Int): Int
}
"#;
    let tree = parse(&tree_sitter_kotlin_ng::LANGUAGE.into(), src);
    let kinds = named_kinds(&tree);
    eprintln!("[kotlin fun interface] kinds: {kinds:?}");
    eprintln!(
        "[kotlin fun interface] root sexp: {}",
        tree.root_node().to_sexp()
    );
    assert!(
        !kinds.contains("ERROR"),
        "kotlin-ng parses `fun interface` cleanly — Task 11 drops the TS ERROR-recovery"
    );
    // Hardening (Task 1 review Minor): `has_error()` also covers MISSING
    // nodes, which the ERROR-kind scan above cannot see — kotlin-ng emits
    // MISSING `_class_member_semi` for single-line class bodies, so a
    // fixture regression would otherwise slip through as "no ERROR".
    assert!(
        !tree.root_node().has_error(),
        "fun-interface probe must be fully clean (no ERROR *or* MISSING nodes)"
    );
}

#[test]
fn bom_prefixed_source_probe() {
    // BOM (U+FEFF, 3 bytes in UTF-8) at file start. Decision: KEEP (no
    // strip) — see the module docs' BOM section. Evidence asserted here:
    // clean parses, rows unaffected, first-line byte columns shifted by 3.

    // Python
    let src = "\u{feff}def f():\n    pass\n";
    let tree = parse(&tree_sitter_python::LANGUAGE.into(), src);
    let kinds = named_kinds(&tree);
    eprintln!("[bom python] kinds: {kinds:?}");
    assert!(!kinds.contains("ERROR"), "BOM python must parse clean");
    let def = tree.root_node().named_child(0).unwrap();
    assert_eq!(def.kind(), "function_definition");
    assert_eq!(def.start_position().row, 0, "BOM adds no row");
    assert_eq!(def.start_byte(), 3, "BOM shifts line-0 byte offsets by 3");
    assert_eq!(def.start_position().column, 3, "…and byte columns by 3");
    // ids embed (1-based) lines, not columns → BOM cannot change a node id.

    // C# — the most BOM-prone ecosystem in v0.
    let cs = "\u{feff}namespace App { public class C { } }\n";
    let cs_kinds = named_kinds(&parse(&tree_sitter_c_sharp::LANGUAGE.into(), cs));
    eprintln!("[bom csharp] kinds: {cs_kinds:?}");
    assert!(!cs_kinds.contains("ERROR"), "BOM C# must parse clean");
    assert!(cs_kinds.contains("class_declaration"));

    // PHP — BOM sits before `<?php`; in mixed HTML mode it becomes leading
    // text and the php tag still opens PHP parsing.
    let php = "\u{feff}<?php\nfunction f() { return 1; }\n";
    let php_kinds = named_kinds(&parse(&tree_sitter_php::LANGUAGE_PHP.into(), php));
    eprintln!("[bom php] kinds: {php_kinds:?}");
    assert!(!php_kinds.contains("ERROR"), "BOM PHP must parse clean");
    assert!(php_kinds.contains("function_definition"));
}
