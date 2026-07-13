//! Function-as-value capture (#756) — Task 15a: the capture machinery + the
//! flush gate for C, C++, TS/JS and Python.
//!
//! Parity contract: `docs/reference/from-codegraph/design/function-ref-capture.md`
//! (per-language value positions + the 10 precision rules) and
//! `../codegraph/src/extraction/function-ref.ts` (`FN_REF_SPECS`) +
//! `tree-sitter.ts:603` (`flushFnRefCandidates`, the gate).
//!
//! Capture side ONLY: these tests assert the `function_ref`
//! `UnresolvedReference`s the extractor emits. Resolution (unique-or-drop,
//! class-scoped `this.X`, overload refusal) is Phase 3 — no edges here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, UnresolvedReference, extract_from_source};

/// Every `function_ref` candidate that survived the gate, in emission order.
fn fn_refs(r: &ExtractionResult) -> Vec<&UnresolvedReference> {
    r.unresolved
        .iter()
        .filter(|u| u.reference_kind == "function_ref")
        .collect()
}

/// The names of the surviving candidates, in emission order.
fn fn_ref_names(r: &ExtractionResult) -> Vec<&str> {
    fn_refs(r)
        .iter()
        .map(|u| u.reference_name.as_str())
        .collect()
}

/// The id of the (unique) node with this kind + name — the expected
/// `from_node_id` of a candidate captured inside it.
fn id_of(r: &ExtractionResult, kind: NodeKind, name: &str) -> String {
    r.nodes
        .iter()
        .find(|n| n.kind == kind && n.name == name)
        .unwrap_or_else(|| panic!("no {kind:?} node named {name}"))
        .id
        .clone()
}

/// Assert exactly one candidate, with exact name / from-node / line / column.
fn assert_one(r: &ExtractionResult, name: &str, from: &str, line: u32, column: u32) {
    let refs = fn_refs(r);
    assert_eq!(
        refs.len(),
        1,
        "expected exactly 1 function_ref, got {:?}",
        fn_ref_names(r)
    );
    let u = refs[0];
    assert_eq!(u.reference_name, name);
    assert_eq!(u.from_node_id, from, "from_node_id");
    assert_eq!(u.line, Some(line), "line");
    assert_eq!(u.column, Some(column), "column");
    // `function_ref` is an INTERNAL reference kind — it must never persist as
    // an edge kind (map §Wire; design doc §Mechanism).
    assert!(
        !r.edges.iter().any(|e| e.kind.as_str() == "function_ref"),
        "function_ref must never become an edge kind"
    );
}

// =============================================================================
// C — argument_list, assignment_expression.right, initializer_pair.value,
//     initializer_list / init_declarator.value, `&fn` (pointer_expression).
// Design doc §Per-language value positions, C row; function-ref.ts:142 cFamilySpec.
// =============================================================================

#[test]
fn c_captures_bare_id_in_call_arguments() {
    let code = r#"
void target_cb(int x) {}

void reg(void) {
    register_handler(target_cb);
}
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "target_cb", &from, 5, 21);
}

#[test]
fn c_captures_assignment_rhs() {
    let code = r#"
void target_cb(int x) {}

void setup(struct ops *o) {
    o->cb = target_cb;
}
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    let from = id_of(&r, NodeKind::Function, "setup");
    assert_one(&r, "target_cb", &from, 5, 12);
}

#[test]
fn c_captures_designated_initializer_value() {
    let code = r#"
void recv_cb(int x) {}

static struct ops my_ops = { .recv = recv_cb };
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    // File-scope initializers are attributed to the FILE node (the walker's
    // variable branch does not push the variable's scope — TS parity).
    assert_one(&r, "recv_cb", "file:ops.c", 4, 37);
}

#[test]
fn c_captures_initializer_list_elements() {
    let code = r#"
void cb_a(int x) {}
void cb_b(int x) {}

static cb_t table[] = { cb_a, cb_b };
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    assert_eq!(fn_ref_names(&r), vec!["cb_a", "cb_b"]);
    assert!(fn_refs(&r).iter().all(|u| u.from_node_id == "file:ops.c"));
}

#[test]
fn c_captures_address_of_wrapper() {
    let code = r#"
void target_cb(int x) {}

void reg(void) {
    signal(2, &target_cb);
}
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    let from = id_of(&r, NodeKind::Function, "reg");
    // The candidate's position is the INNER identifier's, not the `&`.
    assert_one(&r, "target_cb", &from, 5, 15);
}

