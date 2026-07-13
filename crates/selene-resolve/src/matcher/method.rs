//! [`resolve_method_on_type`] — **the safety mechanism** — and
//! [`match_method_call`], strategy 2 of the name matcher.
//!
//! # Validated inference: no edge beats a wrong edge
//!
//! Every type guess in this crate — a receiver inferred from a local declaration
//! (Task 8), a factory's return type (Task 9) — ends here. And
//! [`resolve_method_on_type`] does not *assume* the type is right: it requires the
//! method to **actually exist** on that type, or on a supertype it conforms to.
//!
//! So a mis-inference produces **no edge**, never a wrong one. That single
//! property is what makes it safe for the receiver patterns to be loose regexes
//! rather than a type checker, and it is the reason every chained-call language
//! block in the TS suite carries a *"creates NO edge when the type lacks the
//! method"* test.

use std::collections::HashSet;

use selene_core::{Language, Node, NodeKind, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::matcher::receiver::{
    infer_cpp_receiver_type, infer_java_field_receiver_type, infer_local_receiver_type,
};
use crate::matcher::scoring::{ambiguous_name_ceiling, prefer_call_site_file};
use crate::types::{ResolvedBy, ResolvedRef};

/// How deep the supertype/conformance walk may recurse.
const CONFORMANCE_MAX_DEPTH: u8 = 4;

/// Kinds that can own a supertype (the `implements`/`extends` sources).
const SUPERTYPE_BEARING: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
];

/// Resolve `method` on `type_name` — **and validate that it exists there**.
///
/// Matches are `method`-kind nodes of the reference's language whose
/// `qualified_name` is `"{type}::{method}"` or ends with `"::{type}::{method}"`.
/// The suffix form is what makes an **out-of-line** definition work
/// (`int Foo::bar() { … }` in `foo.cpp` while `class Foo` lives in `foo.hpp` — the
/// typical C++ layout, which a same-file-only lookup misses entirely).
///
/// # The conformance fallback
///
/// When the type declares no such method, the method may live on a **supertype**
/// it conforms to (an inherited method, a default-interface method, a trait
/// default, a Go embedded struct). The walk follows the resolved
/// `implements`/`extends` edges, depth-capped at `CONFORMANCE_MAX_DEPTH`.
///
/// Those edges are **empty during the first resolution pass** and populated by the
/// time the conformance pass runs (Task 9) — which is exactly why the deferral
/// exists. The walk is still *validated*: the method must exist on the supertype,
/// so a wrong inference still yields no edge.
///
/// # Tie-breaks, in order
///
/// 1. **`preferred_fqn`** — a Java/Kotlin import pins WHICH `FooConverter` the
///    caller means when two packages declare one (#314). Its target is
///    deliberately in *another* file, so this must run before…
/// 2. **`prefer_call_site_file`** — …the same-file preference, which otherwise
///    collapses every ambiguous call onto the first-indexed definition, so a call
///    in `b/svc.cpp` wrongly points at `a/svc.cpp` (#1079).
#[allow(clippy::too_many_arguments)]
pub fn resolve_method_on_type<C: ResolutionContext>(
    type_name: &str,
    method: &str,
    r: &UnresolvedRef,
    ctx: &C,
    confidence: f64,
    resolved_by: ResolvedBy,
    preferred_fqn: Option<&str>,
    depth: u8,
) -> Option<ResolvedRef> {
    let lang = Language::from_wire(&r.language)?;
    let matches = ctx.method_matches(lang, type_name, method);

    if matches.is_empty() {
        // THE conformance fallback. Still validated — the method must exist on a
        // supertype, so a wrong inference produces no edge.
        if depth < CONFORMANCE_MAX_DEPTH {
            for supertype in supertype_names_of(type_name, r, ctx) {
                if let Some(via) = resolve_method_on_type(
                    &supertype,
                    method,
                    r,
                    ctx,
                    confidence,
                    resolved_by,
                    preferred_fqn,
                    depth + 1,
                ) {
                    return Some(via);
                }
            }
        }
        // The method does not exist on the type, or on anything it conforms to.
        // NO EDGE. This is the whole safety property.
        return None;
    }

    // (1) A Java/Kotlin import pins which same-named class the caller means (#314).
    if matches.len() > 1
        && let Some(fqn) = preferred_fqn
    {
        let ext = if lang == Language::Kotlin {
            ".kt"
        } else {
            ".java"
        };
        let fqn_path = format!("{}{ext}", fqn.replace('.', "/"));
        if let Some(chosen) = matches
            .iter()
            .find(|m| m.file_path.replace('\\', "/").ends_with(&fqn_path))
        {
            return Some(bind(r, &chosen.id, confidence, resolved_by));
        }
    }

    // (2) Otherwise the call site's own file wins (#1079).
    let ordered = prefer_call_site_file(&matches, &r.file_path);
    let chosen = ordered.first()?;
    Some(bind(r, &chosen.id, confidence, resolved_by))
}

