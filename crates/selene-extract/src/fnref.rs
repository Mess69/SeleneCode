//! Function-as-value capture (#756) — the `FN_REF_SPECS` port
//! (`../codegraph/src/extraction/function-ref.ts`; design contract:
//! `docs/reference/from-codegraph/design/function-ref-capture.md`).
//!
//! A function name used as a VALUE — passed as a call argument
//! (`register_handler(target_cb)`), assigned to a field or function pointer
//! (`o->cb = target_cb`), placed in a struct/object initializer
//! (`{ .recv_cb = my_cb }`, `{ recv: targetCb }`), or listed in a function
//! table (`static cb_t table[] = { cb_a, cb_b }`) — is a real dependency that
//! call extraction misses entirely: `callers(target_cb)` showed nothing but
//! direct calls, so every registered callback looked dead and its
//! registration sites were invisible to impact analysis.
//!
//! This module is the CAPTURE side only: the per-language spec table + the pure
//! "pull candidate names out of a dispatched container" function. The walkers
//! drive it ([`Session::maybe_capture_fn_refs`] and [`scan_fn_ref_subtree`] in
//! `walker/body.rs`) and the GATE (`flush_fn_ref_candidates`, same file) decides
//! which candidates become `function_ref` [`crate::UnresolvedReference`]s.
//! Resolution (unique-or-drop, class-scoped `this.X`, overload refusal) is
//! Phase 3.
//!
//! `function_ref` is an INTERNAL reference kind: resolution maps it to a
//! `references` edge (`metadata.fnRef = true`) — it never persists as an edge
//! kind (map §Wire).
//!
//! **Coverage.** All 13 v0 languages have rows: C, C++, TS/TSX/JS/JSX and
//! Python (Task 15a) plus Go, Rust, Java, Kotlin, C#, Ruby and PHP (Task 15b).
//! ObjC/Swift/Scala/Dart/Lua/Luau/Pascal rows exist in the TS source and land
//! with their grammars in wave 2.
//!
//! **Known gap (Task 15b):** Ruby's hook-DSL symbols (`before_action :auth`)
//! are specified here but never reach capture — the walker's import branch
//! consumes every class-scope Ruby `call` (Ruby's `import_types` is `["call"]`)
//! without walking its children, diverging from TS (`tree-sitter.ts:1173-1175`
//! leaves `skipChildren` false). The three `#[ignore]`d tests in
//! `tests/fnref_test.rs` flip green the moment that one-line walker fix lands.

use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::Node;

use crate::Language;
use crate::helpers::{get_child_by_field, get_node_text};

/// How to pull candidate value nodes out of a dispatched container node
/// (function-ref.ts:60 `CaptureMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureMode {
    /// Every named child is a potential value (call argument lists).
    Args,
    /// The assignment right-hand side (named field, else last named child).
    Rhs,
    /// The `value` field of a keyed pair (object/struct/table initializers).
    Value,
    /// Every named child (array / initializer-list / table positional elements).
    List,
    /// A variable declarator's initializer value.
    VarInit,
}

/// How one container node type yields candidate values (function-ref.ts:67).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureRule {
    pub(crate) mode: CaptureMode,
    /// Field holding the value for rhs/value/varinit (defaults per mode).
    pub(crate) field: Option<&'static str>,
}

const fn rule(mode: CaptureMode) -> CaptureRule {
    CaptureRule { mode, field: None }
}
const fn rule_field(mode: CaptureMode, field: &'static str) -> CaptureRule {
    CaptureRule {
        mode,
        field: Some(field),
    }
}

/// One language's capture spec (function-ref.ts:73 `FnRefSpec`).
///
/// The TS `Set`/`Map` fields become `&'static` slices: every spec has at most
/// a handful of entries, so linear scan beats a `LazyLock<HashMap>` and keeps
/// the whole table a compile-time constant.
pub(crate) struct FnRefSpec {
    /// Bare identifier node types that can act as a function value.
    id_types: &'static [&'static str],
    /// Container node type → how to extract candidate values from it.
    dispatch: &'static [(&'static str, CaptureRule)],
    /// Transparent wrapper layers between a container and its values
    /// (`argument`, `value_argument`, `literal_element`, `expression_list`…).
    /// Value: the field to descend into, or `None` for "named children"
    /// (`expression_list` fans out to ALL named children — Go multi-assign).
    layers: &'static [(&'static str, Option<&'static str>)],
    /// Unary wrappers whose operand is the function value — C/C++ `&fn`
    /// (`pointer_expression`). Value: operand field, or `None` for the first
    /// named child.
    unwrap: &'static [(&'static str, Option<&'static str>)],
    /// Whole-node reference forms needing bespoke name extraction — see
    /// [`normalize_special`].
    special: &'static [&'static str],
    /// Capture modes whose candidates skip the same-file/import gate and rely
    /// on resolution's unique-or-drop rule instead. C-family only: an
    /// initializer value or table element is a function-pointer position by
    /// construction, and C has no symbol imports — the dominant repo-scale
    /// pattern (redis `server.c`'s command table naming handlers defined
    /// across files) would otherwise be invisible. Call arguments stay gated
    /// everywhere (locals passed as args dwarf callbacks). The flush
    /// ADDITIONALLY requires FILE scope (see `flush_fn_ref_candidates`).
    ungated_modes: &'static [CaptureMode],
    /// C++ only: in args/rhs/varinit positions accept ONLY explicit reference
    /// forms (`&fn`, `&Cls::method`) — never bare identifiers. C++ codebases
    /// are dense with generic free-function names (`begin`, `end`, `out`,
    /// `size`, `data`) that collide with locals, and out-of-line member
    /// definitions extract as function-kind nodes — bare-id matching on fmt
    /// was mostly wrong edges (72 generic-name + 105 member/macro mismatches
    /// → 22 edges after the rule). File-scope initializer tables (value/list)
    /// still accept bare identifiers, same as C.
    pub(crate) address_of_only: bool,
}

impl FnRefSpec {
    /// All-empty spec to spread language rows from (`..EMPTY`), so a row
    /// states only what it uses — the TS specs' implicit-undefined fields,
    /// made explicit.
    const EMPTY: FnRefSpec = FnRefSpec {
        id_types: &[],
        dispatch: &[],
        layers: &[],
        unwrap: &[],
        special: &[],
        ungated_modes: &[],
        address_of_only: false,
    };

