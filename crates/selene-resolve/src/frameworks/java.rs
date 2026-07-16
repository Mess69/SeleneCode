//! **Spring** (Task 16) — Java + Kotlin routes, config-key nodes, `@Value`
//! relaxed binding, and the DI naming conventions.
//!
//! # Two hops here are framework work, and both are load-bearing
//!
//! ## 1. The bare-mapping + class-prefix join
//!
//! ```java
//! @RestController
//! @RequestMapping("/articles")          // ← a PREFIX, not a route
//! public class ArticleController {
//!     @GetMapping("/{slug}")           // ← the route is the JOIN of the two
//!     public Article getBySlug(...) { … }
//!     @GetMapping                       // ← BARE. Still a route: GET /articles
//!     public List<Article> list() { … }
//! }
//! ```
//!
//! Miss either half and the dominant Spring shape — a multi-method controller —
//! has *no routes at all*. (The TS build measured it: one real repo produced 28
//! routes across 2,444 files before this join landed.)
//!
//! ## 2. The config bridge — the hop that would otherwise half-bridge
//!
//! ```text
//! @Value("${app.cacheList}")   →   application.yml:  app:
//!   (a bind node in the .java)                         cache-list: true
//! ```
//!
//! An agent tracing "where does this timeout come from?" must land **on the key
//! in `application.yml`**, not one hop short of it. That is why `yaml` and
//! `properties` are in [`FrameworkResolver::languages`] — without them `extract`
//! never runs on the config files, the keys are never nodes, and the `@Value`
//! reference dangles. Per PRD §8.2 that half-bridge is *worse* than no bridge:
//! the agent follows the map, lands nowhere, and goes back to reading files.
//!
//! **Relaxed binding** is what makes the two ends meet: Spring itself treats
//! `cache-list`, `cacheList` and `cache_list` as the same key, so we compare on
//! [`canonical`] (`lowercase`, then strip `-` and `_`), never on the literal.
//!
//! # Two hard contracts
//!
//! - **A config VALUE is never stored** (#383). The node carries the *key* — its
//!   name, its qualified name, its file and its line. Nothing else. A
//!   `password: hunter2` in `application.yml` must not become graph content, and
//!   `config_values_are_never_stored` pins it.
//! - **A `calls` reference never resolves to a config key** (#1180). Only
//!   `references` refs reach the key index. This started as a performance fix (a
//!   dotted `calls` ref scanning every constant in the repo) and it is also a
//!   precision one: `service.timeout(…)` is a method call, not the `app.timeout`
//!   property. [`Spring::resolve`] gates on the kind before it touches the index.
//!
//! # ⚠ Sequencing contract for the batch driver (Part C)
//!
//! The `@Value` reference's name (`app.cacheList`) is declared by the **bind node
//! this pass emits** — nothing in the ordinary extraction declares it. So the
//! resolver's `known_names` set (warmed **once**, in `StoreContext::new`) must be
//! built *after* [`crate::frameworks::run_framework_extract`] has run, or
//! `resolve_one`'s step-3 pre-filter drops every `@Value` ref before this
//! resolver is ever asked. Extract first, **then** construct the resolution
//! context.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef, node_id};

use crate::context::ResolutionContext;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, by_convention, char_boundary_at_or_below,
    line_of, manifest_mentions, route_node_in,
};
use crate::types::{ResolvedBy, ResolvedRef};

/// `updated_at` for every node this module emits — **zero, deliberately**. Route
/// and config emission must be byte-deterministic: the same source must produce
/// the same nodes on every run, and a wall clock is the one thing that cannot be.
const NO_CLOCK: i64 = 0;

/// The framework name. Written into `Node::framework` on every node this module
/// emits, and the discriminator [`is_config_node`] filters the key index by.
const SPRING: &str = "spring";

/// How far past an annotation the handler may be. Ported verbatim.
const HANDLER_WINDOW: usize = 600;

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

// =============================================================================
// Patterns
// =============================================================================