/// The supertypes of every same-named type node — by NAME, unioned.
///
/// `ResolutionContext::supertypes` is node-anchored (it takes an id). The
/// conformance walk needs "what does the type *named* `Foo` extend?", so this
/// unions the supertypes of every supertype-bearing node named `Foo` in the
/// reference's own language.
///
/// That union is what the TS build does here, and it is safe **because the walk is
/// validated**: a supertype pulled in from an unrelated same-named class only
/// matters if it actually declares the method, and then
/// `resolve_method_on_type`'s own file/FQN tie-breaks pick between the results.
/// (The *unvalidated* name-keyed union is what caused the rails cross-class bug —
/// and that path, `resolve_deferred_this_member_refs`, is node-anchored for exactly
/// that reason. See Task 10.)
fn supertype_names_of<C: ResolutionContext>(
    type_name: &str,
    r: &UnresolvedRef,
    ctx: &C,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for node in ctx.nodes_by_name(type_name) {
        if !SUPERTYPE_BEARING.contains(&node.kind) || node.language != r.language {
            continue;
        }
        for supertype in ctx.supertypes(&node.id) {
            if seen.insert(supertype.name.clone()) {
                out.push(supertype.name);
            }
        }
    }
    out
}

fn bind(r: &UnresolvedRef, target: &str, confidence: f64, by: ResolvedBy) -> ResolvedRef {
    ResolvedRef {
        // ⚠ The STORED ROW, unmutated — the keyed delete matches on it (#760).
        original: r.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: by,
    }
}

/// The FQN a Java/Kotlin file's import binds `type_name` to (#314).
pub(crate) fn imported_fqn_of<C: ResolutionContext>(
    type_name: &str,
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<String> {
    ctx.import_mappings(&r.file_path)
        .iter()
        .find(|i| i.local_name == type_name)
        .map(|i| i.source.clone())
}

/// Split a camelCase/PascalCase string into words (dropping 1-char fragments).
fn split_camel_case(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, ' ' | '.' | '_' | ':' | '/' | '\\') {
            if current.len() > 1 {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            continue;
        }
        // A lower→upper boundary, or an ACRONYM→Word boundary (`HTTPServer`).
        let boundary = i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase()
                || (chars[i - 1].is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase())));
        if boundary {
            if current.len() > 1 {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
        current.push(c);
    }
    if current.len() > 1 {
        out.push(current);
    }
    out
}