#[test]
fn c_dereference_is_not_a_function_value() {
    // `&x` and `*x` share `pointer_expression`; only `&` is an address-of.
    // Without the operator check, fmt's `*begin` reads resolved to its free
    // `begin()` functions (design doc rule 4).
    let code = r#"
int total(void) { return 1; }

void use(int *total) {
    consume(*total);
}
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn c_param_forward_assignment_is_skipped() {
    // `o->cb = cb` — the assigned member's name EQUALS the RHS identifier, so
    // the RHS is a forwarded parameter, not a function value (design doc
    // rule 6). A same-named function elsewhere would be the WRONG target.
    let code = r#"
void cb(int x) {}

void setup(struct ops *o, void (*cb)(int)) {
    o->cb = cb;
}
"#;
    let r = extract_from_source("ops.c", code, Language::C);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

// =============================================================================
// C++ — `&` forms ONLY in args/rhs/varinit (addressOfOnly); bare ids qualify
// ONLY in FILE-scope initializer tables. Design doc rule 4; function-ref.ts:377.
// =============================================================================

#[test]
fn cpp_bare_id_in_args_dropped_while_address_of_kept() {
    let code = r#"
void target_cb(int x) {}
void other_cb(int x) {}

void reg() {
    register_handler(&target_cb);
    register_handler(other_cb);
}
"#;
    let r = extract_from_source("h.cpp", code, Language::Cpp);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "target_cb", &from, 6, 22);
}

#[test]
fn cpp_captures_qualified_member_pointer() {
    let code = r#"
class Widget {
public:
    void on_click(int x) {}
    void wire() {
        bind(&Widget::on_click);
    }
};
"#;
    let r = extract_from_source("w.cpp", code, Language::Cpp);
    let from = id_of(&r, NodeKind::Method, "wire");
    // The QUALIFIED name is kept — resolution scopes the method to the class.
    // The position is the inner `qualified_identifier`'s, not the `&`'s.
    assert_one(&r, "Widget::on_click", &from, 6, 14);
}

#[test]
fn cpp_captures_address_of_in_assignment_rhs() {
    // `rhs` is a GATED position (design doc rule 2 — only file-scope
    // initializers are ungated) AND an `address_of_only` one (rule 4). An `&`
    // form must survive BOTH: explicit_ref clears the C++ bare-id drop, and the
    // name clears the gate by matching a same-file function.
    let code = r#"
void target_cb(int x) {}

void setup(Ops *o) {
    o->cb = &target_cb;
}
"#;
    let r = extract_from_source("h.cpp", code, Language::Cpp);
    let from = id_of(&r, NodeKind::Function, "setup");
    assert_one(&r, "target_cb", &from, 5, 13);
}

#[test]
fn cpp_captures_address_of_in_local_varinit() {
    // Same for `varinit` — a LOCAL initializer stays gated (only FILE-scope
    // ones skip the gate), so this pins the `&` form surviving there too.
    let code = r#"
void target_cb(int x) {}

void setup() {
    auto p = &target_cb;
}
"#;
    let r = extract_from_source("h.cpp", code, Language::Cpp);
    let from = id_of(&r, NodeKind::Function, "setup");
    assert_one(&r, "target_cb", &from, 5, 14);
}

#[test]
fn cpp_bare_id_in_local_varinit_is_dropped() {
    // …while a BARE id in the same local position is dropped by
    // `address_of_only` — the file-scope table is the ONLY place C++ accepts
    // one (design doc rule 4).
    let code = r#"
void target_cb(int x) {}

void setup() {
    auto p = target_cb;
}
"#;
    let r = extract_from_source("h.cpp", code, Language::Cpp);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn cpp_file_scope_initializer_table_keeps_bare_ids() {
    let code = r#"
void cb_a(int x) {}
void cb_b(int x) {}

static handler_t table[] = { cb_a, cb_b };
"#;
    let r = extract_from_source("t.cpp", code, Language::Cpp);
    assert_eq!(fn_ref_names(&r), vec!["cb_a", "cb_b"]);
    assert!(fn_refs(&r).iter().all(|u| u.from_node_id == "file:t.cpp"));
}

// =============================================================================
// TS / JS — arguments, assignment_expression.right, pair.value,
//           array / variable_declarator.value, `this.method`.
// function-ref.ts:177 TS_JS_SPEC.
// =============================================================================

#[test]
fn ts_captures_bare_id_in_call_arguments() {
    let code = r#"
function handleClick(e: Event) {}

export function wire(btn: any) {
  btn.addEventListener('click', handleClick);
}
"#;
    let r = extract_from_source("ui.ts", code, Language::Typescript);
    let from = id_of(&r, NodeKind::Function, "wire");
    assert_one(&r, "handleClick", &from, 5, 32);
}

#[test]
fn ts_captures_assignment_rhs() {
    let code = r#"
function handleClick() {}

export function wire(obj: any) {
  obj.cb = handleClick;
}
"#;
    let r = extract_from_source("ui.ts", code, Language::Typescript);
    let from = id_of(&r, NodeKind::Function, "wire");
    assert_one(&r, "handleClick", &from, 5, 11);
}

#[test]
fn ts_captures_object_pair_value() {
    let code = r#"
function renderHome() {}

export const routes = { home: renderHome };
"#;
    let r = extract_from_source("routes.ts", code, Language::Typescript);
    assert_one(&r, "renderHome", "file:routes.ts", 4, 30);
}

#[test]
fn ts_captures_array_elements() {
    let code = r#"
function handleA() {}
function handleB() {}

export const handlers = [handleA, handleB];
"#;
    let r = extract_from_source("h.ts", code, Language::Typescript);
    assert_eq!(fn_ref_names(&r), vec!["handleA", "handleB"]);
    assert!(fn_refs(&r).iter().all(|u| u.from_node_id == "file:h.ts"));
}

#[test]
fn ts_captures_variable_declarator_value() {
    let code = r#"
function handleClick() {}

export const cb = handleClick;
"#;
    let r = extract_from_source("v.ts", code, Language::Typescript);
    assert_one(&r, "handleClick", "file:v.ts", 4, 18);
}

#[test]
fn ts_captures_this_member_as_prefixed_candidate() {
    let code = r#"
class Panel {
  onResize() {}
  mount(el: any) {
    el.addEventListener('resize', this.onResize);
  }
}
"#;
    let r = extract_from_source("panel.ts", code, Language::Typescript);
    let from = id_of(&r, NodeKind::Method, "mount");
    // `this.`-PREFIXED so resolution can scope it to the enclosing class
    // (design doc rule 3); the position is the PROPERTY's.
    assert_one(&r, "this.onResize", &from, 5, 39);
}

#[test]
fn ts_destructuring_is_never_a_function_alias() {
    // `const { center } = ellipse` extracts DATA from the RHS, never a
    // function alias (design doc rule 7) — without the skip, the RHS
    // `ellipse` matches the same-file function and produces a wrong edge.
    let code = r#"
function ellipse() {}

export const { center } = ellipse;
"#;
    let r = extract_from_source("d.ts", code, Language::Typescript);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn js_captures_bare_id_in_call_arguments() {
    let code = r#"
function handleClick() {}

export function wire(btn) {
  btn.addEventListener('click', handleClick);
}
"#;
    let r = extract_from_source("ui.js", code, Language::Javascript);
    let from = id_of(&r, NodeKind::Function, "wire");
    assert_one(&r, "handleClick", &from, 5, 32);
}

// =============================================================================
// Python — argument_list + keyword_argument.value, assignment.right,
//          pair.value, list, `self.method` (attribute).
// function-ref.ts:189 PYTHON_SPEC.
// =============================================================================

#[test]
fn py_captures_bare_id_in_call_arguments() {
    let code = r#"
def target_cb(x):
    pass

def reg():
    register(target_cb)
"#;
    let r = extract_from_source("reg.py", code, Language::Python);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "target_cb", &from, 6, 13);
}

#[test]
fn py_captures_keyword_argument_value() {
    let code = r#"
def worker():
    pass

def start():
    t = Thread(target=worker)
"#;
    let r = extract_from_source("t.py", code, Language::Python);
    let from = id_of(&r, NodeKind::Function, "start");
    assert_one(&r, "worker", &from, 6, 22);
}

#[test]
fn py_captures_assignment_rhs() {
    let code = r#"
def target_cb(x):
    pass

handler = target_cb
"#;
    let r = extract_from_source("h.py", code, Language::Python);
    assert_one(&r, "target_cb", "file:h.py", 5, 10);
}

#[test]
fn py_captures_dict_pair_value() {
    let code = r#"
def target_cb(x):
    pass

handlers = {"recv": target_cb}
"#;
    let r = extract_from_source("h.py", code, Language::Python);
    assert_one(&r, "target_cb", "file:h.py", 5, 20);
}

#[test]
fn py_captures_list_elements() {
    let code = r#"
def cb_a(x):
    pass

def cb_b(x):
    pass

handlers = [cb_a, cb_b]
"#;
    let r = extract_from_source("h.py", code, Language::Python);
    assert_eq!(fn_ref_names(&r), vec!["cb_a", "cb_b"]);
    assert!(fn_refs(&r).iter().all(|u| u.from_node_id == "file:h.py"));
}

#[test]
fn py_captures_self_method_attribute() {
    let code = r#"
class Panel:
    def handle_click(self, e):
        pass

    def mount(self, btn):
        btn.on("click", self.handle_click)
"#;
    let r = extract_from_source("panel.py", code, Language::Python);
    let from = id_of(&r, NodeKind::Method, "mount");
    // Python's `self.m` keeps METHOD targets through its own capture shape —
    // the name is BARE (no `this.` prefix), so it rides the normal gate
    // (design doc rule 3).
    assert_one(&r, "handle_click", &from, 7, 29);
}

// =============================================================================
// The gate (`flush_fn_ref_candidates`) — tree-sitter.ts:603.
// =============================================================================

#[test]
fn gate_drops_unknown_names() {
    // A candidate survives only if its name matches a same-file function/
    // method or an imported binding (design doc rule 1). A local/param
    // passed as an argument is dropped before it reaches the DB.
    let code = r#"
def reg(unknown_thing):
    register(unknown_thing)
"#;
    let r = extract_from_source("u.py", code, Language::Python);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn gate_keeps_imported_bindings() {
    let code = r#"
from helpers import assist

def reg():
    register(assist)
"#;
    let r = extract_from_source("i.py", code, Language::Python);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "assist", &from, 5, 13);
}

#[test]
fn gate_skipped_for_c_file_scope_initializer_tables() {
    // C has no symbol imports and registers callbacks cross-file at repo
    // scale (redis `server.c`'s command table names handlers from `t_*.c`).
    // A FILE-scope initializer is a constant-expression context, so a bare
    // identifier there can only be a function address (design doc rule 2).
    let code = r#"
static cb_t table[] = { external_cb };
static struct ops o = { .recv = other_file_cb };
"#;
    let r = extract_from_source("srv.c", code, Language::C);
    assert_eq!(fn_ref_names(&r), vec!["external_cb", "other_file_cb"]);
}

#[test]
fn gate_still_applies_to_c_local_assignments() {
    // `rhs`/`varinit` stay GATED: `prev = next`, `*str = field` each matched a
    // unique same-named function somewhere and produced wrong edges when
    // ungated (redis/jemalloc — design doc rule 2).
    let code = r#"
void setup(struct ops *o) {
    o->cb = some_unknown_name;
}
"#;
    let r = extract_from_source("srv.c", code, Language::C);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn gate_produces_nothing_for_generated_files() {
    // Minified/generated files (`*.min.js` + the codegen patterns) produce NO
    // fn-ref candidates — single-letter minified symbols resolve everywhere
    // (design doc rule 8; Alamofire's vendored jquery).
    let code = r#"
function handleClick() {}

export const handlers = [handleClick];
"#;
    // Same source, non-generated path: the candidate survives.
    let ok = extract_from_source("app.js", code, Language::Javascript);
    assert_eq!(fn_ref_names(&ok), vec!["handleClick"]);

    let minified = extract_from_source("vendor/app.min.js", code, Language::Javascript);
    assert!(
        fn_refs(&minified).is_empty(),
        "got {:?}",
        fn_ref_names(&minified)
    );
}

#[test]
fn gate_dedups_repeated_candidates_per_scope() {
    // Dedup key is `(from_node_id, name)` — a callback registered twice in
    // one function yields ONE ref.
    let code = r#"
def target_cb(x):
    pass

def reg():
    register(target_cb)
    register_again(target_cb)
"#;
    let r = extract_from_source("d.py", code, Language::Python);
    assert_eq!(fn_ref_names(&r), vec!["target_cb"]);
}

#[test]
fn ts_class_field_object_literal_captures_class_and_property_scoped() {
    // TS `tree-sitter.ts:996-1010` runs BOTH walks over a #808-demoted class
    // field: `visitFunctionBody(value)` under the PROPERTY scope, then
    // `scanFnRefSubtree(field)` under the CLASS scope. Two distinct
    // `from_node_id`s, so the flush dedup on `(from_node_id, name)` keeps both
    // — TS emits 2 candidates here and so do we (before the initializer walk
    // landed, the Rust port emitted only the class-scoped one).
    let code = r#"
function onClick() {}

class Panel {
  static handlers = { click: onClick };
}
"#;
    let r = extract_from_source("panel.ts", code, Language::Typescript);
    let refs = fn_refs(&r);
    assert_eq!(fn_ref_names(&r), vec!["onClick", "onClick"], "{refs:?}");

    let panel = id_of(&r, NodeKind::Class, "Panel");
    let handlers = id_of(&r, NodeKind::Property, "handlers");
    let mut froms: Vec<&str> = refs.iter().map(|u| u.from_node_id.as_str()).collect();
    froms.sort_unstable();
    let mut want = vec![panel.as_str(), handlers.as_str()];
    want.sort_unstable();
    assert_eq!(froms, want, "one class-scoped + one property-scoped");
}

#[test]
fn wave_two_languages_capture_nothing() {
    // Task 15b closed the v0 set (Go/Rust/Java/Kotlin/C#/Ruby/PHP now have
    // rows — see below). A wave-2 language has no grammar and no spec: it must
    // capture nothing and must not panic. (The registry itself is pinned by
    // `spec_registry_covers_every_v0_language` in src/fnref.rs.)
    let code = "func target() {}\nfunc reg() { register(target) }\n";
    let r = extract_from_source("m.swift", code, Language::Swift);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

// =============================================================================
// Task 15b — the remaining v0 languages.
// =============================================================================

// -----------------------------------------------------------------------------
// Go — argument_list, assignment_statement / short_var_declaration
//      (expression_list), keyed_element, literal_value / var_spec.value.
// function-ref.ts:201 GO_SPEC.
// -----------------------------------------------------------------------------

#[test]
fn go_captures_bare_id_in_call_arguments() {
    let code = r#"
package m

func targetCb() {}

func reg() {
	register(targetCb)
}
"#;
    let r = extract_from_source("m.go", code, Language::Go);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "targetCb", &from, 7, 10);
}

#[test]
fn go_captures_assignment_expression_list() {
    // `expression_list` FANS OUT to all named children (Go multi-assign).
    let code = r#"
package m

func cbA() {}
func cbB() {}

func reg() {
	h, g = cbA, cbB
}
"#;
    let r = extract_from_source("m.go", code, Language::Go);
    assert_eq!(fn_ref_names(&r), vec!["cbA", "cbB"]);
}

#[test]
fn go_captures_short_var_declaration() {
    let code = r#"
package m

func targetCb() {}

func reg() {
	h := targetCb
	_ = h
}
"#;
    let r = extract_from_source("m.go", code, Language::Go);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "targetCb", &from, 7, 6);
}

#[test]
fn go_captures_keyed_element() {
    // No `value` field on `keyed_element` — the value is the LAST named child.
    let code = r#"
package m

func targetCb() {}

func reg() {
	ops := Ops{Cb: targetCb}
	_ = ops
}
"#;
    let r = extract_from_source("m.go", code, Language::Go);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "targetCb", &from, 7, 16);
}

#[test]
fn go_captures_literal_value_elements() {
    let code = r#"
package m

func cbA() {}
func cbB() {}

func reg() {
	tbl := []F{cbA, cbB}
	_ = tbl
}
"#;
    let r = extract_from_source("m.go", code, Language::Go);
    assert_eq!(fn_ref_names(&r), vec!["cbA", "cbB"]);
}

#[test]
fn go_captures_var_spec_value() {
    let code = r#"
package m

func targetCb() {}

var V = targetCb
"#;
    let r = extract_from_source("m.go", code, Language::Go);
    assert_one(&r, "targetCb", "file:m.go", 6, 8);
}

// -----------------------------------------------------------------------------
// Rust — arguments, assignment_expression.right, field_initializer.value,
//        array_expression, static_item / let_declaration.value.
// function-ref.ts:217 RUST_SPEC.
// -----------------------------------------------------------------------------

#[test]
fn rust_captures_bare_id_in_call_arguments() {
    let code = r#"
fn target_cb() {}

fn reg() {
    register(target_cb);
}
"#;
    let r = extract_from_source("m.rs", code, Language::Rust);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "target_cb", &from, 5, 13);
}