/// Any `@RequestMapping(...)`. Whether it is a *class* prefix or a *method* route
/// is decided by what FOLLOWS it — see [`CLASS_TAIL`].
static REQUEST_MAPPING: LazyLock<Regex> = re!(r"@RequestMapping\s*\(([^)]*)\)");

/// What follows a **class-level** annotation: more annotations, then modifiers,
/// then `class` / `interface` / `object`. Anchored with `^`, so it is only ever
/// tried at the position right after a `@RequestMapping(...)` — it never scans.
static CLASS_TAIL: LazyLock<Regex> = re!(
    r"^\s*(?:@\w+(?:\([^)]*\))?\s*)*(?:(?:public|private|protected|final|abstract|open|internal|data|sealed)\s+)*(?:class|interface|object)\b"
);

/// `@GetMapping`, `@PostMapping`, … — the parens are **optional** (a bare
/// `@GetMapping` is a route on the class prefix, and it is common). Group 2 is
/// the argument list *inside* the parens.
static VERB_MAPPING: LazyLock<Regex> =
    re!(r"@(Get|Post|Put|Patch|Delete)Mapping\b\s*(?:\(([^)]*)\))?");

/// `method = RequestMethod.POST` inside a method-level `@RequestMapping`.
static REQUEST_METHOD: LazyLock<Regex> = re!(r"method\s*=\s*\{?\s*RequestMethod\.(\w+)");

