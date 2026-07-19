//! Normalization: one value expression → zero or more function names
//! ([`normalize_value`], the special whole-node forms, and their AST helpers).

use tree_sitter::Node;

use super::capture::last_named_child;
use super::regexes::{
    CPP_QUALIFIED_NAME, PHP_PLAIN_CALLABLE, PHP_QUALIFIED_CALLABLE, RUBY_SYMBOL_NAME, SIMPLE_NAME,
};
use super::spec::{FnRefSpec, PHP_CALLABLE_HOFS, is_ruby_hook_call};
use crate::helpers::{get_child_by_field, get_node_text};

/// One normalized function-value: its name, source node, and gate policy.
pub(super) struct NormalizedRef<'t> {
    pub(super) name: String,
    pub(super) node: Node<'t>,
    pub(super) skip_gate: bool,
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
pub(super) fn normalize_value<'t>(
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
            // `Foo::class` is a CLASS LITERAL, not a member reference — and
            // kotlin-ng parses the `class` keyword as an `identifier`, so it
            // arrives here looking EXACTLY like `Foo::method` (capitalized
            // receiver, `::` separator). It names no method, so reject it on
            // the member side. `class` is a hard keyword: it can never be a
            // Kotlin method name, so this can't shadow a real target.
            if m == "class" {
                return Vec::new();
            }
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
pub(super) fn capitalized_receiver(text: &str) -> Option<&str> {
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
