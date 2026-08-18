//! The extraction entry point ([`extract_from_source`]), the name/body
//! resolution helpers, and the [`visit`] dispatch ladder.

use selene_core::{Node as CoreNode, NodeKind, file_node_id};
use tree_sitter::{Node, Parser};

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::{ClassKind, LanguageRules, MethodClass, rules_for};
use crate::{
    ErrorCode, ExtractionError, ExtractionResult, Language, Severity, grammars::grammar_for,
    is_file_level_only,
};

use super::{
    NodeExtra, Session, body, extract_class, extract_enum, extract_field, extract_function,
    extract_import, extract_interface, extract_method, extract_property, extract_struct,
    extract_type_alias, extract_variable, ts_core,
};

/// One extraction pass over `source` (the `extractFromSource` port —
/// declarations subset, Task 5). Errors are collected, never thrown; a
/// language without v0 rules yields an `unsupported_language` result
/// (Warning for known wave-2 languages, Error for [`Language::Unknown`] —
/// the TS severity split).
pub fn extract_from_source(file_path: &str, source: &str, language: Language) -> ExtractionResult {
    let started = std::time::Instant::now();
    let mut result = ExtractionResult::default();

    // Documents (md/txt/rst): the doc-parser branch — Document/Section nodes,
    // outward pointers as unresolved refs. A branch BESIDE the walker, never
    // inside it (doc-ingestion PRD §5.3).
    if crate::docparse::is_document(language) {
        return crate::docparse::extract_document(file_path, source, language);
    }

    // File-level-only languages: indexed as files, no symbol extraction.
    if is_file_level_only(language) {
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    }

    let (Some(rules), Some(grammar)) = (rules_for(language), grammar_for(language)) else {
        result.errors.push(ExtractionError {
            message: format!("Unsupported language: {}", language.as_str()),
            severity: if language == Language::Unknown {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: ErrorCode::UnsupportedLanguage,
            file_path: Some(file_path.to_string()),
        });
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    };

    // Optional byte-offset-preserving pre-parse transform; downstream text
    // reads use the same bytes the parser saw.
    let transformed = rules.pre_parse(source, file_path);
    let source: &str = transformed.as_deref().unwrap_or(source);

    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        result.errors.push(ExtractionError {
            message: format!("Failed to build parser for language: {}", language.as_str()),
            severity: Severity::Error,
            code: ErrorCode::ParserError,
            file_path: Some(file_path.to_string()),
        });
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    }
    let Some(tree) = parser.parse(source, None) else {
        result.errors.push(ExtractionError {
            message: "Parse error: parser returned no tree".to_string(),
            severity: Severity::Error,
            code: ErrorCode::ParseError,
            file_path: Some(file_path.to_string()),
        });
        result.duration_ms = started.elapsed().as_millis() as u64;
        return result;
    };

    let mut s = Session::new(file_path, source, language, rules);

    // File node: unhashed literal id, name = basename, qualifiedName = the
    // file path (the one deliberate path-valued qualifiedName), endLine =
    // line count (split('\n') semantics: trailing newline adds a line).
    let basename = file_path.rsplit('/').next().unwrap_or(file_path);
    let file_node = CoreNode {
        id: file_node_id(file_path),
        kind: NodeKind::File,
        name: basename.to_string(),
        qualified_name: file_path.to_string(),
        file_path: file_path.to_string(),
        language,
        start_line: 1,
        end_line: u32::try_from(source.split('\n').count()).unwrap_or(u32::MAX),
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: Some(false),
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        route_method: None,
        route_path: None,
        framework: None,
        updated_at: s.updated_at,
    };
    s.id_index
        .insert(file_node.id.clone(), (NodeKind::File, basename.to_string()));
    s.node_stack.push(file_node.id.clone());
    s.nodes.push(file_node);

    // Package header (Java/Kotlin/Erlang) → implicit `namespace` node
    // wrapping every top-level declaration.
    let package_idx = extract_file_package(rules, &mut s, tree.root_node());
    if let Some(idx) = package_idx
        && let Some(id) = s.id_of(idx)
    {
        s.node_stack.push(id);
    }

    visit(rules, &mut s, tree.root_node());

    // Gate + flush the function-as-value candidates (#756) while the file's
    // nodes and import refs are complete and the file node is still pushed.
    body::flush_fn_ref_candidates(&mut s);
    body::flush_value_refs(&mut s, &tree);

    if package_idx.is_some() {
        s.node_stack.pop();
    }
    s.node_stack.pop();

    result.nodes = s.nodes;
    result.edges = s.edges;
    result.unresolved = s.unresolved;
    result.errors = s.errors;
    result.duration_ms = started.elapsed().as_millis() as u64;
    result
}