    fn is_id_type(&self, node_type: &str) -> bool {
        self.id_types.contains(&node_type)
    }
    /// The capture rule for a container node type, if it is one.
    pub(crate) fn dispatch_for(&self, node_type: &str) -> Option<CaptureRule> {
        self.dispatch
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, r)| *r)
    }
    /// `Some(field)` when `node_type` is a transparent layer — the inner
    /// `Option` distinguishes "descend into this field" from "descend into all
    /// named children".
    fn layer_for(&self, node_type: &str) -> Option<Option<&'static str>> {
        self.layers
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, f)| *f)
    }
    fn unwrap_for(&self, node_type: &str) -> Option<Option<&'static str>> {
        self.unwrap
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, f)| *f)
    }
    fn is_special(&self, node_type: &str) -> bool {
        self.special.contains(&node_type)
    }
    pub(crate) fn is_ungated_mode(&self, mode: CaptureMode) -> bool {
        self.ungated_modes.contains(&mode)
    }
}

/// Names that are never function references even when grammars call them
/// identifiers (function-ref.ts:121 `NAME_STOPLIST`).
const NAME_STOPLIST: [&str; 12] = [
    "this",
    "self",
    "super",
    "null",
    "nil",
    "true",
    "false",
    "undefined",
    "new",
    "NULL",
    "nullptr",
    "None",
];

// ---------------------------------------------------------------------------
// Per-language specs. Node types verified against each grammar (the #756 probe
// fixtures; design doc §Per-language value positions).
// ---------------------------------------------------------------------------

// C / C++ / ObjC share the C-family initializer & assignment shapes
// (function-ref.ts:142 `cFamilySpec`) — the tables are hoisted to consts so
// the two rows below can share them (a `const fn` returning `FnRefSpec` cannot
// borrow its own temporaries). ObjC (`@selector`) is wave 2.
const C_FAMILY_DISPATCH: [(&str, CaptureRule); 5] = [
    ("argument_list", rule(CaptureMode::Args)),
    (
        "assignment_expression",
        rule_field(CaptureMode::Rhs, "right"),
    ),
    ("init_declarator", rule_field(CaptureMode::VarInit, "value")),
    ("initializer_list", rule(CaptureMode::List)),
    ("initializer_pair", rule_field(CaptureMode::Value, "value")),
];
const C_FAMILY_UNWRAP: [(&str, Option<&str>); 1] = [("pointer_expression", Some("argument"))];
/// ONLY `value`/`list` (struct/array initializers): `rhs`/`varinit` were tried
/// and produced false edges (`prev = next`, `*str = field` — data assignments
/// matching a unique same-named function elsewhere), so assignments stay gated
/// to same-file/import (design doc rule 2).
const C_FAMILY_UNGATED: [CaptureMode; 2] = [CaptureMode::Value, CaptureMode::List];

const C_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &C_FAMILY_DISPATCH,
    unwrap: &C_FAMILY_UNWRAP,
    ungated_modes: &C_FAMILY_UNGATED,
    address_of_only: false,
    ..FnRefSpec::EMPTY
};

/// C++ is the same C-family row plus `address_of_only` (design doc rule 4).
const CPP_SPEC: FnRefSpec = FnRefSpec {
    address_of_only: true,
    ..C_SPEC
};

/// TS/JS (function-ref.ts:177 `TS_JS_SPEC`). `this.handleClick`
/// (`member_expression`) emits a `this.`-PREFIXED candidate: resolution scopes
/// it to the enclosing symbol's class, so `this.fonts` (a property, post-#808)
/// and inherited/unknown members yield no edge, while same-class methods —
/// `btn.on('click', this.handleClick)`, the observer-registration idiom —
/// resolve precisely. Bare identifiers stay function-kind-only (a bare id can
/// never be a method value in JS — design doc rule 3).
const TS_JS_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", rule(CaptureMode::Args)),
        (
            "assignment_expression",
            rule_field(CaptureMode::Rhs, "right"),
        ),
        (
            "variable_declarator",
            rule_field(CaptureMode::VarInit, "value"),
        ),
        ("pair", rule_field(CaptureMode::Value, "value")),
        ("array", rule(CaptureMode::List)),
    ],
    special: &["member_expression"],
    ..FnRefSpec::EMPTY
};

/// Python (function-ref.ts:189 `PYTHON_SPEC`). `self.handle_click`
/// (`attribute`) yields a BARE member name — unlike TS/JS's `this.`-prefixed
/// form it rides the normal gate, and Python's capture shape keeps method
/// targets (design doc rule 3).
const PYTHON_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("argument_list", rule(CaptureMode::Args)),
        ("assignment", rule_field(CaptureMode::Rhs, "right")),
        // `Thread(target=worker)`
        ("keyword_argument", rule_field(CaptureMode::Value, "value")),
        ("pair", rule_field(CaptureMode::Value, "value")),
        ("list", rule(CaptureMode::List)),
    ],
    special: &["attribute"],
    ..FnRefSpec::EMPTY
};

/// Go (function-ref.ts:201 `GO_SPEC`). `keyed_element` has NO `value` field —
/// the value is the last named child (the first is the key). `expression_list`
/// fans out to ALL named children, which is what makes multi-assign
/// (`a, b = f, g`) capture both sides.
const GO_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("argument_list", rule(CaptureMode::Args)),
        (
            "assignment_statement",
            rule_field(CaptureMode::Rhs, "right"),
        ),
        (
            "short_var_declaration",
            rule_field(CaptureMode::Rhs, "right"),
        ),
        ("var_spec", rule_field(CaptureMode::VarInit, "value")),
        // value = last literal_element child
        ("keyed_element", rule(CaptureMode::Value)),
        // positional composite literals
        ("literal_value", rule(CaptureMode::List)),
    ],
    layers: &[("literal_element", None), ("expression_list", None)],
    ..FnRefSpec::EMPTY
};

/// Rust (function-ref.ts:217 `RUST_SPEC`).
const RUST_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("arguments", rule(CaptureMode::Args)),
        (
            "assignment_expression",
            rule_field(CaptureMode::Rhs, "right"),
        ),
        ("field_initializer", rule_field(CaptureMode::Value, "value")),
        ("array_expression", rule(CaptureMode::List)),
        ("static_item", rule_field(CaptureMode::VarInit, "value")),
        ("let_declaration", rule_field(CaptureMode::VarInit, "value")),
    ],
    ..FnRefSpec::EMPTY
};

