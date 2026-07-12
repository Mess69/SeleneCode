//! Java rules — verbatim port of `languages/java.ts` (map §11 java row):
//! package namespaces (`pkg::Class::method` qualified names),
//! `annotation_type_declaration` as interface, `static final` fields as
//! constants, and the **Lombok member synthesis** (#912) in
//! [`LanguageRules::synthesize_members`].
//!
//! Two TS-core pieces are hook-hosted in [`JavaRules::visit_node`] (the
//! walker is owned by the parallel core chain; observable output matches the
//! TS core placement):
//! - `field_declaration` extraction — the walker's generic declarator scan
//!   lacks the Java specifics: the `isConst` (static+final) **constant**-kind
//!   gate and the `"<Type> <name>"` signature. Field-annotation `decorates`
//!   refs and type-annotation refs stay with the core chain (Task 7) —
//!   flagged in the Task 10 report, not silently dropped.
//! - Anonymous classes (`new T() { … }` → `<T$anon@line>`) are NOT here:
//!   they are reached through function bodies, i.e. Task 6's body walker
//!   (tests ported and `#[ignore]`d).

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{NodeKind, Visibility};
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::{ImportInfo, LanguageRules, NodeTypeTables, scope_is_class_like};
use crate::walker::{NodeExtra, Session};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &[],
    class_types: &["class_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    // `annotation_type_declaration` is `@interface Foo { … }` — without it,
    // annotation types aren't nodes, so extracted `@Foo` usages can't
    // resolve and the annotation file shows zero dependents.
    interface_types: &["interface_declaration", "annotation_type_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_constant"],
    import_types: &["import_declaration"],
    call_types: &["method_invocation"],
    variable_types: &["local_variable_declaration"],
    field_types: &["field_declaration"],
    package_types: &["package_declaration"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("type"),
    ..NodeTypeTables::EMPTY
};

/// Return-`type` nodes that can never be a chained-call receiver.
const JAVA_NON_CLASS_RETURN_NODES: [&str; 4] = [
    "void_type",
    "integral_type",       // int, long, short, byte, char
    "floating_point_type", // float, double
    "boolean_type",
];

/// Lombok logging annotations — all generate a field named `log` by default.
const LOMBOK_LOG_ANNOTATIONS: [&str; 9] = [
    "Slf4j",
    "Log4j",
    "Log4j2",
    "Log",
    "CommonsLog",
    "JBossLog",
    "Flogger",
    "XSlf4j",
    "CustomLog",
];

static BARE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"^[A-Za-z_]\w*$").unwrap()
});
static GENERIC_ARGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"<[^>]*>").unwrap()
});
static STATIC_WORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"\bstatic\b").unwrap()
});
static FINAL_WORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"\bfinal\b").unwrap()
});
static IS_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"^is[A-Z]").unwrap()
});

/// Normalize a Java type node to the bare class name a chained
/// `foo.getThing().bar()` could be called on (#645/#608): primitives / void /
/// arrays yield `None`, `List<Foo>` unwraps to `List`, `java.util.List`
/// strips to the simple name.
fn normalize_java_type(type_node: Option<Node<'_>>, source: &str) -> Option<String> {
    let type_node = type_node?;
    if JAVA_NON_CLASS_RETURN_NODES.contains(&type_node.kind()) || type_node.kind() == "array_type" {
        return None;
    }
    let raw = get_node_text(type_node, source).trim();
    let raw = GENERIC_ARGS_RE.replace_all(raw, "");
    let last = raw.rsplit('.').next()?.trim();
    if last.is_empty() || !BARE_NAME_RE.is_match(last) {
        return None;
    }
    Some(last.to_string())
}

/// Text of a declaration's `modifiers` child (keyword modifiers are
/// anonymous, so matching happens on text).
fn modifier_text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers")
        .map(|m| get_node_text(m, source))
        .unwrap_or("")
}