#[test]
fn rust_captures_assignment_rhs() {
    let code = r#"
fn target_cb() {}

fn reg(h: &mut F) {
    *h = target_cb;
}
"#;
    let r = extract_from_source("m.rs", code, Language::Rust);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "target_cb", &from, 5, 9);
}

#[test]
fn rust_captures_field_initializer_value() {
    let code = r#"
fn target_cb() {}

fn reg() {
    let o = Ops { cb: target_cb };
}
"#;
    let r = extract_from_source("m.rs", code, Language::Rust);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "target_cb", &from, 5, 22);
}

#[test]
fn rust_captures_array_expression() {
    let code = r#"
fn cb_a() {}
fn cb_b() {}

fn reg() {
    let t = [cb_a, cb_b];
}
"#;
    let r = extract_from_source("m.rs", code, Language::Rust);
    assert_eq!(fn_ref_names(&r), vec!["cb_a", "cb_b"]);
}

#[test]
fn rust_captures_let_declaration_and_static_item() {
    let code = r#"
fn target_cb() {}

static S: F = target_cb;

fn reg() {
    let v = target_cb;
}
"#;
    let r = extract_from_source("m.rs", code, Language::Rust);
    // `static_item` at file scope, `let_declaration` inside the function.
    assert_eq!(fn_ref_names(&r), vec!["target_cb", "target_cb"]);
    let from = id_of(&r, NodeKind::Function, "reg");
    let froms: Vec<&str> = fn_refs(&r)
        .iter()
        .map(|u| u.from_node_id.as_str())
        .collect();
    assert_eq!(froms, vec!["file:m.rs", from.as_str()]);
}