/// Java (function-ref.ts:229 `JAVA_SPEC`). `id_types` is EMPTY — there are no
/// bare-identifier function values in Java, only method references. But
/// `method_reference` is the only WRAPPER form, NOT the only capture POSITION:
/// args, assignment RHS and variable declarators all capture (a
/// `method_reference` sitting in any of them).
const JAVA_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[
        ("argument_list", rule(CaptureMode::Args)),
        (
            "assignment_expression",
            rule_field(CaptureMode::Rhs, "right"),
        ),
        (
            "variable_declarator",
            rule_field(CaptureMode::VarInit, "value"),
        ),
    ],
    special: &["method_reference"],
    ..FnRefSpec::EMPTY
};

/// Kotlin (function-ref.ts:240 `KOTLIN_SPEC`). `assignment` has no RHS field
/// in the grammar — the value is the last named child.
///
/// **kotlin-ng drift** (the Task 11 protocol — kotlin-ng is a different lineage
/// than the WASM grammar the TS build used; see `normalize_special`): under
/// kotlin-ng only the RECEIVERLESS `::f` form is a `callable_reference`;
/// `this::m` AND `Type::m` both parse as `navigation_expression`.
const KOTLIN_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[
        ("value_arguments", rule(CaptureMode::Args)),
        // RHS = last named child (no field in the grammar)
        ("assignment", rule(CaptureMode::Rhs)),
    ],
    layers: &[("value_argument", None)],
    special: &["callable_reference", "navigation_expression"],
    ..FnRefSpec::EMPTY
};

/// C# (function-ref.ts:250 `CSHARP_SPEC`). `variable_declarator` has no value
/// field — it rides the no-field `varinit` path. `assignment_expression.right`
/// covers `+=` event subscription.
const CSHARP_SPEC: FnRefSpec = FnRefSpec {
    id_types: &["identifier"],
    dispatch: &[
        ("argument_list", rule(CaptureMode::Args)),
        // covers `+=` event subscription
        (
            "assignment_expression",
            rule_field(CaptureMode::Rhs, "right"),
        ),
        ("initializer_expression", rule(CaptureMode::List)),
        ("variable_declarator", rule(CaptureMode::VarInit)),
    ],
    layers: &[("argument", None)],
    special: &["member_access_expression"],
    ..FnRefSpec::EMPTY
};

/// Ruby (function-ref.ts:262 `RUBY_SPEC`). `id_types` is EMPTY: bare
/// identifiers in Ruby args are method CALLS or locals, never function values.
/// Only the `method(:name)` idiom (and `&method(:name)` via the
/// `block_argument` layer) plus the hook-DSL symbols qualify.
const RUBY_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[
        ("argument_list", rule(CaptureMode::Args)),
        ("pair", rule_field(CaptureMode::Value, "value")),
    ],
    layers: &[("block_argument", None)],
    special: &["call", "simple_symbol"],
    ..FnRefSpec::EMPTY
};

/// PHP (function-ref.ts:360 `PHP_SPEC`). `id_types` is EMPTY — PHP has no
/// bare-identifier function values (the first-class callable `fn(...)` already
/// extracts as a `calls` edge). What qualifies: a string argument to a known
/// callable-taking core function ([`PHP_CALLABLE_HOFS`]), and array callables
/// (`[$this, 'm']`, `[Foo::class, 'm']`) in ANY call's arguments.
const PHP_SPEC: FnRefSpec = FnRefSpec {
    id_types: &[],
    dispatch: &[("arguments", rule(CaptureMode::Args))],
    layers: &[("argument", None)],
    special: &["encapsed_string", "string", "array_creation_expression"],
    ..FnRefSpec::EMPTY
};

/// Capture specs by language (function-ref.ts:376 `FN_REF_SPECS`). `None` =
/// the language captures nothing — ObjC/Swift/Scala/Dart/Lua/Luau/Pascal are
/// wave 2 (their rows exist in the TS source; they land with their grammars).
pub(crate) fn fn_ref_spec(language: Language) -> Option<&'static FnRefSpec> {
    match language {
        Language::C => Some(&C_SPEC),
        Language::Cpp => Some(&CPP_SPEC),
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => {
            Some(&TS_JS_SPEC)
        }
        Language::Python => Some(&PYTHON_SPEC),
        Language::Go => Some(&GO_SPEC),
        Language::Rust => Some(&RUST_SPEC),
        Language::Java => Some(&JAVA_SPEC),
        Language::Kotlin => Some(&KOTLIN_SPEC),
        Language::CSharp => Some(&CSHARP_SPEC),
        Language::Ruby => Some(&RUBY_SPEC),
        Language::Php => Some(&PHP_SPEC),
        _ => None,
    }
}

/// Rails/ActiveSupport-style hook DSLs whose symbol arguments name a method of
/// the enclosing class: lifecycle callbacks (`before_action`, `after_save`,
/// `around_create`, `skip_before_action`…), `validate :method`, `set_callback`,
/// `helper_method`, and `rescue_from(..., with: :handler)`.
///
/// **NOT `validates`** (plural) — its symbols name ATTRIBUTES, not methods
/// (function-ref.ts:279-286). The exact-string set below is what keeps the two
/// apart: `validate` is a hook, `validates` is not, and the regex requires an
/// `_` after the before/after/around prefix.
static RUBY_HOOK_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `ruby_hook_call_detection`
    Regex::new(r"^(skip_)?(before|after|around)_[a-z_]+$").unwrap()
});
const RUBY_HOOK_NAMES: [&str; 4] = ["validate", "set_callback", "helper_method", "rescue_from"];

fn is_ruby_hook_call(name: &str) -> bool {
    RUBY_HOOK_RE.is_match(name) || RUBY_HOOK_NAMES.contains(&name)
}