/// Simple names of every annotation in a node's `modifiers` child
/// (`@lombok.Getter` → `Getter`).
fn lombok_annotation_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    let Some(modifiers) = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers")
    else {
        return names;
    };
    let mut c2 = modifiers.walk();
    for child in modifiers.named_children(&mut c2) {
        if matches!(child.kind(), "marker_annotation" | "annotation")
            && let Some(name_node) = get_child_by_field(child, "name")
        {
            let simple = get_node_text(name_node, source)
                .trim()
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_string();
            if !simple.is_empty() && !names.contains(&simple) {
                names.push(simple);
            }
        }
    }
    names
}

fn capitalize_java(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Lombok getter name: `getX`, or `isX` for a primitive boolean (an existing
/// `isFoo` field name is kept).
fn lombok_getter_name(field_name: &str, is_boolean_primitive: bool) -> String {
    if is_boolean_primitive {
        if IS_PREFIX_RE.is_match(field_name) {
            field_name.to_string()
        } else {
            format!("is{}", capitalize_java(field_name))
        }
    } else {
        format!("get{}", capitalize_java(field_name))
    }
}

/// Lombok setter name: `setX` (a primitive boolean `isFoo` sets via `setFoo`).
fn lombok_setter_name(field_name: &str, is_boolean_primitive: bool) -> String {
    let base = if is_boolean_primitive && IS_PREFIX_RE.is_match(field_name) {
        &field_name[2..]
    } else {
        field_name
    };
    format!("set{}", capitalize_java(base))
}

pub(crate) struct JavaRules;

impl LanguageRules for JavaRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// The `type` field (constructors have none → `None`), normalized.
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        normalize_java_type(get_child_by_field(node, "type"), source)
    }

    /// `ReturnType (params)` or `(params)` (the TS spelling).
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let params_text = get_node_text(params, source);
        Some(match get_child_by_field(node, "type") {
            Some(rt) => format!("{} {}", get_node_text(rt, source), params_text),
            None => params_text.to_string(),
        })
    }

    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let text = modifier_text(node, source);
        if text.contains("public") {
            Some(Visibility::Public)
        } else if text.contains("private") {
            Some(Visibility::Private)
        } else if text.contains("protected") {
            Some(Visibility::Protected)
        } else {
            None
        }
    }

    fn is_static(&self, node: Node<'_>, source: &str) -> Option<bool> {
        Some(modifier_text(node, source).contains("static"))
    }

    /// A `static final` field is a Java constant (`MAX_ITEMS`, lookup
    /// tables); instance / `final`-only / `static`-only fields stay mutable
    /// fields.
    fn is_const(&self, node: Node<'_>, source: &str) -> Option<bool> {
        let text = modifier_text(node, source);
        Some(STATIC_WORD_RE.is_match(text) && FINAL_WORD_RE.is_match(text))
    }

    /// `import a.b.C;` → the scoped_identifier text (wildcards keep the
    /// prefix; the `.*`/`static` live in the signature).
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let mut cursor = node.walk();
        let scoped = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "scoped_identifier")?;
        Some(ImportInfo {
            module_name: get_node_text(scoped, source).to_string(),
            signature: get_node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }

    /// `package_declaration` → `scoped_identifier` or single-segment
    /// `identifier`.
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        let id = node
            .named_children(&mut cursor)
            .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"))?;
        Some(get_node_text(id, source).trim().to_string())
    }

    /// Java `field_declaration` (hook-hosted TS-core shape — module docs):
    /// per declarator, kind `constant` when static+final else `field`,
    /// signature `"<Type> <name>"`.
    fn visit_node(&self, node: Node<'_>, session: &mut Session<'_>) -> bool {
        if node.kind() != "field_declaration" || !scope_is_class_like(session) {
            return false;
        }
        extract_java_fields(self, node, session);
        true
    }

    /// Lombok member synthesis (#912) — see [`synthesize_lombok_members`].
    fn synthesize_members(&self, class_node: Node<'_>, session: &mut Session<'_>) {
        synthesize_lombok_members(class_node, session);
    }
}

