//! The capture entry point: extract pre-gate [`FnRefCandidate`]s from a
//! dispatched container node.

use tree_sitter::Node;

use super::normalize::normalize_value;
use super::regexes::LHS_LAST_NAME;
use super::spec::{CaptureMode, CaptureRule, FnRefSpec, NAME_STOPLIST};
use crate::helpers::{get_child_by_field, get_node_text};

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// The last named child, or `None` when there are none. (The TS source spells
/// this `namedChild(namedChildCount - 1)`, which is `undefined` at count 0;
/// `named_child` takes a `u32` here, so the subtraction needs the guard.)
pub(super) fn last_named_child<'t>(node: Node<'t>) -> Option<Node<'t>> {
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