// -----------------------------------------------------------------------------
// Java — argument_list, assignment_expression.right, variable_declarator.value;
//        `method_reference` is the only WRAPPER form (bare ids never qualify).
// function-ref.ts:229 JAVA_SPEC + :625 normalizeSpecial.
// -----------------------------------------------------------------------------

#[test]
fn java_captures_type_method_reference_in_args() {
    let code = r#"
class M {
  static void cb() {}
  void reg() {
    register(M::cb);
  }
}
"#;
    let r = extract_from_source("M.java", code, Language::Java);
    let from = id_of(&r, NodeKind::Method, "reg");
    // Qualified `Type::method` — resolution suffix-anchors it to that type.
    assert_one(&r, "M::cb", &from, 5, 16);
}

#[test]
fn java_captures_this_method_reference() {
    let code = r#"
class M {
  void cb() {}
  void reg() {
    register(this::cb);
  }
}
"#;
    let r = extract_from_source("M.java", code, Language::Java);
    let from = id_of(&r, NodeKind::Method, "reg");
    // `this::m` / `super::m` route through the class-scoped `this.` resolver.
    assert_one(&r, "this.cb", &from, 5, 19);
}

#[test]
fn java_captures_assignment_rhs_method_reference() {
    let code = r#"
class M {
  static void cb() {}
  void reg(Runnable h) {
    h = M::cb;
  }
}
"#;
    let r = extract_from_source("M.java", code, Language::Java);
    let from = id_of(&r, NodeKind::Method, "reg");
    assert_one(&r, "M::cb", &from, 5, 11);
}