/// The TS-core Java field shape: type text = first named child that isn't a
/// modifier/annotation/declarator; one node per `variable_declarator` with
/// `"<Type> <name>"` signature, anchored at the declarator.
fn extract_java_fields(rules: &JavaRules, node: Node<'_>, s: &mut Session<'_>) {
    let docstring = get_preceding_docstring(node, s.source());
    let visibility = rules.get_visibility(node, s.source());
    let is_static = rules.is_static(node, s.source());
    let kind = if rules.is_const(node, s.source()).unwrap_or(false) {
        NodeKind::Constant
    } else {
        NodeKind::Field
    };

    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let type_node = children.iter().find(|c| {
        !matches!(
            c.kind(),
            "modifiers"
                | "modifier"
                | "variable_declarator"
                | "variable_declaration"
                | "marker_annotation"
                | "annotation"
        )
    });
    let type_text = type_node.map(|t| get_node_text(*t, s.source()).to_string());

    for decl in children
        .iter()
        .filter(|c| c.kind() == "variable_declarator")
    {
        let name_node = get_child_by_field(*decl, "name").or_else(|| {
            let mut c = decl.walk();
            decl.named_children(&mut c)
                .find(|n| n.kind() == "identifier")
        });
        let Some(name_node) = name_node else { continue };
        let name = get_node_text(name_node, s.source()).to_string();
        let signature = match &type_text {
            Some(t) => format!("{t} {name}"),
            None => name.clone(),
        };
        s.create_node(
            &JavaRules,
            kind,
            &name,
            *decl,
            NodeExtra {
                docstring: docstring.clone(),
                signature: Some(signature),
                visibility,
                is_static,
                ..NodeExtra::default()
            },
        );
        // Field-annotation `decorates` refs + type-annotation refs stay with
        // the core chain (Task 7) — flagged in the Task 10 report.
    }
}

