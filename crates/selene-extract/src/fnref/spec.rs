//! The capture-spec model ([`CaptureMode`], [`CaptureRule`], [`FnRefSpec`]) and
//! the per-language spec table ([`fn_ref_spec`]) with its spec-side data
//! (the name stoplist, the Ruby hook DSLs, the PHP callable HOFs).

use std::sync::LazyLock;

use regex::Regex;

use crate::Language;

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

    pub(super) fn is_id_type(&self, node_type: &str) -> bool {
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
    pub(super) fn layer_for(&self, node_type: &str) -> Option<Option<&'static str>> {
        self.layers
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, f)| *f)
    }
    pub(super) fn unwrap_for(&self, node_type: &str) -> Option<Option<&'static str>> {
        self.unwrap
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, f)| *f)
    }
    pub(super) fn is_special(&self, node_type: &str) -> bool {
        self.special.contains(&node_type)
    }
    pub(crate) fn is_ungated_mode(&self, mode: CaptureMode) -> bool {
        self.ungated_modes.contains(&mode)
    }
}

/// Names that are never function references even when grammars call them
/// identifiers (function-ref.ts:121 `NAME_STOPLIST`).
pub(super) const NAME_STOPLIST: [&str; 12] = [
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

pub(super) fn is_ruby_hook_call(name: &str) -> bool {
    RUBY_HOOK_RE.is_match(name) || RUBY_HOOK_NAMES.contains(&name)
}

/// PHP core functions whose string arguments are CALLABLES — the positional
/// prior that makes a bare string trustworthy as a function reference
/// (function-ref.ts:347, copied verbatim). Deliberately core-PHP only;
/// framework registries (WordPress `add_action`) belong in a `frameworks/`
/// resolver if ever added.
pub(super) const PHP_CALLABLE_HOFS: [&str; 27] = [
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