/// `extractFilePackage`: first `package_types` child under the root → a
/// `namespace` node; caller scopes top-level declarations underneath.
fn extract_file_package(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    root: Node<'_>,
) -> Option<usize> {
    let types = rules.tables().package_types;
    if types.is_empty() {
        return None;
    }
    let mut cursor = root.walk();
    let pkg = root
        .named_children(&mut cursor)
        .find(|c| types.contains(&c.kind()))?;
    let name = rules.extract_package(pkg, s.source())?;
    s.create_node(NodeKind::Namespace, &name, pkg, NodeExtra::default())
}

/// `extractName`: resolve_name hook, else the `name_field` child (C/C++
/// declarator unwrapping arrives with Task 13), else `<anonymous>`; passed
/// through `recover_mangled_name` (identity by default).
pub(super) fn extract_name(
    rules: &'static dyn LanguageRules,
    node: Node<'_>,
    source: &str,
) -> String {
    let raw = rules
        .resolve_name(node, source)
        .or_else(|| {
            let name_node = get_child_by_field(node, rules.tables().name_field)?;
            Some(resolve_declarator_name(name_node, source))
        })
        .or_else(|| {
            // Arrow/function expressions never name themselves from body
            // identifiers — the parent declarator names them (or nothing).
            if node.kind() == "arrow_function" || node.kind() == "function_expression" {
                return None;
            }
            // Fall back to the first identifier-like child (how a C
            // `struct point` gets its name despite name_field being
            // `declarator` — struct_specifier has no such field).
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| {
                    matches!(
                        c.kind(),
                        "identifier" | "type_identifier" | "simple_identifier" | "constant"
                    )
                })
                .map(|c| get_node_text(c, source).to_string())
        })
        .unwrap_or_else(|| "<anonymous>".to_string());
    rules.recover_mangled_name(raw)
}

/// The C/C++ declarator-unwrapping tail of `extractNameRaw` (Task 13):
/// pointer/reference declarators unwrap to their inner (`int* f()` names
/// `f`, not `* f(...)`); a user-defined conversion operator names
/// `operator <type>`; a `function_declarator`/`declarator` yields its inner
/// declarator (the identifier). Inert for grammars whose name field is
/// already an identifier. (Lua dot/method index shapes — wave 2.)
fn resolve_declarator_name(name_node: Node<'_>, source: &str) -> String {
    let mut resolved = name_node;
    while resolved.kind() == "pointer_declarator" || resolved.kind() == "reference_declarator" {
        let inner = get_child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0));
        match inner {
            Some(i) => resolved = i,
            None => break,
        }
    }
    if resolved.kind() == "operator_cast" {
        return match resolved.named_child(0) {
            Some(type_node) => format!("operator {}", get_node_text(type_node, source).trim()),
            None => get_node_text(resolved, source).to_string(),
        };
    }
    if resolved.kind() == "function_declarator" || resolved.kind() == "declarator" {
        let inner = get_child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0));
        return match inner {
            Some(i) => get_node_text(i, source).to_string(),
            None => get_node_text(resolved, source).to_string(),
        };
    }
    get_node_text(resolved, source).to_string()
}

