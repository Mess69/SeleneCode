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
fn no_fn_ref_specs_for_languages_without_a_row() {
    // Task 15b lands Go/Rust/Java/Kotlin/C#/Ruby/PHP — until then those
    // languages capture nothing (and must not panic).
    let code = "func target() {}\nfunc reg() { register(target) }\n";
    let r = extract_from_source("m.go", code, Language::Go);
    assert!(fn_refs(&r).is_empty(), "got {:?}", fn_ref_names(&r));
}