/// Strategy 2 — a **method call** on a receiver whose type we can infer.
///
/// Shapes: `receiver.method` (the receiver may itself be dotted, so a C# DI
/// `builder.Services.AddCoreServices()` resolves by its last segment),
/// `Class::method`, and the PHP `this->prop.method` form.
///
/// | strategy | confidence · `resolved_by` |
/// |---|---|
/// | 0. PHP typed property (**exclusive**) | **0.9** · `instance-method` |
/// | 1. local receiver inference → validated on the type | **0.9** · `instance-method` |
/// | 2. Java/Kotlin field-signature inference | **0.9** · `instance-method` |
/// | 3. the receiver names a class, method found in its file | **0.85** · `qualified-name` |
/// | 4. the CAPITALIZED receiver names a class | **0.8** · `instance-method` |
/// | 5. method-name fallback (unique same-language **0.7**, else word-overlap ≥ 2 **0.65**) | `instance-method` |
pub fn match_method_call<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    let lang = Language::from_wire(&r.language)?;
    let name = r.reference_name.as_str();

    // --- 0. PHP `$this->prop->method()` — an EXCLUSIVE path -------------------
    // The extractor encodes it as `this->prop.method`. It resolves ONLY through
    // declared-type inference + validation: the name-similarity strategies below
    // must never see this shape, so a property whose type cannot be recovered
    // stays unlinked rather than guessed.
    if lang == Language::Php
        && let Some((receiver, method)) = split_php_this_prop(name)
    {
        let inferred = infer_local_receiver_type(&receiver, r, ctx)?;
        let fqn = imported_fqn_of(&inferred, r, ctx);
        return resolve_method_on_type(
            &inferred,
            &method,
            r,
            ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            fqn.as_deref(),
            0,
        );
    }

    let (receiver, method, inferable) = parse_receiver(name, lang)?;

    // --- 1. Local receiver inference (#1108) ---------------------------------
    if inferable {
        let inferred = if lang == Language::Cpp {
            infer_cpp_receiver_type(&receiver, r, ctx, 0)
        } else {
            infer_local_receiver_type(&receiver, r, ctx)
        };

        if let Some(ty) = inferred {
            // Java/Kotlin: when two classes share a simple name, the file's import
            // pins WHICH one (#314). Other languages disambiguate by call-site file.
            let fqn = if matches!(lang, Language::Java | Language::Kotlin) {
                imported_fqn_of(&ty, r, ctx)
            } else {
                None
            };
            if let Some(hit) = resolve_method_on_type(
                &ty,
                &method,
                r,
                ctx,
                0.9,
                ResolvedBy::InstanceMethod,
                fqn.as_deref(),
                0,
            ) {
                return Some(hit);
            }
            // The type was inferred but does NOT declare the method: fall through.
            // (The validation held — no edge was invented.)
        }
    }

    // --- 2. Java/Kotlin field receivers ---------------------------------------
    // A field name often does not match its type by convention (`userbo` → class
    // `UserBO`). Covers Spring `@Resource`/`@Autowired` field injection.
    if matches!(lang, Language::Java | Language::Kotlin)
        && inferable
        && let Some(ty) = infer_java_field_receiver_type(&receiver, r, ctx)
    {
        let fqn = imported_fqn_of(&ty, r, ctx);
        if let Some(hit) = resolve_method_on_type(
            &ty,
            &method,
            r,
            ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            fqn.as_deref(),
            0,
        ) {
            return Some(hit);
        }
    }

    // --- 3. The receiver names a class directly -------------------------------
    // `Logger.log()` where a `Logger` exists in both `a/` and `b/`: the call site's
    // own file wins, or the first-indexed class does and a call in `b/` resolves
    // to `a/` (#1079).
    if let Some(hit) =
        method_in_class_named(&receiver, &method, r, ctx, 0.85, ResolvedBy::QualifiedName)
    {
        return Some(hit);
    }

    // --- 4. The CAPITALIZED receiver names a class ----------------------------
    // An instance variable: `permissionEngine.check()` → class `PermissionEngine`.
    let capitalized = crate::builtins::capitalize_ascii(&receiver);
    if capitalized != receiver
        && let Some(hit) = method_in_class_named(
            &capitalized,
            &method,
            r,
            ctx,
            0.8,
            ResolvedBy::InstanceMethod,
        )
    {
        return Some(hit);
    }

    // --- 5. Method-name fallback ----------------------------------------------
    method_name_fallback(&receiver, &method, r, ctx)
}