/// Class/module-scope `CONST = …`: an `assignment` whose LHS is a
/// `constant` node (the TS `isClassScopeConstantAssignment` — see the
/// variable branch of the dispatch ladder).
fn is_class_scope_constant_assignment(node: Node<'_>) -> bool {
    if node.kind() != "assignment" {
        return false;
    }
    get_child_by_field(node, "left")
        .or_else(|| node.named_child(0))
        .is_some_and(|left| left.kind() == "constant")
}

/// `resolveBody` hook, else the `body_field` child.
pub(super) fn resolve_body<'t>(
    rules: &'static dyn LanguageRules,
    node: Node<'t>,
) -> Option<Node<'t>> {
    rules
        .resolve_body(node, rules.tables().body_field)
        .or_else(|| get_child_by_field(node, rules.tables().body_field))
}

/// The dispatch ladder (map §8, exact order; first match wins; matched
/// branches handle their own descent).
pub(super) fn visit(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let t = rules.tables();
    let node_type = node.kind();

    // 1. Custom visit_node hook — short-circuits the whole ladder.
    if rules.visit_node(node, s) {
        // The hook consumed this subtree, so the walkers below never descend
        // into it — scan it for function-as-value candidates (#756). The scan
        // is capture-only and halts at nested function boundaries.
        body::scan_fn_ref_subtree(s, node, 0);
        return;
    }

    // C++ namespace blocks (Task 13): carry the namespace name as a
    // qualifiedName prefix while walking the body — NO node is minted, so
    // `namespace flash { void compute(); }` indexes `flash::compute` and a
    // namespace-qualified call resolves by exact qualified match (#387).
    // C++17 nested forms (`namespace a::b {`) prefix as written; an
    // ANONYMOUS namespace falls through to the generic walk — its contents
    // stay bare, matching how call sites spell them.
    if s.language() == Language::Cpp && node_type == "namespace_definition" {
        let ns_name = get_child_by_field(node, "name")
            .map(|n| get_node_text(n, s.source()).to_string())
            .unwrap_or_default();
        if !ns_name.is_empty() {
            s.namespace_prefix.push(ns_name);
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            for child in children {
                visit(rules, s, child);
            }
            s.namespace_prefix.pop();
            return;
        }
    }

    // Function-as-value capture (#756, Task 15a) — deliberately INDEPENDENT of
    // the dispatch ladder below (the captured container types have no other
    // handler there), so it can never shadow or be shadowed by an extraction
    // branch. Subtrees a matched branch consumes without descending get
    // `scan_fn_ref_subtree` instead (below).
    s.maybe_capture_fn_refs(node, node_type);

    let mut matched = true;
    if t.function_types.contains(&node_type) {
        if s.is_inside_class_like() && t.method_types.contains(&node_type) {
            extract_method(rules, s, node);
        } else {
            extract_function(rules, s, node);
        }
    } else if t.class_types.contains(&node_type) {
        match rules.classify_class_node(node, s.source()) {
            Some(ClassKind::Struct) => extract_struct(rules, s, node),
            Some(ClassKind::Enum) => extract_enum(rules, s, node),
            Some(ClassKind::Interface) => extract_interface(rules, s, node),
            Some(ClassKind::Trait) => extract_class(rules, s, node, NodeKind::Trait),
            _ => extract_class(rules, s, node, NodeKind::Class),
        }
    } else if t.extra_class_node_types.contains(&node_type) {
        extract_class(rules, s, node, NodeKind::Class);
    } else if t.method_types.contains(&node_type) {
        // TS/JS #808: a field-shaped method node may be a plain property.
        if rules.classify_method_node(node, s.source()) == Some(MethodClass::Property) {
            let prop_id = extract_property(rules, s, node);
            // Walk the initializer so its calls/instantiations attribute to
            // the PROPERTY (`client = new HttpClient()` → client instantiates
            // HttpClient; `history = createHistory()` → history calls
            // createHistory). TS `tree-sitter.ts:996-1006` pushes `propNode.id`
            // as the scope and hands the `value` field to `visitFunctionBody`;
            // without this the field subtree is consumed by the property branch
            // and nothing ever walks it (`resolve_body` only reaches function
            // bodies), so these edges were missing outright.
            //
            // This lives HERE, not in `extract_property`, because TS gates it
            // here too: the `propertyTypes` (C#/Kotlin/Scala) and `fieldTypes`
            // (Java/C#) branches at `tree-sitter.ts:1038-1055` call
            // extract{Property,Field} and then ONLY `scanFnRefSubtree` — they
            // never walk the value. `classify_method_node` returns
            // `Property` for TS/JS alone, so no other language reaches this.
            if let Some(prop_id) = prop_id
                && let Some(value) = get_child_by_field(node, "value")
            {
                s.push_scope(prop_id);
                s.visit_function_body(value, "");
                s.pop_scope();
            }
            // A field initializer can also register callbacks
            // (`static handlers = { click: onClick }`) — the property branch
            // consumes the subtree, so scan it for fn-ref candidates
            // (TS `tree-sitter.ts:1007-1010`). Scanned from the CLASS scope
            // (the stack top here), while the value walk above captured the
            // same names under the PROPERTY scope: two distinct `from_node_id`s,
            // so the flush-time dedup on `(from_node_id, name)` keeps both —
            // exactly as TS does. The class-scoped one is what resolution and
            // callers/impact traverse.
            body::scan_fn_ref_subtree(s, node, 0);
        } else {
            extract_method(rules, s, node);
        }
    } else if t.interface_types.contains(&node_type) {
        extract_interface(rules, s, node);
    } else if t.struct_types.contains(&node_type) {
        extract_struct(rules, s, node);
    } else if t.enum_types.contains(&node_type) {
        extract_enum(rules, s, node);
    } else if t.type_alias_types.contains(&node_type) {
        // TS semantics: a plain alias does NOT skip children (the walker
        // recurses into the alias value); a reclassified one does.
        matched = extract_type_alias(rules, s, node);
    } else if t.property_types.contains(&node_type) && s.is_inside_class_like() {
        // C#/Kotlin/Scala: TS `tree-sitter.ts:1038-1044` does NOT walk the
        // value here (unlike the TS/JS field branch above) — don't either.
        let _ = extract_property(rules, s, node);
        // Property initializers aren't walked — scan for fn-ref candidates
        // (Kotlin `val cb = ::handler` class properties — Task 15b).
        body::scan_fn_ref_subtree(s, node, 0);
    } else if t.field_types.contains(&node_type) && s.is_inside_class_like() {
        extract_field(rules, s, node);
        // Field initializers aren't walked — scan for fn-ref candidates (Java
        // `List<IntConsumer> table = List.of(Main::cb)`, C#
        // `List<Action<int>> table = new() { TargetCb }` — Task 15b).
        body::scan_fn_ref_subtree(s, node, 0);
    } else if t.variable_types.contains(&node_type)
        && (!s.is_inside_class_like() || is_class_scope_constant_assignment(node))
    {
        // Top-level variables — plus class/module-scope CONSTANTS (Task 14):
        // a Ruby `CONST = …` has a `constant`-typed LHS; no other grammar
        // puts one here, so the gate is effectively Ruby-only and never
        // disturbs other languages' class-internal locals.
        extract_variable(rules, s, node);
        // `extract_variable` doesn't walk every initializer shape (object
        // literals are deliberately skipped; Python/C don't walk at all), so
        // scan the declaration subtree for fn-ref candidates — `const routes =
        // { home: renderHome }`, `handlers = {"recv": target_cb}`, `static
        // cb_t table[] = { cb_a, cb_b }`. The scan halts at nested function
        // definitions (their bodies are walked — and attributed — separately)
        // and flush-time dedup absorbs any overlap with the initializers
        // `extract_variable` DOES walk — AND the fact that this node itself was
        // already offered to `maybe_capture_fn_refs` pre-ladder, so the scan's
        // depth-0 visit re-offers it (TS does the same: ts:954 then ts:1074;
        // the `(from_node_id, name)` dedup key collapses the pair).
        body::scan_fn_ref_subtree(s, node, 0);
    } else if t.import_types.contains(&node_type) {
        extract_import(rules, s, node);
        // TS parity (`tree-sitter.ts:1173-1175`): the import branch does NOT
        // set `skipChildren` — it extracts the import and KEEPS WALKING.
        //
        // This is load-bearing for Ruby, whose `import_types` is `["call"]`
        // (require/require_relative) — the same node type as its `call_types`.
        // Because the ladder tests imports BEFORE calls, EVERY class/file-scope
        // Ruby `call` lands in this branch, including every DSL block
        // (`RSpec.describe … do … end`, `namespace :x do … end`, Rails
        // routers/callbacks). Skipping children here consumed those subtrees
        // whole: the declarations inside a DSL block body were never extracted
        // (an `RSpec.describe` block's methods simply did not exist as nodes,
        // so their bodies were never walked either) and the hook-DSL fn-ref
        // symbols (`before_action :authenticate`) were never captured.
        //
        // For every other v0 language an import subtree holds only module
        // paths and binding names, which no ladder branch matches — so
        // recursing is inert there (pinned by the per-language import tests +
        // snapshots).
        matched = false;
    }
    // TS/JS re-export refs: `export { A, B as C } from './y'` — barrels
    // record a dependency on their source module (Task 8).
    else if node_type == "export_statement"
        && is_ts_js_language(s.language())
        && get_child_by_field(node, "source").is_some()
    {
        if let Some(parent_id) = s.scope_id().cloned() {
            s.emit_re_export_refs(node, &parent_id);
        }
        matched = false; // children still recurse (a re-export can't nest, but parity)
    }
    // Vuex MODULE default export: `export default { namespaced, actions:
    // {…} }` — store-file gated; the collection methods become nodes and
    // the subtree is consumed (Task 8).
    else if node_type == "export_statement"
        && is_ts_js_language(s.language())
        && s.looks_like_vue_store_file()
        && let Some(exported) = get_child_by_field(node, "value")
        && (exported.kind() == "object" || exported.kind() == "object_expression")
    {
        s.extract_store_collection_methods(rules, exported);
    } else if t.call_types.contains(&node_type) {
        // Top-level calls (IIFE module wrappers #528, side-effect calls)
        // attribute to the stack top; children STILL recurse so nested
        // arrows/calls extract (TS: skipChildren stays false here).
        s.extract_call(node);
        matched = false;
    } else if body::INSTANTIATION_KINDS.contains(&node_type) {
        // Children still walked so ctor-arg calls get their own refs.
        s.extract_instantiation(node);
        // Java/C# `new T(...) { … }` — anonymous class with body (Task 10):
        // consumed whole (TS skipChildren = true); plain instantiations
        // keep recursing.
        if let Some(anon_body) = body::find_anonymous_class_body(node) {
            s.extract_anonymous_class(node, anon_body);
        } else {
            matched = false;
        }
    }
    // INSERTION POINT (Task 9): Rust `impl_item` implements refs.
    // TS interface members: property_signature / method_signature carry
    // type annotations the interface walker would otherwise drop (Task 8).
    else if (node_type == "property_signature" || node_type == "method_signature")
        && s.is_inside_class_like()
        && ts_core::is_type_annotation_language(s.language())
    {
        if let Some(parent_id) = s.scope_id().cloned() {
            s.extract_type_annotations(rules, node, &parent_id);
        }
        matched = false; // nested signatures still need traversal
    } else {
        matched = false;
    }

    if matched {
        return; // matched branches walked (or deliberately skipped) children
    }

    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
}

/// The TS/JS language family gate for the ts_core ladder branches.
pub(super) fn is_ts_js_language(l: Language) -> bool {
    matches!(
        l,
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx
    )
}