/// PHP core functions whose string arguments are CALLABLES — the positional
/// prior that makes a bare string trustworthy as a function reference
/// (function-ref.ts:347, copied verbatim). Deliberately core-PHP only;
/// framework registries (WordPress `add_action`) belong in a `frameworks/`
/// resolver if ever added.
const PHP_CALLABLE_HOFS: [&str; 27] = [
    "array_map",
    "array_filter",
    "array_walk",
    "array_walk_recursive",
    "array_reduce",
    "usort",
    "uasort",
    "uksort",
    "array_udiff",
    "array_udiff_assoc",
    "array_uintersect",
    "array_uintersect_assoc",
    "call_user_func",
    "call_user_func_array",
    "forward_static_call",
    "forward_static_call_array",
    "preg_replace_callback",
    "preg_replace_callback_array",
    "register_shutdown_function",
    "register_tick_function",
    "set_error_handler",
    "set_exception_handler",
    "spl_autoload_register",
    "ob_start",
    "iterator_apply",
    "header_register_callback",
    "is_callable",
];

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// The last named child, or `None` when there are none. (The TS source spells
/// this `namedChild(namedChildCount - 1)`, which is `undefined` at count 0;
/// `named_child` takes a `u32` here, so the subtraction needs the guard.)
fn last_named_child<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let count = node.named_child_count();
    if count == 0 {
        return None;
    }
    node.named_child(u32::try_from(count - 1).ok()?)
}

/// One captured function-value candidate (function-ref.ts:36
/// `FnRefCandidate`), pre-gate.
#[derive(Debug, Clone)]
pub(crate) struct FnRefCandidate {
    pub(crate) name: String,
    /// 1-based line of the value expression.
    pub(crate) line: u32,
    /// 0-based column of the value expression.
    pub(crate) column: u32,
    /// Which capture position produced this candidate (gate policy keys on it).
    pub(crate) mode: CaptureMode,
    /// True when the value was an explicit reference form (`&fn`, `&Cls::m`)
    /// rather than a bare identifier — C++'s flush policy keys on it.
    pub(crate) explicit_ref: bool,
    /// Skip the same-file/import name gate for this candidate (PHP HOF-position
    /// string callables — Task 15b; the strong positional prior plus
    /// resolution's unique-or-drop rule carry the precision instead).
    pub(crate) skip_gate: bool,
}