/// `this->prop.method` → (`this->prop`, `method`). A DEEPER chain
/// (`this->a->b.method`) does not match, and stays unlinked — same as TS.
fn split_php_this_prop(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("this->")?;
    let (prop, method) = rest.split_once('.')?;
    if prop.is_empty()
        || method.is_empty()
        || !prop.chars().all(|c| c.is_alphanumeric() || c == '_')
        || !method.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }
    Some((format!("this->{prop}"), method.to_string()))
}

/// Split a reference into `(receiver, method, inferable)`.
///
/// - `a.b` / `a.b.c` — dotted. The receiver may itself be dotted so a chained C#
///   DI call (`builder.Services.AddCoreServices()`) resolves by its last segment.
///   **Inferable.**
/// - `A::b` — scoped. **Not** inferable (the receiver is a type already).
/// - Lua `a:b`, R `a$b` — inferable (wave 2 languages, but the shapes are free).
fn parse_receiver(name: &str, lang: Language) -> Option<(String, String, bool)> {
    // ObjC selectors carry trailing colons (`storeImage:`) — harmless elsewhere.
    if let Some(dot) = name.rfind('.') {
        let (recv, method) = (&name[..dot], &name[dot + 1..]);
        if !recv.is_empty()
            && !method.is_empty()
            && recv
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && method
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
        {
            return Some((recv.to_string(), method.to_string(), true));
        }
    }
    if let Some(i) = name.find("::") {
        let (recv, method) = (&name[..i], &name[i + 2..]);
        if !recv.is_empty()
            && !method.is_empty()
            && recv.chars().all(|c| c.is_alphanumeric() || c == '_')
            && method.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            // A `::` receiver IS a type name — there is nothing to infer.
            return Some((recv.to_string(), method.to_string(), false));
        }
    }
    if matches!(lang, Language::Lua | Language::Luau)
        && let Some((recv, method)) = name.split_once(':')
        && !recv.is_empty()
        && !method.is_empty()
    {
        return Some((recv.to_string(), method.to_string(), true));
    }
    if lang == Language::R
        && let Some((recv, method)) = name.split_once('$')
        && !recv.is_empty()
        && !method.is_empty()
    {
        return Some((recv.to_string(), method.to_string(), true));
    }
    None
}

/// Find `method` inside the file of a class named `class_name` (same language),
/// preferring the call site's own file among several same-named classes.
fn method_in_class_named<C: ResolutionContext>(
    class_name: &str,
    method: &str,
    r: &UnresolvedRef,
    ctx: &C,
    confidence: f64,
    by: ResolvedBy,
) -> Option<ResolvedRef> {
    let classes = prefer_call_site_file(&ctx.nodes_by_name(class_name), &r.file_path);

    for class_node in classes {
        if !matches!(
            class_node.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Interface
        ) || class_node.language != r.language
        {
            continue;
        }
        let found = ctx
            .nodes_in_file(&class_node.file_path)
            .into_iter()
            .find(|n| {
                n.kind == NodeKind::Method
                    && n.name == method
                    && n.qualified_name.contains(&class_node.name)
            });
        if let Some(m) = found {
            return Some(bind(r, &m.id, confidence, by));
        }
    }
    None
}

