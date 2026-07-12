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
//! This module is the CAPTURE side only (Task 15a): the per-language spec
//! table + the pure "pull candidate names out of a dispatched container"
//! function. The walkers drive it ([`Session::maybe_capture_fn_refs`] and
//! [`scan_fn_ref_subtree`] in `walker/body.rs`) and the GATE
//! (`flush_fn_ref_candidates`, same file) decides which candidates become
//! `function_ref` [`crate::UnresolvedReference`]s. Resolution (unique-or-drop,
//! class-scoped `this.X`, overload refusal) is Phase 3.
//!
//! `function_ref` is an INTERNAL reference kind: resolution maps it to a
//! `references` edge (`metadata.fnRef = true`) — it never persists as an edge
//! kind (map §Wire).
//!
//! Task 15b adds the remaining v0 rows (Go, Rust, Java, Kotlin, C#, Ruby, PHP)
//! — spec rows plus their bespoke [`normalize_special`] arms.

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

/// Capture specs by language (function-ref.ts:376 `FN_REF_SPECS`). `None` =
/// the language captures nothing — Task 15b lands Go/Rust/Java/Kotlin/C#/
/// Ruby/PHP; ObjC/Swift/Scala/Dart/Lua/Pascal are wave 2.
pub(crate) fn fn_ref_spec(language: Language) -> Option<&'static FnRefSpec> {
    match language {
        Language::C => Some(&C_SPEC),
        Language::Cpp => Some(&CPP_SPEC),
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => {
            Some(&TS_JS_SPEC)
        }
        Language::Python => Some(&PYTHON_SPEC),
        _ => None,
    }
}

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
/// (function-ref.ts:612 `normalizeSpecial`). Task 15a covers TS/JS
/// `member_expression` and Python `attribute`; Task 15b adds
/// `method_reference` (Java), `callable_reference` / `navigation_expression`
/// (Kotlin), `member_access_expression` (C#), `call` / `simple_symbol` (Ruby),
/// and the PHP string/array callables.
fn normalize_special<'t>(node: Node<'t>, node_type: &str, source: &str) -> Vec<NormalizedRef<'t>> {
    match node_type {
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

// ---------------------------------------------------------------------------
// Regexes (compile-time literals — the house `unwrap` idiom, each exercised by
// a test below).
// ---------------------------------------------------------------------------

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
    fn spec_registry_covers_task_15a_languages_only() {
        assert!(fn_ref_spec(Language::C).is_some());
        assert!(fn_ref_spec(Language::Cpp).is_some());
        assert!(fn_ref_spec(Language::Typescript).is_some());
        assert!(fn_ref_spec(Language::Tsx).is_some());
        assert!(fn_ref_spec(Language::Javascript).is_some());
        assert!(fn_ref_spec(Language::Jsx).is_some());
        assert!(fn_ref_spec(Language::Python).is_some());
        // Task 15b.
        assert!(fn_ref_spec(Language::Go).is_none());
        assert!(fn_ref_spec(Language::Java).is_none());
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