/// Extract candidate names from a dispatched container node
/// (function-ref.ts:408 `captureFnRefCandidates`).
pub(crate) fn capture_fn_ref_candidates(
    container: Node<'_>,
    rule: CaptureRule,
    spec: &FnRefSpec,
    source: &str,
) -> Vec<FnRefCandidate> {
    let mut value_nodes: Vec<Node<'_>> = Vec::new();

    match rule.mode {
        CaptureMode::Args | CaptureMode::List => {
            let mut cursor = container.walk();
            value_nodes.extend(container.named_children(&mut cursor));
        }
        CaptureMode::Rhs => {
            let rhs = match rule.field {
                Some(f) => get_child_by_field(container, f),
                None => last_named_child(container),
            };
            if let Some(rhs) = rhs {
                // Param-storage skip (design doc rule 6): `this.status = status`
                // / `o->cb = cb` — when the assigned member's name EQUALS the
                // RHS identifier, the RHS is a local/parameter being stored and
                // the function it holds (if any) is unknowable statically. A
                // same-named function elsewhere would resolve to the WRONG
                // target (excalidraw A/B finding), so skip.
                let lhs = get_child_by_field(container, "left")
                    .or_else(|| get_child_by_field(container, "lhs"))
                    .or_else(|| get_child_by_field(container, "target"))
                    .or_else(|| {
                        (container.named_child_count() >= 2).then(|| container.named_child(0))?
                    });
                let lhs_text = lhs.map(|n| get_node_text(n, source)).unwrap_or("");
                let lhs_last_name = LHS_LAST_NAME
                    .captures(lhs_text)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str());
                let rhs_text = get_node_text(rhs, source).trim();
                if lhs_last_name != Some(rhs_text) {
                    value_nodes.push(rhs);
                }
            }
        }
        CaptureMode::Value => {
            let mut value = rule.field.and_then(|f| get_child_by_field(container, f));
            // Keyed containers without a value field (Go `keyed_element`): the
            // value is the LAST named child (the first is the key).
            if value.is_none() {
                value = last_named_child(container);
            }
            if let Some(v) = value {
                value_nodes.push(v);
            }
        }
        CaptureMode::VarInit => {
            // Destructuring (`const { center } = ellipse`) extracts DATA from
            // the RHS — never a function alias (design doc rule 7). Without
            // this skip, a parameter shadowing a same-named imported function
            // produced a wrong edge.
            let name_node = get_child_by_field(container, "name")
                .or_else(|| get_child_by_field(container, "pattern"));
            let destructured = name_node.is_some_and(|n| {
                matches!(
                    n.kind(),
                    "object_pattern" | "array_pattern" | "tuple_pattern" | "struct_pattern"
                )
            });
            if !destructured {
                match rule.field {
                    Some(f) => {
                        if let Some(v) = get_child_by_field(container, f) {
                            value_nodes.push(v);
                        }
                    }
                    None => {
                        // No value field in this grammar (C# `variable_declarator`,
                        // Dart `static_final_declaration`): the initializer is the
                        // last named child — but a declarator WITHOUT an
                        // initializer has its NAME there instead. Require ≥2 named
                        // children and never pick the name/pattern child.
                        if let Some(v) = last_named_child(container)
                            && container.named_child_count() >= 2
                            && name_node.is_none_or(|n| n.id() != v.id())
                        {
                            value_nodes.push(v);
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<FnRefCandidate> = Vec::new();
    for v in value_nodes {
        // A bare identifier is one that normalizes without passing through an
        // unwrap/special reference form. C++'s address_of_only policy (applied
        // at flush, where file scope is known) drops bare ids outside
        // file-scope initializer tables.
        let explicit_ref = !spec.is_id_type(v.kind());
        for nref in normalize_value(v, spec, source, 0) {
            if nref.name.is_empty() || NAME_STOPLIST.contains(&nref.name.as_str()) {
                continue;
            }
            out.push(FnRefCandidate {
                name: nref.name,
                line: u32::try_from(nref.node.start_position().row).unwrap_or(0) + 1,
                column: u32::try_from(nref.node.start_position().column).unwrap_or(0),
                mode: rule.mode,
                explicit_ref,
                skip_gate: nref.skip_gate,
            });
        }
    }
    out
}

/// One normalized function-value: its name, source node, and gate policy.
struct NormalizedRef<'t> {
    name: String,
    node: Node<'t>,
    skip_gate: bool,
}

impl<'t> NormalizedRef<'t> {
    fn new(name: String, node: Node<'t>) -> Self {
        NormalizedRef {
            name,
            node,
            skip_gate: false,
        }
    }
}

/// Normalize one value expression to zero or more function names
/// (function-ref.ts:525 `normalizeValue`). Recursion is bounded (wrapper
/// layers only); anything that isn't a recognized function-value shape yields
/// an empty vec.
fn normalize_value<'t>(
    node: Node<'t>,
    spec: &FnRefSpec,
    source: &str,
    depth: u32,
) -> Vec<NormalizedRef<'t>> {
    if depth > 4 {
        return Vec::new();
    }
    let node_type = node.kind();

    // Bare identifier.
    if spec.is_id_type(node_type) {
        return vec![NormalizedRef::new(
            get_node_text(node, source).to_string(),
            node,
        )];
    }

    // Transparent layers (argument, value_argument, literal_element,
    // expression_list, block_argument). `expression_list` fans out
    // (Go `a, b = f, g`).
    if let Some(layer_field) = spec.layer_for(node_type) {
        // Labeled-argument param-forward skip (Swift/Kotlin — Task 15b):
        // `value: value` / `delay: delay` — when the label EQUALS the value
        // identifier, the value is a forwarded local/parameter, not a function
        // reference (Alamofire A/B finding; same rationale as the
        // `this.x = x` assignment skip — design doc rule 6).
        if node_type == "value_argument" {
            let label = get_child_by_field(node, "name");
            let value = get_child_by_field(node, "value").or_else(|| last_named_child(node));
            if let (Some(l), Some(v)) = (label, value)
                && get_node_text(l, source).trim() == get_node_text(v, source).trim()
            {
                return Vec::new();
            }
        }
        return match layer_field {
            Some(field) => get_child_by_field(node, field)
                .map(|inner| normalize_value(inner, spec, source, depth + 1))
                .unwrap_or_default(),
            None => {
                let mut cursor = node.walk();
                let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                children
                    .into_iter()
                    .flat_map(|c| normalize_value(c, spec, source, depth + 1))
                    .collect()
            }
        };
    }

    // Unary wrappers: `&fn`.
    if let Some(unwrap_field) = spec.unwrap_for(node_type) {
        // C-family `pointer_expression` covers BOTH `&x` (address-of — a
        // function value) and `*x` (dereference — a data read, never a
        // function value). Only `&` qualifies; without this, fmt's `*begin`
        // reads resolved to its free `begin()` functions (design doc rule 4).
        if node_type == "pointer_expression" && node.child(0).map(|c| c.kind()) != Some("&") {
            return Vec::new();
        }
        let inner = match unwrap_field {
            Some(field) => get_child_by_field(node, field),
            None => node.named_child(0),
        };
        let Some(inner) = inner else {
            return Vec::new();
        };
        // C++ `&Widget::on_click` — keep the QUALIFIED name. Resolution scopes
        // the method to that class (more precise than a bare-name match, and
        // exempt from the cpp bare-ids-are-free-functions rule since
        // `&Cls::m` is an explicit member-pointer).
        if inner.kind() == "qualified_identifier" {
            let text = get_node_text(inner, source).trim();
            return if CPP_QUALIFIED_NAME.is_match(text) {
                vec![NormalizedRef::new(text.to_string(), inner)]
            } else {
                Vec::new()
            };
        }
        return normalize_value(inner, spec, source, depth + 1);
    }

    // Special whole-node reference forms.
    if spec.is_special(node_type) {
        return normalize_special(node, node_type, source);
    }

    Vec::new()
}

/// Whole-node reference forms needing bespoke name extraction
/// (function-ref.ts:612 `normalizeSpecial`).
fn normalize_special<'t>(node: Node<'t>, node_type: &str, source: &str) -> Vec<NormalizedRef<'t>> {
    match node_type {
        // Java method references (function-ref.ts:625). The RECEIVER decides
        // the resolution route (#808):
        //   `this::run0` / `super::close` → `this.<m>` (class-scoped resolver;
        //     `super` rides the inherited-member supertype pass)
        //   `Type::method` (CAPITALIZED) → qualified `Type::method`
        //     (suffix-matched against that type's members, cross-file capable)
        //   `variable::method` → NOTHING (the receiver's type is statically
        //     unknown — the deferred obj.method class; RxJava's baseline bare
        //     capture was resolving these to same-named same-file methods)
        "method_reference" => {
            let mut cursor = node.walk();
            let ids: Vec<Node<'t>> = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "identifier")
                .collect();
            let text = get_node_text(node, source);

            // `this::run0` / `super::close` — the receiver is a `this`/`super`
            // node, so the ONLY identifier child is the member.
            if text.starts_with("this::") || text.starts_with("super::") {
                let Some(member) = ids.last() else {
                    return Vec::new();
                };
                return vec![NormalizedRef::new(
                    format!("this.{}", get_node_text(*member, source)),
                    *member,
                )];
            }

            // `Type::method` needs BOTH a receiver and a member identifier.
            //
            // GRAMMAR NOTE: a constructor reference (`Type::new`) exposes only
            // the RECEIVER as an identifier — tree-sitter-java keeps `new` an
            // anonymous token. (The WASM grammar the TS build used surfaced it,
            // which is why function-ref.ts:640 can guard on `m === 'new'`.)
            // Without the arity check the receiver would be read as the member
            // and emit a bogus `M::M`. Either way the outcome is the TS one:
            // a constructor ref has no method node to land on, so no candidate.
            if ids.len() < 2 {
                return Vec::new();
            }
            let Some(member) = ids.last() else {
                return Vec::new();
            };
            let m = get_node_text(*member, source);
            if m == "new" {
                return Vec::new();
            }
            // `variable::method` → nothing (the receiver's type is statically
            // unknown — the deferred obj.method class).
            match capitalized_receiver(text) {
                Some(recv) => vec![NormalizedRef::new(format!("{recv}::{m}"), *member)],
                None => Vec::new(),
            }
        }

        // Kotlin `::targetCb` (function-ref.ts:646).
        //
        // kotlin-ng: the receiverless `::f` form is the ONLY shape that reaches
        // here — its single child is an `identifier` (the WASM grammar the TS
        // build used called it `simple_identifier`, and also routed
        // `Other::handle` through this node type; under kotlin-ng that is a
        // `navigation_expression`, handled below). A receiver-bearing form is
        // accepted defensively, with the same capitalized-receiver rule.
        "callable_reference" => {
            let mut cursor = node.walk();
            let ids: Vec<Node<'t>> = node
                .named_children(&mut cursor)
                .filter(|c| {
                    matches!(
                        c.kind(),
                        "identifier" | "simple_identifier" | "type_identifier"
                    )
                })
                .collect();
            match ids.as_slice() {
                // `::topLevelFn`
                [member] => vec![NormalizedRef::new(
                    get_node_text(*member, source).to_string(),
                    *member,
                )],
                // `Type::member` — a lowercase receiver is a VARIABLE (unknown
                // type), so it yields nothing.
                [receiver, member] => {
                    let recv = get_node_text(*receiver, source);
                    let m = get_node_text(*member, source);
                    if recv.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                        vec![NormalizedRef::new(format!("{recv}::{m}"), *member)]
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            }
        }

        // Kotlin `this::fire` (function-ref.ts:671).
        //
        // kotlin-ng: `this::fire` parses as `navigation_expression`
        // (`this_expression` + `identifier`) with NO `navigation_suffix` node,
        // and `Other::handle` parses here too (identifier + identifier) rather
        // than as a `callable_reference`. So this arm carries BOTH qualified
        // routes, keyed on the SEPARATOR: only `::` is a member reference —
        // ordinary `.` navigation (`a.b`, `obj.prop`) is a DATA read and must
        // yield nothing.
        "navigation_expression" => {
            let (Some(receiver), Some(member)) = (node.named_child(0), last_named_child(node))
            else {
                return Vec::new();
            };
            if receiver.id() == member.id() {
                return Vec::new();
            }
            // The separator sits between the two children; `::` is a member
            // reference, `.` is navigation.
            let sep = source
                .get(receiver.end_byte()..member.start_byte())
                .unwrap_or("");
            if !sep.contains("::") {
                return Vec::new();
            }
            let m = get_node_text(member, source);
            if receiver.kind() == "this_expression" || receiver.kind() == "super_expression" {
                return vec![NormalizedRef::new(format!("this.{m}"), member)];
            }
            let recv = get_node_text(receiver, source);
            // A lowercase receiver is a VARIABLE — its type is statically
            // unknown (`subscriber::onNext`), so no edge (design doc §Known
            // limits: "Java/Kotlin method refs through a VARIABLE").
            if recv.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && SIMPLE_NAME.is_match(recv)
            {
                vec![NormalizedRef::new(format!("{recv}::{m}"), member)]
            } else {
                Vec::new()
            }
        }

        // `this.Run0` (C#) — the receiver must be EXACTLY `this`
        // (function-ref.ts:738). Two grammar shapes: newer tree-sitter-c-sharp
        // exposes an `expression` field holding a `this_expression`; the
        // vendored grammar keeps `this` as an ANONYMOUS token (only the `name`
        // field is a named child), so fall back to the node text. The name is
        // BARE — C# method groups are real method values, so unlike TS/JS this
        // rides the normal gate (design doc rule 3).
        "member_access_expression" => {
            let Some(name) = get_child_by_field(node, "name") else {
                return Vec::new();
            };
            let is_this_receiver = match get_child_by_field(node, "expression") {
                Some(expr) => expr.kind() == "this_expression" || expr.kind() == "this",
                None => get_node_text(node, source).starts_with("this."),
            };
            if is_this_receiver {
                vec![NormalizedRef::new(
                    get_node_text(name, source).to_string(),
                    name,
                )]
            } else {
                Vec::new()
            }
        }

        // Ruby `method(:target_cb)` — a `call` whose method is literally
        // `method` with a single symbol argument (function-ref.ts:700).
        "call" => {
            let Some(method) = get_child_by_field(node, "method") else {
                return Vec::new();
            };
            if get_node_text(method, source) != "method" {
                return Vec::new();
            }
            let Some(args) = get_child_by_field(node, "arguments") else {
                return Vec::new();
            };
            if args.named_child_count() != 1 {
                return Vec::new();
            }
            let Some(sym) = args.named_child(0) else {
                return Vec::new();
            };
            if sym.kind() != "simple_symbol" {
                return Vec::new();
            }
            let name = get_node_text(sym, source).trim_start_matches(':');
            if name.is_empty() {
                return Vec::new();
            }
            vec![NormalizedRef::new(name.to_string(), sym)]
        }

        // Ruby hook-DSL symbols (`before_action :authenticate`,
        // `rescue_from E, with: :render_404`) — the symbol names a method of
        // the ENCLOSING class, so it routes through the class-scoped `this.`
        // resolver (which also walks superclasses, covering
        // ApplicationController-style inheritance). Symbols under ANY OTHER
        // call yield nothing — notably `validates` (plural), whose symbols are
        // ATTRIBUTES (function-ref.ts:797).
        "simple_symbol" => {
            let Some(call) = ruby_enclosing_call(node) else {
                return Vec::new();
            };
            let Some(method) = get_child_by_field(call, "method") else {
                return Vec::new();
            };
            if !is_ruby_hook_call(get_node_text(method, source)) {
                return Vec::new();
            }
            let sym = get_node_text(node, source).trim_start_matches(':');
            if !RUBY_SYMBOL_NAME.is_match(sym) {
                return Vec::new();
            }
            vec![NormalizedRef::new(format!("this.{sym}"), node)]
        }

        // PHP string callable — trustworthy ONLY as an argument to a known
        // callable-taking core function (`usort($a, 'cmp_items')`;
        // function-ref.ts:753). PHP global functions are referenced cross-file
        // WITHOUT imports, so these skip the name gate and lean on resolution's
        // unique-or-drop rule instead. A `'Cls::method'` string becomes a
        // qualified candidate.
        "encapsed_string" | "string" => {
            let Some(callee) = php_enclosing_call_name(node, source) else {
                return Vec::new();
            };
            if !PHP_CALLABLE_HOFS.contains(&callee) {
                return Vec::new();
            }
            let Some(content) = php_string_content(node, source) else {
                return Vec::new();
            };
            if PHP_PLAIN_CALLABLE.is_match(content) || PHP_QUALIFIED_CALLABLE.is_match(content) {
                vec![NormalizedRef {
                    name: content.to_string(),
                    node,
                    skip_gate: true,
                }]
            } else {
                Vec::new()
            }
        }

        // PHP array callables, valid in ANY call's arguments (the shape itself
        // is unambiguous; function-ref.ts:771): `[$this, 'method']` →
        // class-scoped `this.method`; `[Foo::class, 'method']` → qualified
        // `Foo::method`.
        "array_creation_expression" => {
            if node.named_child_count() != 2 {
                return Vec::new();
            }
            let recv = node.named_child(0).and_then(|e| e.named_child(0));
            let str_el = node.named_child(1).and_then(|e| e.named_child(0));
            let (Some(recv), Some(str_el)) = (recv, str_el) else {
                return Vec::new();
            };
            if str_el.kind() != "encapsed_string" && str_el.kind() != "string" {
                return Vec::new();
            }
            let Some(member) = php_string_content(str_el, source) else {
                return Vec::new();
            };
            if !PHP_PLAIN_CALLABLE.is_match(member) {
                return Vec::new();
            }
            if recv.kind() == "variable_name" && get_node_text(recv, source) == "$this" {
                return vec![NormalizedRef::new(format!("this.{member}"), str_el)];
            }
            if recv.kind() == "class_constant_access_expression" {
                let cls = recv.named_child(0);
                let kw = recv.named_child(1);
                if let (Some(cls), Some(kw)) = (cls, kw)
                    && get_node_text(kw, source) == "class"
                {
                    return vec![NormalizedRef::new(
                        format!("{}::{member}", get_node_text(cls, source)),
                        str_el,
                    )];
                }
            }
            Vec::new()
        }

        // `this.handleClick` (TS/JS) — the object must be EXACTLY `this`. The
        // name keeps the `this.` prefix so resolution can scope it to the
        // enclosing class instead of bare name-matching (design doc rule 3).
        "member_expression" => {
            let obj = get_child_by_field(node, "object");
            let prop = get_child_by_field(node, "property");
            match (obj, prop) {
                (Some(o), Some(p)) if o.kind() == "this" && p.kind() == "property_identifier" => {
                    vec![NormalizedRef::new(
                        format!("this.{}", get_node_text(p, source)),
                        p,
                    )]
                }
                _ => Vec::new(),
            }
        }

        // `self.handle_click` (Python) — the object must be EXACTLY `self`.
        "attribute" => {
            let obj = get_child_by_field(node, "object");
            let attr = get_child_by_field(node, "attribute");
            match (obj, attr) {
                (Some(o), Some(a))
                    if o.kind() == "identifier" && get_node_text(o, source) == "self" =>
                {
                    vec![NormalizedRef::new(get_node_text(a, source).to_string(), a)]
                }
                _ => Vec::new(),
            }
        }

        _ => Vec::new(),
    }
}

/// The CAPITALIZED receiver of a `Type::member` reference — a lowercase
/// receiver is a VARIABLE whose type is statically unknown, so it yields
/// nothing (function-ref.ts:637).
fn capitalized_receiver(text: &str) -> Option<&str> {
    let recv = text.split("::").next()?.trim();
    if recv.is_empty() || !recv.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    SIMPLE_NAME.is_match(recv).then_some(recv)
}

/// The content of a PHP string literal node (single- or double-quoted)
/// (function-ref.ts:813 `phpStringContent`).
fn php_string_content<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let mut cursor = node.walk();
    let content = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "string_content")?;
    Some(get_node_text(content, source).trim())
}