/// An explicit `value = "/x"` / `path = "/x"` in an annotation's arguments.
static NAMED_PATH: LazyLock<Regex> = re!(r#"(?:value|path)\s*=\s*\{?\s*["']([^"']*)["']"#);

/// The first quoted token of an annotation's arguments.
static FIRST_QUOTED: LazyLock<Regex> = re!(r#"["']([^"']*)["']"#);

/// Java handler signature: `public ResponseEntity<Article> getBySlug(` → `getBySlug`.
static JAVA_HANDLER: LazyLock<Regex> =
    re!(r"\b(?:public|private|protected)\s+[^;{=]*?\s+(\w+)\s*\(");

/// Kotlin handler signature: `fun getBySlug(` → `getBySlug`.
static KOTLIN_HANDLER: LazyLock<Regex> = re!(r"\bfun\s+(\w+)\s*\(");

/// `@Value("${app.timeout:30}")` → key `app.timeout` (**the default is dropped**;
/// it is a value, and values are not ours to keep).
static VALUE_ANNOT: LazyLock<Regex> =
    re!(r#"@Value\s*\(\s*["']\$\{([^:}]+)(?::[^}]*)?\}["']\s*\)"#);

/// `@ConfigurationProperties(prefix = "app.cache")` → prefix `app.cache`.
static CONFIG_PROPS: LazyLock<Regex> =
    re!(r#"@ConfigurationProperties\s*\(\s*(?:prefix\s*=\s*)?["']([^"']+)["']"#);

/// The config files Spring actually reads — `application.yml`,
/// `bootstrap-prod.properties`, … A `docker-compose.yml` is not one of them, and
/// turning every yaml key in a repo into a graph node is how a config index
/// becomes noise.
static CONFIG_BASENAME: LazyLock<Regex> =
    re!(r"^(?:application|bootstrap)(?:-[\w.-]+)?\.(?:yml|yaml|properties)$");

/// A yaml `key:` line — with an optional inline value (which decides leaf-ness).
static YAML_KEY: LazyLock<Regex> = re!(r"^(\s*)([\w.\-]+)\s*:\s*(.*)$");

/// A `.properties` entry — `key=value` or `key: value`.
static PROPS_KEY: LazyLock<Regex> = re!(r"^\s*([\w.\-]+)\s*[=:]\s*(.*)$");

/// The annotations that give a Spring project away when there is no build file.
static SPRING_ANNOT: LazyLock<Regex> =
    re!(r"@(?:SpringBootApplication|RestController|Service|Repository)\b");

const JVM_MANIFESTS: [&str; 3] = ["pom.xml", "build.gradle", "build.gradle.kts"];

// =============================================================================
// Relaxed binding
// =============================================================================

/// Spring's **relaxed binding**, canonicalized: `app.cache-list`, `app.cacheList`
/// and `app.cache_list` are the same key, and Spring itself binds all three. So
/// the key index is keyed on this, never on the literal spelling.
pub fn canonical(key: &str) -> String {
    key.to_lowercase().replace(['-', '_'], "")
}

/// `/articles` + `/{slug}` → `/articles/{slug}`; `""` + `""` → `/`. Duplicate and
/// trailing slashes collapse, because `//articles` and `/articles` are the same
/// route and must not become two nodes.
fn join_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<&str> = prefix
        .split('/')
        .chain(sub.split('/'))
        .filter(|s| !s.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// The path an annotation declares: an explicit `value =` / `path =`, else a bare
/// leading string literal.
///
/// **Deviation from the TS (deliberate, precision):** the TS build took the first
/// quoted token of the argument list, so `@GetMapping(produces = "application/json")`
/// registered a route at the path `application/json`. A junk route is not a
/// harmless one — it is a node an agent can be sent to. We take a quoted token
/// only when it is *positional* or explicitly the `value`/`path` argument.
fn annotation_path(args: &str) -> Option<String> {
    if let Some(c) = NAMED_PATH.captures(args) {
        return c.get(1).map(|m| m.as_str().to_string());
    }
    let head = args.trim_start().trim_start_matches('{').trim_start();
    if head.starts_with('"') || head.starts_with('\'') {
        return FIRST_QUOTED
            .captures(head)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
    }
    None
}

/// The handler a mapping annotation decorates: the first method signature within
/// [`HANDLER_WINDOW`] bytes of it.
fn next_handler(src: &str, offset: usize, language: Language) -> Option<(String, u32)> {
    let end = char_boundary_at_or_below(src, offset.saturating_add(HANDLER_WINDOW));
    let start = char_boundary_at_or_below(src, offset.min(end));
    let window = &src[start..end];
    let re: &Regex = if language == Language::Kotlin {
        &KOTLIN_HANDLER
    } else {
        &JAVA_HANDLER
    };
    let m = re.captures(window)?.get(1)?;
    Some((m.as_str().to_string(), line_of(src, start + m.start())))
}

/// A `references` ref from a node this pass emitted to the symbol/key it names.
fn spring_ref(from: &str, name: &str, file: &str, line: u32, language: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: "references".to_string(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.to_string(),
        language: language.as_str().to_string(),
        status: RefStatus::Pending,
        name_tail: name.rsplit('.').next().unwrap_or(name).to_string(),
    }
}

/// A `Constant` node. **The only thing it ever carries is the key** — see the
/// module docs' secret-redaction contract (#383).
fn constant_node(
    name: &str,
    qualified_name: &str,
    file: &str,
    line: u32,
    language: Language,
) -> Node {
    Node {
        id: node_id(file, NodeKind::Constant, name, line),
        kind: NodeKind::Constant,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        file_path: file.to_string(),
        language: language.as_str().to_string(),
        start_line: line,
        end_line: line,
        start_column: 0,
        end_column: 0,
        // ⚠ NOT a place to stash the value. Nothing here is.
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        route_method: None,
        route_path: None,
        framework: Some(SPRING.to_string()),
        updated_at: NO_CLOCK,
    }
}

// =============================================================================
// Spring
// =============================================================================

/// Spring / Spring Boot.
pub struct Spring;

impl FrameworkResolver for Spring {
    fn name(&self) -> &'static str {
        SPRING
    }

    /// `Yaml` and `Properties` are **not decoration**: without them `extract`
    /// never runs on `application.yml`, and the `@Value` bridge above dies one
    /// hop short of the key.
    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[
            Language::Java,
            Language::Kotlin,
            Language::Yaml,
            Language::Properties,
        ])
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if manifest_mentions(ctx, &JVM_MANIFESTS, "spring-boot")
            || manifest_mentions(ctx, &JVM_MANIFESTS, "springframework")
        {
            return true;
        }
        // No build file (a vendored module, a partial checkout): the annotations
        // are the signal. Bounded — detection runs once per index, but it must
        // not read a monorepo's every Java file.
        ctx.all_files()
            .iter()
            .filter(|f| f.ends_with(".java"))
            .take(200)
            .any(|f| {
                ctx.read_file(f)
                    .is_some_and(|src| SPRING_ANNOT.is_match(&src))
            })
    }

    /// **The `*:prefix` refs, and only those.**
    ///
    /// `@ConfigurationProperties(prefix = "app")` emits a reference named
    /// `app:prefix`, which names no declared symbol anywhere — so without this
    /// hook `resolve_one`'s pre-filter drops it and the whole
    /// `@ConfigurationProperties` bridge is silently inert.
    ///
    /// A `@Value` ref (`app.cacheList`) needs no claim: the bind node this pass
    /// emits *is* named that, so the name is declared. That is only true if the
    /// resolution context is built after the extract pass — see the module docs'
    /// sequencing contract.
    fn claims_reference(&self, name: &str) -> bool {
        name.ends_with(":prefix")
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        match language {
            Language::Java | Language::Kotlin => extract_code(path, content, language),
            Language::Yaml | Language::Properties => extract_config(path, content, language),
            _ => FrameworkExtraction::default(),
        }
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if !matches!(
            Language::from_wire(&r.language),
            Some(Language::Java | Language::Kotlin)
        ) {
            return None;
        }

        // ⚠ #1180 — the hard gate. ONLY a `references` ref may reach the config
        // key index. A `calls` ref never does, whatever it is named.
        if r.reference_kind == "references" {
            if let Some(prefix) = r.reference_name.strip_suffix(":prefix") {
                return resolve_prefix(r, prefix, ctx);
            }
            if r.reference_name.contains('.')
                && let Some(hit) = resolve_config_key(r, ctx)
            {
                return Some(hit);
            }
        }

        resolve_by_di_convention(r, ctx)
    }
}

// =============================================================================
// Extract — Java / Kotlin
// =============================================================================

fn extract_code(path: &str, content: &str, language: Language) -> FrameworkExtraction {
    let mut out = FrameworkExtraction::default();

    // --- pass 1: the class-level prefix, and which mappings are class-level ---
    let mut prefix = String::new();
    let mut class_level: Vec<usize> = Vec::new();
    for caps in REQUEST_MAPPING.captures_iter(content) {
        let (Some(whole), Some(args)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        if !CLASS_TAIL.is_match(&content[whole.end()..]) {
            continue;
        }
        class_level.push(whole.start());
        // The FIRST controller's prefix wins. (One controller per file is the
        // Spring convention; NestJS's two-per-file shape is Task 13's problem.)
        if prefix.is_empty() {
            prefix = annotation_path(args.as_str()).unwrap_or_default();
        }
    }

    // --- pass 2: verb mappings — bare or with a path ---------------------------
    for caps in VERB_MAPPING.captures_iter(content) {
        let (Some(whole), Some(verb)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        let sub = caps
            .get(2)
            .and_then(|g| annotation_path(g.as_str()))
            .unwrap_or_default();
        push_route(
            &mut out,
            path,
            content,
            language,
            &verb.as_str().to_uppercase(),
            &join_path(&prefix, &sub),
            whole.start(),
            whole.end(),
        );
    }

    // --- pass 3: method-level `@RequestMapping` --------------------------------
    for caps in REQUEST_MAPPING.captures_iter(content) {
        let (Some(whole), Some(args)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        if class_level.contains(&whole.start()) {
            continue;
        }
        let method = REQUEST_METHOD
            .captures(args.as_str())
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_uppercase())
            .unwrap_or_else(|| "ANY".to_string());
        let sub = annotation_path(args.as_str()).unwrap_or_default();
        push_route(
            &mut out,
            path,
            content,
            language,
            &method,
            &join_path(&prefix, &sub),
            whole.start(),
            whole.end(),
        );
    }

    // --- pass 4: the config bridge --------------------------------------------
    for caps in VALUE_ANNOT.captures_iter(content) {
        let (Some(whole), Some(key)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        let key = key.as_str().trim();
        let line = line_of(content, whole.start());
        let node = constant_node(key, &format!("{path}::@Value:{key}"), path, line, language);
        out.refs
            .push(spring_ref(&node.id, key, path, line, language));
        out.nodes.push(node);
    }

    for caps in CONFIG_PROPS.captures_iter(content) {
        let (Some(whole), Some(p)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        let p = p.as_str().trim();
        let line = line_of(content, whole.start());
        let node = constant_node(
            p,
            &format!("{path}::@ConfigurationProperties:{p}"),
            path,
            line,
            language,
        );
        // `app:prefix` — the shape `claims_reference` exists for.
        out.refs.push(spring_ref(
            &node.id,
            &format!("{p}:prefix"),
            path,
            line,
            language,
        ));
        out.nodes.push(node);
    }

    sort_extraction(&mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn push_route(
    out: &mut FrameworkExtraction,
    path: &str,
    content: &str,
    language: Language,
    method: &str,
    route_path: &str,
    start: usize,
    end: usize,
) {
    let line = line_of(content, start);
    let node = route_node_in(
        &RouteSpec::new(SPRING, Some(method), route_path, path, line),
        language.as_str(),
        NO_CLOCK,
    );
    if let Some((handler, handler_line)) = next_handler(content, end, language) {
        out.refs
            .push(spring_ref(&node.id, &handler, path, handler_line, language));
    }
    out.nodes.push(node);
}

/// The three passes above walk the file three times, so emission order is
/// pass order, not source order. Sort — the node/ref order is observable (id
/// ties, parity diffs), and it must not depend on which pass found what.
fn sort_extraction(out: &mut FrameworkExtraction) {
    out.nodes
        .sort_by(|a, b| (a.start_line, &a.name, &a.id).cmp(&(b.start_line, &b.name, &b.id)));
    out.refs.sort_by(|a, b| {
        (a.line, &a.reference_name, &a.from_node_id).cmp(&(
            b.line,
            &b.reference_name,
            &b.from_node_id,
        ))
    });
}

// =============================================================================
// Extract — application.yml / application.properties
// =============================================================================

/// One `Constant` per **leaf** key. `app:` is not a leaf; `app.timeout: 30` is.
///
/// A parent key is not a node because nothing references it — `@Value("${app}")`
/// is not a thing. Emitting them would double the node count and give the agent
/// interior nodes it can never usefully land on.
fn extract_config(path: &str, content: &str, language: Language) -> FrameworkExtraction {
    let mut out = FrameworkExtraction::default();
    let basename = path.rsplit('/').next().unwrap_or(path);
    if !CONFIG_BASENAME.is_match(basename) {
        return out;
    }

    match language {
        Language::Yaml => extract_yaml(path, content, &mut out),
        Language::Properties => extract_properties(path, content, &mut out),
        _ => {}
    }
    sort_extraction(&mut out);
    out
}

fn extract_yaml(path: &str, content: &str, out: &mut FrameworkExtraction) {
    // (indent, key) — the ancestors of the line being read.
    let mut stack: Vec<(usize, String)> = Vec::new();

    for (i, raw) in content.lines().enumerate() {
        let line = (i + 1) as u32;
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let Some(caps) = YAML_KEY.captures(raw) else {
            continue;
        };
        let (Some(indent), Some(key), Some(value)) = (caps.get(1), caps.get(2), caps.get(3)) else {
            continue;
        };
        let indent = indent.as_str().len();

        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }

        // A value on the line ⇒ a leaf. No value ⇒ a parent; push and descend.
        // (The value itself is read only to make this decision. It is not kept.)
        if value.as_str().trim().is_empty() {
            stack.push((indent, key.as_str().to_string()));
            continue;
        }

        let mut dotted: Vec<&str> = stack.iter().map(|(_, k)| k.as_str()).collect();
        dotted.push(key.as_str());
        let dotted = dotted.join(".");
        out.nodes.push(constant_node(
            key.as_str(),
            &dotted,
            path,
            line,
            Language::Yaml,
        ));
    }
}

fn extract_properties(path: &str, content: &str, out: &mut FrameworkExtraction) {
    for (i, raw) in content.lines().enumerate() {
        let line = (i + 1) as u32;
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let Some(caps) = PROPS_KEY.captures(raw) else {
            continue;
        };
        let Some(key) = caps.get(1) else { continue };
        let key = key.as_str();
        // Every `.properties` entry is already a leaf, and its key is already
        // dotted — the leaf name is its last segment.
        let leaf = key.rsplit('.').next().unwrap_or(key);
        out.nodes
            .push(constant_node(leaf, key, path, line, Language::Properties));
    }
}

// =============================================================================
// Resolve
// =============================================================================

/// A config-key node: emitted by *this* framework, out of a yaml/properties file.
/// A `Constant` in a `.java` file is a `@Value` **bind** node, not a key — binding
/// one `@Value` to another would be a wrong edge, and a wrong edge is worse than
/// none.
fn is_config_node(n: &Node) -> bool {
    n.framework.as_deref() == Some(SPRING)
        && matches!(
            Language::from_wire(&n.language),
            Some(Language::Yaml | Language::Properties)
        )
}

fn config_nodes(ctx: &dyn ResolutionContext) -> Vec<Node> {
    crate::context::owned(ctx.nodes_by_kind(NodeKind::Constant))
        .into_iter()
        .filter(is_config_node)
        .collect()
}

/// `application.yml` is the base; `application-prod.yml` is a profile variant.
/// The base wins a tie — it is the one that always applies.
fn is_profile_variant(file: &str) -> bool {
    let basename = file.rsplit('/').next().unwrap_or(file);
    basename.starts_with("application-") || basename.starts_with("bootstrap-")
}

fn basename_len(file: &str) -> usize {
    file.rsplit('/').next().unwrap_or(file).len()
}

/// `@Value("${app.cacheList}")` → the `app.cache-list` key in `application.yml`.
///
/// Unique ⇒ **0.9** (this IS the key). Several files declare it ⇒ **0.75**, broken
/// by base-file-over-profile, then shorter basename, then path/line — never by
/// iteration order.
fn resolve_config_key(r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
    let want = canonical(&r.reference_name);
    let mut hits: Vec<Node> = config_nodes(ctx)
        .into_iter()
        .filter(|n| canonical(&n.qualified_name) == want)
        .collect();
    if hits.is_empty() {
        return None;
    }

    let confidence = if hits.len() == 1 { 0.9 } else { 0.75 };
    hits.sort_by(|a, b| {
        (
            is_profile_variant(&a.file_path),
            basename_len(&a.file_path),
            &a.file_path,
            a.start_line,
        )
            .cmp(&(
                is_profile_variant(&b.file_path),
                basename_len(&b.file_path),
                &b.file_path,
                b.start_line,
            ))
    });

    Some(ResolvedRef {
        original: r.clone(), // the STORED ROW, unmutated (#760)
        target_node_id: hits.first()?.id.clone(),
        confidence,
        resolved_by: ResolvedBy::Framework,
    })
}

/// `@ConfigurationProperties(prefix = "app")` → the **shortest** key under `app.`.
/// A prefix binds a whole subtree, so no single key is "the" target; the shortest
/// is the stable, cheapest anchor into that subtree.
fn resolve_prefix(
    r: &UnresolvedRef,
    prefix: &str,
    ctx: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let want = canonical(prefix);
    let mut hits: Vec<Node> = config_nodes(ctx)
        .into_iter()
        .filter(|n| canonical(&n.qualified_name).starts_with(&want))
        .collect();
    if hits.is_empty() {
        return None;
    }

    hits.sort_by(|a, b| {
        (
            canonical(&a.qualified_name).len(),
            &a.qualified_name,
            &a.file_path,
            a.start_line,
        )
            .cmp(&(
                canonical(&b.qualified_name).len(),
                &b.qualified_name,
                &b.file_path,
                b.start_line,
            ))
    });

    Some(ResolvedRef {
        original: r.clone(),
        target_node_id: hits.first()?.id.clone(),
        confidence: 0.85,
        resolved_by: ResolvedBy::Framework,
    })
}

const ENTITY_DIRS: &[&str] = &["/entity/", "/entities/", "/model/", "/models/", "/domain/"];

/// Spring's DI is invisible to a parser: a field is *injected*, never assigned, so
/// there is no `new ArticleService()` to follow. What is left is the naming
/// convention — and in Spring it is close to universal.
///
/// Confidence tracks how strong the convention is: a `*Service` suffix is a
/// contract (0.85); a bare PascalCase name might just be a class (0.70).
fn resolve_by_di_convention(r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
    let name = r.reference_name.as_str();

    if name.ends_with("Service") {
        return by_convention(
            r,
            ctx,
            &[NodeKind::Class, NodeKind::Interface],
            &["/service/"],
            0.85,
        );
    }
    if name.ends_with("Repository") {
        return by_convention(
            r,
            ctx,
            &[NodeKind::Class, NodeKind::Interface],
            &["/repository/"],
            0.85,
        );
    }
    if name.ends_with("Controller") {
        return by_convention(r, ctx, &[NodeKind::Class], &["/controller/"], 0.85);
    }
    if name.ends_with("Component") || name.ends_with("Config") {
        return by_convention(
            r,
            ctx,
            &[NodeKind::Class],
            &["/component/", "/components/", "/config/"],
            0.80,
        );
    }
    if is_pascal_case(name) {
        return by_convention(r, ctx, &[NodeKind::Class], ENTITY_DIRS, 0.70);
    }
    None
}

/// `Article` — yes. `articleService`, `ARTICLE`, `app.timeout` — no.
fn is_pascal_case(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().any(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn relaxed_binding_makes_the_three_spellings_one_key() {
        assert_eq!(canonical("app.cache-list"), "app.cachelist");
        assert_eq!(canonical("app.cacheList"), "app.cachelist");
        assert_eq!(canonical("app.cache_list"), "app.cachelist");
        // The dot is structure, not spelling — it must survive.
        assert_ne!(canonical("app.timeout"), canonical("apptimeout"));
    }

    #[test]
    fn join_path_collapses_slashes_and_bottoms_out_at_root() {
        assert_eq!(join_path("/articles", "/{slug}"), "/articles/{slug}");
        assert_eq!(join_path("/articles", ""), "/articles");
        assert_eq!(join_path("", "/health"), "/health");
        assert_eq!(join_path("", ""), "/");
        assert_eq!(join_path("/articles/", "/{slug}"), "/articles/{slug}");
    }

    #[test]
    fn annotation_path_ignores_a_non_path_argument() {
        assert_eq!(annotation_path(r#""/x""#).as_deref(), Some("/x"));
        assert_eq!(annotation_path(r#"value = "/x""#).as_deref(), Some("/x"));
        assert_eq!(annotation_path(r#"path = "/x""#).as_deref(), Some("/x"));
        // The deviation the doc comment describes: this is NOT a route path.
        assert_eq!(annotation_path(r#"produces = "application/json""#), None);
        assert_eq!(annotation_path(""), None);
    }

    #[test]
    fn a_bare_verb_mapping_still_matches() {
        let caps = VERB_MAPPING
            .captures("@GetMapping\n    public List<A> list() {")
            .unwrap();
        assert_eq!(&caps[1], "Get");
        assert!(
            caps.get(2).is_none(),
            "a bare @GetMapping has no argument list — and it is still a route, \
             on the class prefix. (The `\\s*` before the optional parens must not \
             be allowed to reach the `(` of `list()`.)"
        );
        // …and with parens, group 2 is the arguments WITHOUT them.
        let caps = VERB_MAPPING.captures(r#"@GetMapping("/{slug}")"#).unwrap();
        assert_eq!(&caps[2], r#""/{slug}""#);
    }

    #[test]
    fn is_pascal_case_accepts_an_entity_and_rejects_a_field_or_key() {
        assert!(is_pascal_case("Article"));
        assert!(!is_pascal_case("articleService"));
        assert!(!is_pascal_case("app.timeout"));
        assert!(!is_pascal_case("ARTICLE"));
    }
}