#[test]
fn java_captures_field_variable_declarator_method_reference() {
    let code = r#"
class M {
  static void cb() {}
  Runnable r = M::cb;
}
"#;
    let r = extract_from_source("M.java", code, Language::Java);
    let from = id_of(&r, NodeKind::Class, "M");
    assert_one(&r, "M::cb", &from, 4, 18);
}

#[test]
fn java_bare_identifier_is_never_a_function_value() {
    // Java has NO bare-identifier function values — only method references
    // (function-ref.ts:230: `idTypes: new Set<string>()`).
    let code = r#"
class M {
  static void cb() {}
  void reg() {
    register(cb);
  }
}
"#;
    let r = extract_from_source("M.java", code, Language::Java);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn java_constructor_reference_yields_nothing() {
    // `Type::new` has no method node to land on (function-ref.ts:640).
    let code = r#"
class M {
  void reg() {
    supply(M::new);
  }
}
"#;
    let r = extract_from_source("M.java", code, Language::Java);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

// -----------------------------------------------------------------------------
// Kotlin — value_arguments, assignment (last child); wrappers
//          `callable_reference` (`::f`) and `navigation_expression` (`this::m`).
// function-ref.ts:240 KOTLIN_SPEC. NOTE: kotlin-ng drift — see fnref.rs.
// -----------------------------------------------------------------------------

#[test]
fn kotlin_captures_callable_reference_in_args() {
    let code = r#"
fun targetCb() {}

fun reg() {
    register(::targetCb)
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "targetCb", &from, 5, 15);
}

#[test]
fn kotlin_captures_this_navigation_reference() {
    let code = r#"
class M {
    fun fire() {}
    fun reg() {
        register(this::fire)
    }
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    let from = id_of(&r, NodeKind::Method, "reg");
    assert_one(&r, "this.fire", &from, 5, 23);
}

#[test]
fn kotlin_captures_type_qualified_reference() {
    let code = r#"
fun reg() {
    register(Other::handle)
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    let from = id_of(&r, NodeKind::Function, "reg");
    // Qualified — skips the gate (the referenced type needs no import in the
    // same package; resolution is scope-suffix-anchored + unique-or-drop).
    assert_one(&r, "Other::handle", &from, 3, 20);
}

#[test]
fn kotlin_variable_receiver_reference_is_dropped() {
    // `subscriber::onNext` — the receiver's type is statically unknown (the
    // deferred obj.method class; design doc §Known limits).
    let code = r#"
fun onNext() {}

fun reg(subscriber: S) {
    register(subscriber::onNext)
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn kotlin_plain_navigation_is_not_a_function_value() {
    // Ordinary `a.b` navigation is a DATA read, never a function value.
    let code = r#"
fun prop() {}

fun reg(obj: O) {
    register(obj.prop)
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn kotlin_captures_assignment_rhs() {
    // `assignment` has no rhs FIELD — the value is the last named child.
    let code = r#"
fun targetCb() {}

fun reg(h: F) {
    h = ::targetCb
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "targetCb", &from, 5, 10);
}

#[test]
fn kotlin_bare_identifier_is_never_a_function_value() {
    let code = r#"
fun targetCb() {}

fun reg() {
    register(targetCb)
}
"#;
    let r = extract_from_source("M.kt", code, Language::Kotlin);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

// -----------------------------------------------------------------------------
// C# — argument_list (`argument` layer), assignment_expression.right (incl.
//      `+=`), initializer_expression, variable_declarator, `this.M`.
// function-ref.ts:250 CSHARP_SPEC.
// -----------------------------------------------------------------------------

#[test]
fn csharp_captures_bare_id_in_argument() {
    let code = r#"
class M {
  void TargetCb() {}
  void Reg() {
    Register(TargetCb);
  }
}
"#;
    let r = extract_from_source("M.cs", code, Language::CSharp);
    let from = id_of(&r, NodeKind::Method, "Reg");
    assert_one(&r, "TargetCb", &from, 5, 13);
}

#[test]
fn csharp_captures_event_subscription_rhs() {
    // `+=` event subscription rides the same `assignment_expression.right`.
    let code = r#"
class M {
  void TargetCb() {}
  void Reg() {
    ev += TargetCb;
  }
}
"#;
    let r = extract_from_source("M.cs", code, Language::CSharp);
    let from = id_of(&r, NodeKind::Method, "Reg");
    assert_one(&r, "TargetCb", &from, 5, 10);
}

#[test]
fn csharp_captures_initializer_expression() {
    let code = r#"
class M {
  void TargetCb() {}
  void Reg() {
    var l = new List<Action> { TargetCb };
  }
}
"#;
    let r = extract_from_source("M.cs", code, Language::CSharp);
    let from = id_of(&r, NodeKind::Method, "Reg");
    assert_one(&r, "TargetCb", &from, 5, 31);
}

#[test]
fn csharp_captures_variable_declarator() {
    // `variable_declarator` has NO value field — the no-field varinit path
    // (≥2 named children, never the name child).
    let code = r#"
class M {
  void TargetCb() {}
  void Reg() {
    Action a = TargetCb;
  }
}
"#;
    let r = extract_from_source("M.cs", code, Language::CSharp);
    let from = id_of(&r, NodeKind::Method, "Reg");
    assert_one(&r, "TargetCb", &from, 5, 15);
}

#[test]
fn csharp_captures_this_member_access() {
    // The vendored grammar keeps `this` ANONYMOUS (only `name` is a named
    // child) — the receiver check falls back to the node text
    // (function-ref.ts:738-745). The candidate name is BARE (C# keeps method
    // targets), unlike TS/JS's `this.`-prefixed form.
    let code = r#"
class M {
  void Run0() {}
  void Reg() {
    Register(this.Run0);
  }
}
"#;
    let r = extract_from_source("M.cs", code, Language::CSharp);
    let from = id_of(&r, NodeKind::Method, "Reg");
    assert_one(&r, "Run0", &from, 5, 18);
}

// -----------------------------------------------------------------------------
// Ruby — `method(:sym)` / `&method(:sym)` ONLY (bare ids are calls/locals),
//        plus the hook DSLs → class-scoped `this.<sym>`.
// function-ref.ts:262 RUBY_SPEC, :282 RUBY_HOOK_RE, :700/:797 normalizeSpecial.
// -----------------------------------------------------------------------------

#[test]
fn ruby_captures_method_symbol_idiom() {
    let code = r#"
class M
  def target_cb
  end

  def reg
    register(method(:target_cb))
  end
end
"#;
    let r = extract_from_source("m.rb", code, Language::Ruby);
    let from = id_of(&r, NodeKind::Method, "reg");
    assert_one(&r, "target_cb", &from, 7, 20);
}

#[test]
fn ruby_captures_block_argument_method_symbol() {
    // `&method(:sym)` — the `block_argument` LAYER is transparent.
    let code = r#"
class M
  def target_cb
  end

  def reg
    each(&method(:target_cb))
  end
end
"#;
    let r = extract_from_source("m.rb", code, Language::Ruby);
    let from = id_of(&r, NodeKind::Method, "reg");
    assert_one(&r, "target_cb", &from, 7, 17);
}

#[test]
#[ignore = "BLOCKED on a walker import-branch parity bug (see task-15b-report.md, Blocker section): Ruby's import_types is [call] (require/require_relative) and the Rust ladder's import branch skips children, so every class-scope Ruby call is consumed and its argument_list is never walked (it loses its calls refs too). TS (tree-sitter.ts:1173-1175) does NOT set skipChildren there. The fix is one line in walker/mod.rs, which Task 15b is scoped out of."]
fn ruby_captures_hook_dsl_symbol() {
    // `before_action :authenticate` — the symbol names a method of the
    // ENCLOSING class, so it routes through the class-scoped `this.` resolver
    // (which also walks superclasses — ApplicationController inheritance).
    let code = r#"
class M
  before_action :authenticate

  def authenticate
  end
end
"#;
    let r = extract_from_source("m.rb", code, Language::Ruby);
    let from = id_of(&r, NodeKind::Class, "M");
    assert_one(&r, "this.authenticate", &from, 3, 16);
}

#[test]
#[ignore = "BLOCKED on a walker import-branch parity bug (see task-15b-report.md, Blocker section): Ruby's import_types is [call] (require/require_relative) and the Rust ladder's import branch skips children, so every class-scope Ruby call is consumed and its argument_list is never walked (it loses its calls refs too). TS (tree-sitter.ts:1173-1175) does NOT set skipChildren there. The fix is one line in walker/mod.rs, which Task 15b is scoped out of."]
fn ruby_captures_rescue_from_with_pair() {
    let code = r#"
class M
  rescue_from E, with: :render_404

  def render_404
  end
end
"#;
    let r = extract_from_source("m.rb", code, Language::Ruby);
    let from = id_of(&r, NodeKind::Class, "M");
    assert_one(&r, "this.render_404", &from, 3, 23);
}

#[test]
#[ignore = "BLOCKED on a walker import-branch parity bug (see task-15b-report.md, Blocker section): Ruby's import_types is [call] (require/require_relative) and the Rust ladder's import branch skips children, so every class-scope Ruby call is consumed and its argument_list is never walked (it loses its calls refs too). TS (tree-sitter.ts:1173-1175) does NOT set skipChildren there. The fix is one line in walker/mod.rs, which Task 15b is scoped out of."]
fn ruby_validates_symbols_are_attributes_not_methods() {
    // `validates` (PLURAL) is EXCLUDED — its symbols name ATTRIBUTES, not
    // methods (function-ref.ts:279-283). Singular `validate` IS a hook.
    let code = r#"
class M
  validates :name
  validate :name_ok

  def name_ok
  end
end
"#;
    let r = extract_from_source("m.rb", code, Language::Ruby);
    // ONLY the singular `validate` hook survives.
    assert_eq!(fn_ref_names(&r), vec!["this.name_ok"]);
}

#[test]
fn ruby_bare_identifier_is_never_a_function_value() {
    // Bare identifiers in Ruby args are method CALLS or locals, never values.
    let code = r#"
class M
  def target_cb
  end

  def reg
    register(target_cb)
  end
end
"#;
    let r = extract_from_source("m.rb", code, Language::Ruby);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

// -----------------------------------------------------------------------------
// PHP — string callables ONLY as args of PHP_CALLABLE_HOFS (ungated);
//       `[$this,'m']` → `this.m`; `[Foo::class,'m']` / `'Cls::m'` → qualified.
// function-ref.ts:347 PHP_CALLABLE_HOFS, :360 PHP_SPEC, :753/:771 normalizeSpecial.
// -----------------------------------------------------------------------------

#[test]
fn php_captures_hof_string_callable() {
    // PHP globals are referenced cross-file WITHOUT imports, so the gate can't
    // see them — the strong positional prior (a string argument to `usort`)
    // plus resolution's unique-or-drop rule carry the precision instead. So
    // `cmp_items` survives even though it is NOT defined in this file.
    let code = r#"<?php
function reg() {
    usort($a, 'cmp_items');
}
"#;
    let r = extract_from_source("m.php", code, Language::Php);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "cmp_items", &from, 3, 14);
}

#[test]
fn php_non_hof_string_is_dropped() {
    // A string is only trustworthy as a callable in a KNOWN callable position
    // (design doc §Known limits — framework registries like WordPress
    // `add_action` are deliberately uncaptured).
    let code = r#"<?php
function reg() {
    other_fn('not_a_cb');
}
"#;
    let r = extract_from_source("m.php", code, Language::Php);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}

#[test]
fn php_captures_this_array_callable() {
    // `[$this, 'method']` is valid in ANY call's arguments — the shape itself
    // is unambiguous.
    let code = r#"<?php
class M {
    function handler() {}
    function reg() {
        register([$this, 'handler']);
    }
}
"#;
    let r = extract_from_source("m.php", code, Language::Php);
    let from = id_of(&r, NodeKind::Method, "reg");
    assert_one(&r, "this.handler", &from, 5, 25);
}

#[test]
fn php_captures_class_array_callable() {
    let code = r#"<?php
function reg() {
    register([Foo::class, 'handler']);
}
"#;
    let r = extract_from_source("m.php", code, Language::Php);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "Foo::handler", &from, 3, 26);
}

#[test]
fn php_captures_qualified_string_callable() {
    let code = r#"<?php
function reg() {
    call_user_func('Cls::m');
}
"#;
    let r = extract_from_source("m.php", code, Language::Php);
    let from = id_of(&r, NodeKind::Function, "reg");
    assert_one(&r, "Cls::m", &from, 3, 19);
}