/// Strategy 5 — find the method by NAME across the codebase, then score it against
/// the receiver.
///
/// A unique same-language method resolves at **0.7**. Several candidates are scored
/// by camelCase **word overlap** between the receiver and the method's qualified
/// name (`permissionEngine` → `PermissionRuleEngine`), **+1** for the same language,
/// and the winner must score **≥ 2** — below that it is a coin flip, and a coin flip
/// is a wrong edge waiting to happen.
///
/// The ceiling (#999) applies here too: a method name re-declared across a vendored
/// theme/SDK (`init`, `update` on every widget) yields K candidates that word overlap
/// cannot disambiguate, and scoring all K per call is the O(K²) that wedged
/// "resolving refs" for 15–28 minutes. Strategies 1–4 already had their precise shot.
fn method_name_fallback<C: ResolutionContext>(
    receiver: &str,
    method: &str,
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let candidates = ctx.nodes_by_name(method);
    if candidates.len() > ambiguous_name_ceiling() {
        return None;
    }

    let methods: Vec<Node> = candidates
        .into_iter()
        .filter(|n| n.kind == NodeKind::Method && n.name == method)
        .collect();

    let same_language: Vec<Node> = methods
        .iter()
        .filter(|m| m.language == r.language)
        .cloned()
        .collect();
    let targets = if same_language.is_empty() {
        methods
    } else {
        same_language
    };

    if targets.len() == 1 && targets[0].language == r.language {
        return Some(bind(r, &targets[0].id, 0.7, ResolvedBy::InstanceMethod));
    }

    if targets.len() > 1 {
        let receiver_words = split_camel_case(receiver);
        let mut best: Option<&Node> = None;
        let mut best_score = 0i32;

        // Same-file candidates first, so a tie (`>` keeps the first seen) resolves
        // to the call site's own file rather than the first-indexed duplicate (#1079).
        let ordered = prefer_call_site_file(&targets, &r.file_path);
        for m in &ordered {
            let class_words = split_camel_case(&m.qualified_name);
            let mut score = receiver_words
                .iter()
                .filter(|w| {
                    class_words
                        .iter()
                        .any(|cw| cw.eq_ignore_ascii_case(w.as_str()))
                })
                .count() as i32;
            if m.language == r.language {
                score += 1;
            }
            if score > best_score {
                best_score = score;
                best = Some(m);
            }
        }

        if let Some(m) = best
            && best_score >= 2
        {
            return Some(bind(r, &m.id, 0.65, ResolvedBy::InstanceMethod));
        }
    }

    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_splits_words_and_acronyms() {
        assert_eq!(
            split_camel_case("permissionEngine"),
            vec!["permission", "Engine"]
        );
        assert_eq!(
            split_camel_case("PermissionRuleEngine"),
            vec!["Permission", "Rule", "Engine"]
        );
        assert_eq!(split_camel_case("HTTPServer"), vec!["HTTP", "Server"]);
        assert_eq!(split_camel_case("Foo::bar"), vec!["Foo", "bar"]);
        // Single-char fragments are dropped (they overlap with everything).
        assert_eq!(split_camel_case("aB"), Vec::<String>::new());
    }

    #[test]
    fn the_receiver_parser_reads_every_shape() {
        assert_eq!(
            parse_receiver("lg.log", Language::Typescript).unwrap(),
            ("lg".into(), "log".into(), true)
        );
        // A dotted receiver: the C# DI chain resolves by its last segment.
        assert_eq!(
            parse_receiver("builder.Services.AddCore", Language::CSharp).unwrap(),
            ("builder.Services".into(), "AddCore".into(), true)
        );
        // A `::` receiver IS a type — nothing to infer.
        assert_eq!(
            parse_receiver("Logger::log", Language::Cpp).unwrap(),
            ("Logger".into(), "log".into(), false)
        );
        assert!(parse_receiver("bare", Language::Typescript).is_none());
    }

    #[test]
    fn the_php_this_prop_shape_is_exclusive_and_shallow() {
        assert_eq!(
            split_php_this_prop("this->repo.save").unwrap(),
            ("this->repo".into(), "save".into())
        );
        assert!(
            split_php_this_prop("this->a->b.save").is_none(),
            "a deeper chain does not match the single-property shape — it stays \
             unlinked rather than guessed"
        );
        assert!(split_php_this_prop("repo.save").is_none());
    }
}