/// The function name of the PHP call whose arguments contain `node`, if any
/// (function-ref.ts:822 `phpEnclosingCallName`). Method calls are NOT core
/// HOFs, so they short-circuit to `None`.
fn php_enclosing_call_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let mut cur = node.parent();
    for _ in 0..4 {
        let n = cur?;
        if n.kind() == "function_call_expression" {
            let f = get_child_by_field(n, "function")?;
            return Some(get_node_text(f, source));
        }
        if n.kind() == "member_call_expression" || n.kind() == "scoped_call_expression" {
            return None; // method calls aren't core HOFs
        }
        cur = n.parent();
    }
    None
}

/// The Ruby `call` node whose argument_list (or keyword pair) contains `node`
/// (function-ref.ts:837 `rubyEnclosingCall`).
fn ruby_enclosing_call<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut cur = node.parent();
    for _ in 0..4 {
        let n = cur?;
        if n.kind() == "call" {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

// ---------------------------------------------------------------------------
// Regexes (compile-time literals — the house `unwrap` idiom, each exercised by
// a test below).
// ---------------------------------------------------------------------------

/// A Ruby method symbol — `?`/`!` suffixes are legal method names
/// (function-ref.ts:803).
static RUBY_SYMBOL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `ruby_hook_call_detection`
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_?!]*$").unwrap()
});