/// Synthesize the members Lombok generates at compile time (#912):
///
/// - `@Getter`/`@Setter` (class- or field-level) → `getX()`/`isX()`, `setX()`
/// - `@Data` → getters + setters (non-final) + equals/hashCode/toString
/// - `@Value` → getters + equals/hashCode/toString (immutable, no setters)
/// - `@Builder`/`@SuperBuilder` → static `builder()` returning `<Class>Builder`
/// - `@ToString`/`@EqualsAndHashCode` → those methods
/// - `@Slf4j` + the other log annotations → the `log` field
///
/// Every synthesized member: `visibility: public`, `decorators: ["lombok"]`,
/// `docstring: "Lombok-generated (@Ann)"`, anchored on the field's (or
/// class's) name token. Static fields skipped; setters skip `final`; a
/// member the source already declares is NEVER overridden (taken sets keyed
/// `classQN::name`, scanned from the session's nodes). Deliberately not
/// synthesized (TS parity): constructors, fluent builder setters,
/// `@Accessors` naming.
fn synthesize_lombok_members(class_node: Node<'_>, s: &mut Session<'_>) {
    // Pass 1 (immutable session reads): decide every member to synthesize.
    // Pass 2 (mutable): emit them. Split keeps the borrow checker happy —
    // `Session::source()` borrows the session, and `create_node` needs it
    // mutably; anchors are plain tree-sitter `Node`s (independent lifetimes).
    struct Planned<'t> {
        kind: NodeKind,
        name: String,
        anchor: Node<'t>,
        signature: String,
        docstring: String,
        visibility: Visibility,
        is_static: Option<bool>,
        return_type: Option<String>,
    }
    let mut plan: Vec<Planned<'_>> = Vec::new();

    #[allow(clippy::too_many_arguments)] // mirrors the TS emitMethod closure
    fn plan_method<'t>(
        name: String,
        anchor: Node<'t>,
        signature: String,
        from: &str,
        is_static: Option<bool>,
        return_type: Option<String>,
        taken: &mut Vec<String>,
        plan: &mut Vec<Planned<'t>>,
    ) {
        if name.is_empty() || taken.contains(&name) {
            return;
        }
        taken.push(name.clone());
        plan.push(Planned {
            kind: NodeKind::Method,
            name,
            anchor,
            signature,
            docstring: format!("Lombok-generated ({from})"),
            visibility: Visibility::Public,
            is_static,
            return_type,
        });
    }

    {
        let source = s.source();
        let class_anns = lombok_annotation_names(class_node, source);
        let has = |a: &str| class_anns.iter().any(|x| x == a);
        let class_getter = has("Getter");
        let class_setter = has("Setter");
        let is_data = has("Data");
        let is_value = has("Value");
        let has_builder = has("Builder") || has("SuperBuilder");
        let has_to_string = is_data || is_value || has("ToString");
        let has_equals = is_data || is_value || has("EqualsAndHashCode");
        let log_ann = class_anns
            .iter()
            .find(|a| LOMBOK_LOG_ANNOTATIONS.contains(&a.as_str()))
            .cloned();

        let Some(body) = get_child_by_field(class_node, "body") else {
            return;
        };
        let mut cursor = body.walk();
        let fields: Vec<Node<'_>> = body
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "field_declaration")
            .collect();

        // Leave immediately when nothing Lombok is present — a non-Lombok
        // class pays one scan of its direct field declarations at most.
        let class_has_lombok = class_getter
            || class_setter
            || is_data
            || is_value
            || has_builder
            || has_to_string
            || has_equals
            || log_ann.is_some();
        if !class_has_lombok
            && !fields
                .iter()
                .any(|f| !lombok_annotation_names(*f, source).is_empty())
        {
            return;
        }

        // Members already declared in this class — Lombok never overrides an
        // explicit member. Methods and fields tracked separately (distinct
        // namespaces in Java: a boolean field `isRunning` coexists with its
        // generated getter `isRunning()`).
        let class_id = s.node_stack().last().cloned().unwrap_or_default();
        let class_rec = s.nodes().iter().find(|n| n.id == class_id);
        let class_qn = class_rec.map(|n| n.qualified_name.clone());
        let mut taken_methods: Vec<String> = Vec::new();
        let mut taken_fields: Vec<String> = Vec::new();
        if let Some(qn) = &class_qn {
            for n in s.nodes() {
                if n.file_path == s.file_path() && n.qualified_name == format!("{qn}::{}", n.name) {
                    match n.kind {
                        NodeKind::Method | NodeKind::Function => {
                            taken_methods.push(n.name.clone());
                        }
                        NodeKind::Field
                        | NodeKind::Variable
                        | NodeKind::Constant
                        | NodeKind::Property => taken_fields.push(n.name.clone()),
                        _ => {}
                    }
                }
            }
        }

        let class_name_node = get_child_by_field(class_node, "name").unwrap_or(class_node);
        let class_name = class_rec
            .map(|n| n.name.clone())
            .unwrap_or_else(|| get_node_text(class_name_node, source).trim().to_string());

        // Per-field getters/setters.
        for fd in &fields {
            let mods = modifier_text(*fd, source);
            if STATIC_WORD_RE.is_match(mods) {
                continue; // Lombok skips static fields.
            }
            let is_final = FINAL_WORD_RE.is_match(mods);
            let field_anns = lombok_annotation_names(*fd, source);
            let field_getter = field_anns.iter().any(|a| a == "Getter");
            let field_setter = field_anns.iter().any(|a| a == "Setter");

            let want_getter = class_getter || is_data || is_value || field_getter;
            let want_setter = (class_setter || is_data || field_setter) && !is_final;
            if !want_getter && !want_setter {
                continue;
            }

            let type_node = get_child_by_field(*fd, "type");
            let type_text = type_node
                .map(|t| get_node_text(t, source).trim().to_string())
                .unwrap_or_else(|| "Object".to_string());
            let is_boolean_primitive = type_node.is_some_and(|t| t.kind() == "boolean_type");
            let return_type = normalize_java_type(type_node, source);

            let mut c2 = fd.walk();
            let declarators: Vec<Node<'_>> = fd
                .named_children(&mut c2)
                .filter(|c| c.kind() == "variable_declarator")
                .collect();
            for vd in declarators {
                let Some(name_node) = get_child_by_field(vd, "name") else {
                    continue;
                };
                let field_name = get_node_text(name_node, source).trim().to_string();
                if field_name.is_empty() {
                    continue;
                }

                if want_getter {
                    let g = lombok_getter_name(&field_name, is_boolean_primitive);
                    let from = if field_getter {
                        "@Getter"
                    } else if is_data {
                        "@Data"
                    } else if is_value {
                        "@Value"
                    } else {
                        "@Getter"
                    };
                    plan_method(
                        g.clone(),
                        name_node,
                        format!("{type_text} {g}()"),
                        from,
                        None,
                        return_type.clone(),
                        &mut taken_methods,
                        &mut plan,
                    );
                }
                if want_setter {
                    let st = lombok_setter_name(&field_name, is_boolean_primitive);
                    let from = if field_setter {
                        "@Setter"
                    } else if is_data {
                        "@Data"
                    } else {
                        "@Setter"
                    };
                    plan_method(
                        st.clone(),
                        name_node,
                        format!("void {st}({type_text} {field_name})"),
                        from,
                        None,
                        None,
                        &mut taken_methods,
                        &mut plan,
                    );
                }
            }
        }

        // Class-level synthesized methods.
        if has_builder {
            let from = if has("SuperBuilder") {
                "@SuperBuilder"
            } else {
                "@Builder"
            };
            plan_method(
                "builder".to_string(),
                class_name_node,
                format!("static {class_name}.{class_name}Builder builder()"),
                from,
                Some(true),
                Some(format!("{class_name}Builder")),
                &mut taken_methods,
                &mut plan,
            );
        }
        if has_to_string {
            let from = if is_data {
                "@Data"
            } else if is_value {
                "@Value"
            } else {
                "@ToString"
            };
            plan_method(
                "toString".to_string(),
                class_name_node,
                "String toString()".to_string(),
                from,
                None,
                None,
                &mut taken_methods,
                &mut plan,
            );
        }
        if has_equals {
            let from = if is_data {
                "@Data"
            } else if is_value {
                "@Value"
            } else {
                "@EqualsAndHashCode"
            };
            plan_method(
                "equals".to_string(),
                class_name_node,
                "boolean equals(Object o)".to_string(),
                from,
                None,
                None,
                &mut taken_methods,
                &mut plan,
            );
            plan_method(
                "hashCode".to_string(),
                class_name_node,
                "int hashCode()".to_string(),
                from,
                None,
                None,
                &mut taken_methods,
                &mut plan,
            );
        }

        // Logger field (@Slf4j and friends).
        if let Some(log_ann) = log_ann
            && !taken_fields.iter().any(|f| f == "log")
        {
            plan.push(Planned {
                kind: NodeKind::Field,
                name: "log".to_string(),
                anchor: class_name_node,
                signature: "Logger log".to_string(),
                docstring: format!("Lombok-generated (@{log_ann})"),
                visibility: Visibility::Private,
                is_static: Some(true),
                return_type: None,
            });
        }
    }

    // Pass 2: emit.
    for m in plan {
        s.create_node(
            &JavaRules,
            m.kind,
            &m.name,
            m.anchor,
            NodeExtra {
                visibility: Some(m.visibility),
                signature: Some(m.signature),
                docstring: Some(m.docstring),
                decorators: vec!["lombok".to_string()],
                is_static: m.is_static,
                return_type: m.return_type,
                ..NodeExtra::default()
            },
        );
    }
}