/// A PHP plain string callable (`'cmp_items'`) (function-ref.ts:759).
static PHP_PLAIN_CALLABLE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `php_callable_regexes`
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap()
});

/// A PHP qualified string callable (`'Cls::method'`) (function-ref.ts:762).
static PHP_QUALIFIED_CALLABLE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `php_callable_regexes`
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*::[A-Za-z_][A-Za-z0-9_]*$").unwrap()
});

/// The trailing simple name of an assignment LHS (`o->cb` → `cb`,
/// `this.status` → `status`) — the param-forward skip's comparison key
/// (function-ref.ts:441).
static LHS_LAST_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `lhs_last_name_regex`
    Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)\s*$").unwrap()
});

/// A C++ member-pointer target (`Widget::on_click`) — ASCII-explicit because
/// Rust's `\w` is Unicode-aware while the TS source's was not
/// (function-ref.ts:586).
static CPP_QUALIFIED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `cpp_qualified_name_regex`
    Regex::new(r"^[A-Za-z_][0-9A-Za-z_:]*$").unwrap()
});

/// A gate-eligible simple binding name (tree-sitter.ts:625 `SIMPLE_NAME`).
pub(crate) static SIMPLE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `import_name_regexes`
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").unwrap()
});

/// A dotted/backslashed import whose LAST segment is the simple name code
/// actually references — JVM `import com.example.OtherClass`, PHP
/// `use App\Services\Mailer` (tree-sitter.ts:629 `QUALIFIED_IMPORT`).
pub(crate) static QUALIFIED_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `import_name_regexes`
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$.\\]*[.\\]([A-Za-z_$][A-Za-z0-9_$]*)$").unwrap()
});

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn lhs_last_name_regex() {
        let last = |s: &str| {
            LHS_LAST_NAME
                .captures(s)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        };
        assert_eq!(last("o->cb"), Some("cb".to_string()));
        assert_eq!(last("this.status"), Some("status".to_string()));
        assert_eq!(last("obj.handlers.onClick "), Some("onClick".to_string()));
        assert_eq!(last("cb"), Some("cb".to_string()));
        // A subscript LHS has no trailing simple name.
        assert_eq!(last("table[0]"), None);
    }

    #[test]
    fn cpp_qualified_name_regex() {
        assert!(CPP_QUALIFIED_NAME.is_match("Widget::on_click"));
        assert!(CPP_QUALIFIED_NAME.is_match("ns::Widget::on_click"));
        assert!(CPP_QUALIFIED_NAME.is_match("plain"));
        assert!(!CPP_QUALIFIED_NAME.is_match("Widget::on_click(int)"));
        assert!(!CPP_QUALIFIED_NAME.is_match("1bad"));
    }

    #[test]
    fn import_name_regexes() {
        assert!(SIMPLE_NAME.is_match("assist"));
        assert!(SIMPLE_NAME.is_match("$jq"));
        assert!(!SIMPLE_NAME.is_match("com.example.Other"));

        let last = |s: &str| {
            QUALIFIED_IMPORT
                .captures(s)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        };
        assert_eq!(
            last("com.example.OtherClass"),
            Some("OtherClass".to_string())
        );
        assert_eq!(last(r"App\Services\Mailer"), Some("Mailer".to_string()));
        assert_eq!(last("plain"), None);
    }

    #[test]
    fn spec_registry_covers_every_v0_language() {
        for l in [
            Language::C,
            Language::Cpp,
            Language::Typescript,
            Language::Tsx,
            Language::Javascript,
            Language::Jsx,
            Language::Python,
            Language::Go,
            Language::Rust,
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Ruby,
            Language::Php,
        ] {
            assert!(fn_ref_spec(l).is_some(), "no fn-ref spec for {l:?}");
        }
        // Wave 2 — their TS rows exist but the grammars don't ship in v0.
        assert!(fn_ref_spec(Language::Swift).is_none());
        assert!(fn_ref_spec(Language::Objc).is_none());
    }

    #[test]
    fn languages_without_bare_identifier_function_values() {
        // Java/Kotlin/Ruby/PHP have EMPTY `id_types`: a bare identifier is a
        // call, a local or a param — never a function value.
        for l in [
            Language::Java,
            Language::Kotlin,
            Language::Ruby,
            Language::Php,
        ] {
            assert!(
                !fn_ref_spec(l).unwrap().is_id_type("identifier"),
                "{l:?} must not accept bare identifiers"
            );
        }
        // …while C/Go/Rust/C# do.
        for l in [Language::C, Language::Go, Language::Rust, Language::CSharp] {
            assert!(fn_ref_spec(l).unwrap().is_id_type("identifier"));
        }
    }

    #[test]
    fn ruby_hook_call_detection() {
        // Lifecycle callbacks + the named hook DSLs.
        for name in [
            "before_action",
            "after_save",
            "around_create",
            "skip_before_action",
            "validate",
            "set_callback",
            "helper_method",
            "rescue_from",
        ] {
            assert!(is_ruby_hook_call(name), "{name} should be a hook");
        }
        // `validates` (PLURAL) is EXCLUDED — its symbols are ATTRIBUTES.
        assert!(!is_ruby_hook_call("validates"));
        // Not hooks.
        for name in ["register", "before", "after", "each", "attr_accessor"] {
            assert!(!is_ruby_hook_call(name), "{name} should NOT be a hook");
        }
        // Symbol names may carry `?`/`!`.
        assert!(RUBY_SYMBOL_NAME.is_match("valid?"));
        assert!(RUBY_SYMBOL_NAME.is_match("save!"));
        assert!(!RUBY_SYMBOL_NAME.is_match("2bad"));
    }

    #[test]
    fn php_callable_regexes() {
        assert!(PHP_PLAIN_CALLABLE.is_match("cmp_items"));
        assert!(!PHP_PLAIN_CALLABLE.is_match("Cls::m"));
        assert!(PHP_QUALIFIED_CALLABLE.is_match("Cls::m"));
        assert!(!PHP_QUALIFIED_CALLABLE.is_match("just_a_string with spaces"));
        // The HOF list is the positional prior that makes a bare string
        // trustworthy — verbatim from function-ref.ts:347 (27 entries).
        assert_eq!(PHP_CALLABLE_HOFS.len(), 27);
        assert!(PHP_CALLABLE_HOFS.contains(&"usort"));
        assert!(PHP_CALLABLE_HOFS.contains(&"is_callable"));
        assert!(!PHP_CALLABLE_HOFS.contains(&"add_action")); // WordPress — deliberately not core
    }

    #[test]
    fn capitalized_receiver_gate() {
        assert_eq!(capitalized_receiver("Main::cb"), Some("Main"));
        // A lowercase receiver is a VARIABLE — unknown type, no edge.
        assert_eq!(capitalized_receiver("subscriber::onNext"), None);
        assert_eq!(capitalized_receiver("nocolons"), None);
    }

    #[test]
    fn cpp_is_address_of_only_but_c_is_not() {
        // Design doc rule 4 — the ONE difference between the two C-family rows.
        assert!(!fn_ref_spec(Language::C).unwrap().address_of_only);
        assert!(fn_ref_spec(Language::Cpp).unwrap().address_of_only);
    }

    #[test]
    fn c_family_ungates_only_initializer_modes() {
        let c = fn_ref_spec(Language::C).unwrap();
        assert!(c.is_ungated_mode(CaptureMode::Value));
        assert!(c.is_ungated_mode(CaptureMode::List));
        // `rhs`/`varinit` stay gated (design doc rule 2).
        assert!(!c.is_ungated_mode(CaptureMode::Rhs));
        assert!(!c.is_ungated_mode(CaptureMode::VarInit));
        assert!(!c.is_ungated_mode(CaptureMode::Args));
        // TS/JS ungate nothing.
        let ts = fn_ref_spec(Language::Typescript).unwrap();
        assert!(!ts.is_ungated_mode(CaptureMode::Value));
    }
}
