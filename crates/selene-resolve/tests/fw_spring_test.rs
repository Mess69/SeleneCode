#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 16 — Spring (Java + Kotlin routes, config keys, relaxed binding, DI).
//!
//! # The invariant these tests exist to enforce
//!
//! **Dispatch coverage is end-to-end or not at all** (PRD §8.2). Spring has two
//! flows, and each is asserted hop by hop, never partially:
//!
//! ```text
//! route(GET /articles/{slug}) → getBySlug() → articleService.findBySlug()
//!         hop 1 (framework)      hop 2 (Part A: field-type inference + validation)
//!
//! @Value("${app.cacheList}") → application.yml : app.cache-list
//!         hop 1 (framework: relaxed binding)
//! ```
//!
//! The config bridge is *the* hop that would otherwise half-bridge: an agent
//! asking "where does this timeout come from?" must land ON the key in
//! `application.yml`. Landing on the `@Value` and no further is worse than
//! nothing — it sends the agent back to reading files with extra steps.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::frameworks::java::Spring;
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};
use selene_resolve::{ReferenceResolver, ResolvedBy};

// =============================================================================
// Fixtures
// =============================================================================

const CONTROLLER_JAVA: &str = "\
package com.example.controller;

@RestController
@RequestMapping(\"/articles\")
public class ArticleController {

    private final ArticleService articleService;

    @GetMapping(\"/{slug}\")
    public ResponseEntity<Article> getBySlug(@PathVariable String slug) {
        return ResponseEntity.ok(articleService.findBySlug(slug));
    }

    @GetMapping
    public List<Article> list() {
        return articleService.findAll();
    }
}
";

const CONTROLLER_KT: &str = "\
package com.example.controller

@RestController
@RequestMapping(\"/articles\")
class ArticleController(
    private val articleService: ArticleService,
) {
    @GetMapping
    fun getBySlug(@PathVariable slug: String): Article =
        articleService.findBySlug(slug)
}
";

const APPLICATION_YML: &str = "\
app:
  cache-list: true
  timeout: 30
  db:
    password: s3cr3t
spring:
  datasource:
    url: jdbc:postgresql://localhost/db
";

const CACHE_CONFIG_JAVA: &str = "\
package com.example.config;

@Component
public class CacheConfig {
    @Value(\"${app.cacheList}\")
    private boolean cacheList;
}
";

const CTRL: &str = "src/main/java/com/example/controller/ArticleController.java";
const CTRL_KT: &str = "src/main/kotlin/com/example/controller/ArticleController.kt";
const SVC: &str = "src/main/java/com/example/service/ArticleService.java";
const SVC_KT: &str = "src/main/kotlin/com/example/service/ArticleService.kt";
const CFG: &str = "src/main/java/com/example/config/CacheConfig.java";
const YML: &str = "src/main/resources/application.yml";

fn jvm_method(id: &str, ty: &str, name: &str, file: &str, lang: Language) -> Node {
    node(
        id,
        NodeKind::Method,
        name,
        &format!("{ty}::{name}"),
        file,
        lang,
    )
}

fn jvm_class(id: &str, name: &str, file: &str, lang: Language) -> Node {
    node(id, NodeKind::Class, name, name, file, lang)
}

fn a_ref(from: &str, name: &str, kind: &str, file: &str, lang: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.into(),
        reference_name: name.into(),
        reference_kind: kind.into(),
        line: Some(11),
        column: Some(8),
        candidates: vec![],
        file_path: file.into(),
        language: lang.as_str().into(),
        status: RefStatus::Pending,
        name_tail: name.rsplit('.').next().unwrap_or(name).into(),
    }
}

/// A `pom.xml` that makes the project a Spring project.
///
/// **Not boilerplate.** `ReferenceResolver::new` runs `detect` once, at
/// construction, and only resolvers that fire there are ever consulted. A context
/// holding nothing but `application.yml` is not a Spring project, and Spring is
/// correctly inert in it — which is exactly how the first draft of these tests
/// went green on the route flow while the config bridge silently resolved
/// nothing. Detection is asserted on its own, below.
const POM: &str = "<dependency><groupId>org.springframework.boot</groupId></dependency>";

/// Index a project the way the real pipeline does: extract the framework nodes
/// FIRST, then build the context over everything.
///
/// This ordering is not cosmetic. A `@Value` reference is named after the key
/// (`app.cacheList`), and the only node declaring that name is the bind node this
/// very pass emits — so a context built *before* the pass has no such name, and
/// `resolve_one`'s pre-filter drops the reference before Spring is ever asked.
/// Part C's batch driver must construct its `StoreContext` after
/// `run_framework_extract`, and this helper is that contract in miniature.
fn indexed(files: &[(&str, &str, Language)], ctx: FakeContext) -> (FakeContext, Vec<Node>) {
    let mut ctx = ctx.with_file("pom.xml", POM);
    let mut emitted = Vec::new();
    for (path, src, lang) in files {
        ctx = ctx.with_file(path, src);
        let out = Spring.extract(path, src, *lang);
        for n in out.nodes {
            ctx = ctx.with_node(n.clone());
            emitted.push(n);
        }
    }
    (ctx, emitted)
}

fn refs_of(path: &str, src: &str, lang: Language) -> Vec<UnresolvedRef> {
    Spring.extract(path, src, lang).refs
}

// =============================================================================
// Flow 1 — Java: route → handler → service
// =============================================================================

/// **Flow closed ⇔** `GET /articles/{slug}` → `getBySlug()` → `ArticleService.findBySlug()`.
#[test]
fn spring_java_flow_route_to_handler_to_service() {
    let out = Spring.extract(CTRL, CONTROLLER_JAVA, Language::Java);

    // --- hop 1: the routes ----------------------------------------------------
    assert_eq!(out.nodes.len(), 2, "one route per verb annotation");
    let slug = &out.nodes[0];
    assert_eq!(slug.kind, NodeKind::Route);
    assert_eq!(
        slug.name, "GET /articles/{slug}",
        "the class-level @RequestMapping is a PREFIX, joined onto the method's path"
    );
    assert_eq!(slug.route_method.as_deref(), Some("GET"));
    assert_eq!(slug.route_path.as_deref(), Some("/articles/{slug}"));
    assert_eq!(slug.framework.as_deref(), Some("spring"));
    assert_eq!(slug.start_line, 9);

    let list = &out.nodes[1];
    assert_eq!(
        list.name, "GET /articles",
        "a BARE @GetMapping is still a route — on the class prefix alone. Without \
         this arm a multi-method controller (the dominant Spring shape) has no \
         routes at all."
    );

    // Two routes, two handler refs — and they point at DIFFERENT handlers.
    let handlers: Vec<&str> = out.refs.iter().map(|r| r.reference_name.as_str()).collect();
    assert_eq!(handlers, vec!["getBySlug", "list"]);

    // --- hop 1 resolves: route → handler --------------------------------------
    let ctx = FakeContext::new()
        .with_file(CTRL, CONTROLLER_JAVA)
        .with_node(jvm_class(
            "class:ArticleController",
            "ArticleController",
            CTRL,
            Language::Java,
        ))
        .with_node(jvm_method(
            "method:getBySlug",
            "ArticleController",
            "getBySlug",
            CTRL,
            Language::Java,
        ))
        .with_node(jvm_class(
            "class:ArticleService",
            "ArticleService",
            SVC,
            Language::Java,
        ))
        .with_node(jvm_method(
            "method:findBySlug",
            "ArticleService",
            "findBySlug",
            SVC,
            Language::Java,
        ))
        .with_node(slug.clone())
        .with_node(list.clone());

    let mut resolver = ReferenceResolver::new(ctx);
    let hop1 = resolver
        .resolve_one(&out.refs[0])
        .expect("hop 1: the route binds to its handler method");
    assert_eq!(hop1.target_node_id, "method:getBySlug");

    // --- hop 2 resolves: handler body → the injected service -------------------
    // `articleService` is never assigned — it is INJECTED. Part A recovers its
    // type from the field declaration, then VALIDATES that `findBySlug` actually
    // exists on `ArticleService`; 0.9 is that validated claim, and asserting it
    // (not merely the target id) is what stops a 0.7 name fallback from quietly
    // covering for broken inference.
    let hop2 = resolver
        .resolve_one(&a_ref(
            "method:getBySlug",
            "articleService.findBySlug",
            "calls",
            CTRL,
            Language::Java,
        ))
        .expect("hop 2: the handler's call binds to the service method");
    assert_eq!(hop2.target_node_id, "method:findBySlug");
    assert_eq!(
        hop2.confidence, 0.9,
        "0.9 = 'I know the receiver's type'; 0.7 would mean the name fallback \
         resolved it and the field-type inference did nothing"
    );

    // THE FLOW IS CLOSED: route → handler → service, every hop asserted.
}

/// The Kotlin twin. Same controller, same flow, a **bare** mapping — and the type
/// comes from a constructor parameter rather than a field.
#[test]
fn spring_kotlin_flow_bare_mapping_joins_the_class_prefix() {
    let out = Spring.extract(CTRL_KT, CONTROLLER_KT, Language::Kotlin);
    assert_eq!(out.nodes.len(), 1);
    assert_eq!(out.nodes[0].name, "GET /articles");
    assert_eq!(out.refs[0].reference_name, "getBySlug");

    let ctx = FakeContext::new()
        .with_file(CTRL_KT, CONTROLLER_KT)
        .with_node(jvm_method(
            "method:kt:getBySlug",
            "ArticleController",
            "getBySlug",
            CTRL_KT,
            Language::Kotlin,
        ))
        .with_node(jvm_method(
            "method:kt:findBySlug",
            "ArticleService",
            "findBySlug",
            SVC_KT,
            Language::Kotlin,
        ))
        .with_node(out.nodes[0].clone());

    let mut resolver = ReferenceResolver::new(ctx);
    let hop1 = resolver.resolve_one(&out.refs[0]).expect("route → handler");
    assert_eq!(hop1.target_node_id, "method:kt:getBySlug");

    let hop2 = resolver
        .resolve_one(&a_ref(
            "method:kt:getBySlug",
            "articleService.findBySlug",
            "calls",
            CTRL_KT,
            Language::Kotlin,
        ))
        .expect("handler → service");
    assert_eq!(hop2.target_node_id, "method:kt:findBySlug");
    assert_eq!(hop2.confidence, 0.9);
}

// =============================================================================
// Flow 2 — the config bridge
// =============================================================================

/// **Flow closed ⇔** `@Value("${app.cacheList}")` lands **on the key** in
/// `application.yml` — which is spelled `cache-list` there.
///
/// This is Spring's relaxed binding: the framework itself binds `cache-list`,
/// `cacheList` and `cache_list` to the same property, so the graph must too.
#[test]
fn config_bridge_value_annotation_lands_on_the_yaml_key() {
    let (ctx, emitted) = indexed(
        &[
            (YML, APPLICATION_YML, Language::Yaml),
            (CFG, CACHE_CONFIG_JAVA, Language::Java),
        ],
        FakeContext::new(),
    );

    // The yaml produced one Constant per LEAF key — and `app:` / `db:` (parents)
    // produced none.
    let keys: Vec<&str> = emitted
        .iter()
        .filter(|n| n.file_path == YML)
        .map(|n| n.qualified_name.as_str())
        .collect();
    assert_eq!(
        keys,
        vec![
            "app.cache-list",
            "app.timeout",
            "app.db.password",
            "spring.datasource.url"
        ],
        "leaf keys only, dotted through their ancestors"
    );

    let key_node = emitted
        .iter()
        .find(|n| n.qualified_name == "app.cache-list")
        .unwrap();

    // The `@Value` reference, exactly as the extract pass emitted it.
    let value_ref = refs_of(CFG, CACHE_CONFIG_JAVA, Language::Java)
        .into_iter()
        .find(|r| r.reference_name == "app.cacheList")
        .expect("the @Value bind node emits a reference named after the key");

    let mut resolver = ReferenceResolver::new(ctx);
    let hit = resolver.resolve_one(&value_ref).expect(
        "the @Value MUST land on the key — a bridge that stops here is \
                 worse than no bridge at all",
    );

    assert_eq!(hit.target_node_id, key_node.id);
    assert_eq!(hit.resolved_by, ResolvedBy::Framework);
    assert_eq!(
        hit.confidence, 0.9,
        "one file declares this key ⇒ this IS the key, not a guess"
    );
}

/// #383 — **a config value is never stored.** The node carries the key; nothing
/// on it carries the secret.
#[test]
fn config_values_are_never_stored() {
    let out = Spring.extract(YML, APPLICATION_YML, Language::Yaml);
    let secret = out
        .nodes
        .iter()
        .find(|n| n.qualified_name == "app.db.password")
        .expect("the key itself is indexed");

    // Every string-bearing field on the node, exhaustively.
    let mut carried = vec![
        secret.id.clone(),
        secret.name.clone(),
        secret.qualified_name.clone(),
        secret.file_path.clone(),
        secret.language.clone(),
    ];
    carried.extend(secret.docstring.clone());
    carried.extend(secret.signature.clone());
    carried.extend(secret.return_type.clone());
    carried.extend(secret.decorators.clone());
    carried.extend(secret.type_parameters.clone());
    carried.extend(secret.route_method.clone());
    carried.extend(secret.route_path.clone());
    carried.extend(secret.framework.clone());

    for field in &carried {
        assert!(
            !field.contains("s3cr3t"),
            "a config VALUE leaked into the graph via {field:?} — the index would \
             then hand secrets to any agent that reads a node"
        );
    }
    assert_eq!(secret.name, "password");
    assert_eq!(
        secret.start_line, 5,
        "the key's line, so an agent can open it"
    );
}

/// #1180 — **a `calls` reference never resolves to a config key.**
///
/// The pair is the point: the same name, one hop apart in kind, resolves for a
/// `references` ref and must NOT resolve for a `calls` ref. `service.timeout(…)`
/// is a method call, not the `app.timeout` property.
#[test]
fn a_calls_ref_never_resolves_to_a_config_key_but_a_references_ref_does() {
    let (ctx, emitted) = indexed(
        &[(YML, APPLICATION_YML, Language::Yaml)],
        FakeContext::new().with_node(node(
            "method:doWork",
            NodeKind::Method,
            "doWork",
            "Worker::doWork",
            CTRL,
            Language::Java,
        )),
    );
    let timeout = emitted
        .iter()
        .find(|n| n.qualified_name == "app.timeout")
        .unwrap();

    let mut resolver = ReferenceResolver::new(ctx);

    // `references` — the legitimate bridge.
    let ok = resolver
        .resolve_one(&a_ref(
            "method:doWork",
            "app.timeout",
            "references",
            CTRL,
            Language::Java,
        ))
        .expect("a `references` ref DOES reach the key index");
    assert_eq!(ok.target_node_id, timeout.id);
    assert_eq!(ok.confidence, 0.9);

    // `calls` — the gate.
    let call = resolver.resolve_one(&a_ref(
        "method:doWork",
        "app.timeout",
        "calls",
        CTRL,
        Language::Java,
    ));
    assert!(
        call.is_none() || call.as_ref().unwrap().target_node_id != timeout.id,
        "a `calls` ref bound to a yaml key — #1180. It was a perf catastrophe (a \
         dotted call scanning every constant in the repo) and it is a precision \
         bug: a method call is not a property."
    );

    // …and the resolver is not merely returning None for everything: the unit-level
    // gate is where the decision is actually made.
    assert!(
        Spring
            .resolve(
                &a_ref(
                    "method:doWork",
                    "app.timeout",
                    "calls",
                    CTRL,
                    Language::Java
                ),
                &FakeContext::new()
            )
            .is_none()
    );
}

/// Two files declare the same key. The base file wins, and the confidence drops
/// to 0.75 — because now it *is* a choice.
#[test]
fn a_profile_variant_loses_the_tie_to_the_base_file() {
    let prod = "src/main/resources/application-prod.yml";
    let (ctx, emitted) = indexed(
        &[
            (YML, "app:\n  timeout: 30\n", Language::Yaml),
            (prod, "app:\n  timeout: 5\n", Language::Yaml),
        ],
        FakeContext::new(),
    );
    assert_eq!(emitted.len(), 2, "both files declare the key");

    let base = emitted.iter().find(|n| n.file_path == YML).unwrap();

    let mut resolver = ReferenceResolver::new(ctx);
    let hit = resolver
        .resolve_one(&a_ref(
            "method:doWork",
            "app.timeout",
            "references",
            CTRL,
            Language::Java,
        ))
        .expect("still resolves — an ambiguous key is not an absent one");

    assert_eq!(
        hit.target_node_id, base.id,
        "application.yml over application-prod.yml: the base file is the one that \
         always applies"
    );
    assert_eq!(hit.confidence, 0.75, "a tie is a choice, and it says so");
}

/// `@ConfigurationProperties(prefix = "app")` → the subtree's anchor key.
///
/// The reference is named `app:prefix`, which names **no declared symbol
/// anywhere** — so this test also proves `claims_reference` is wired: without it
/// `resolve_one`'s step-3 pre-filter drops the ref and the whole
/// `@ConfigurationProperties` bridge is inert.
#[test]
fn configuration_properties_prefix_resolves_to_the_shortest_key_under_it() {
    let props_java = "\
@ConfigurationProperties(prefix = \"app\")
public class AppProps {
}
";
    let (ctx, emitted) = indexed(
        &[
            (YML, APPLICATION_YML, Language::Yaml),
            (
                "src/main/java/com/example/AppProps.java",
                props_java,
                Language::Java,
            ),
        ],
        FakeContext::new(),
    );

    let prefix_ref = refs_of(
        "src/main/java/com/example/AppProps.java",
        props_java,
        Language::Java,
    )
    .into_iter()
    .find(|r| r.reference_name == "app:prefix")
    .expect("the @ConfigurationProperties bind node names the prefix");

    assert!(
        Spring.claims_reference("app:prefix"),
        "`app:prefix` names nothing — unclaimed, the pre-filter drops it"
    );

    let timeout = emitted
        .iter()
        .find(|n| n.qualified_name == "app.timeout")
        .unwrap();

    let mut resolver = ReferenceResolver::new(ctx);
    let hit = resolver
        .resolve_one(&prefix_ref)
        .expect("the prefix binds into its subtree");
    assert_eq!(
        hit.target_node_id, timeout.id,
        "the SHORTEST key under `app.` — a stable anchor into the subtree"
    );
    assert_eq!(hit.confidence, 0.85);
    assert_eq!(hit.resolved_by, ResolvedBy::Framework);
}

// =============================================================================
// Units — the arms each flow leans on
// =============================================================================

#[test]
fn method_level_request_mapping_reads_its_verb_else_any() {
    let src = "\
@RequestMapping(\"/admin\")
public class AdminController {
    @RequestMapping(value = \"/purge\", method = RequestMethod.POST)
    public void purge() {}

    @RequestMapping(\"/status\")
    public void status() {}
}
";
    let out = Spring.extract("Admin.java", src, Language::Java);
    let names: Vec<&str> = out.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["POST /admin/purge", "ANY /admin/status"],
        "no `method=` ⇒ ANY. The class-level @RequestMapping is NOT one of these — \
         it is the prefix, and emitting it as a route would be a route to nothing."
    );
    // Both routes found their handler.
    let handlers: Vec<&str> = out.refs.iter().map(|r| r.reference_name.as_str()).collect();
    assert_eq!(handlers, vec!["purge", "status"]);
}

/// Annotations stack between the mapping and the method. The handler is the next
/// **signature**, not the next line.
#[test]
fn stacked_annotations_do_not_hide_the_handler() {
    let src = "\
@RestController
public class SecureController {
    @GetMapping(\"/secret\")
    @PreAuthorize(\"hasRole('ADMIN')\")
    @ResponseStatus(HttpStatus.OK)
    public Secret reveal() { return null; }
}
";
    let out = Spring.extract("Secure.java", src, Language::Java);
    assert_eq!(out.nodes[0].name, "GET /secret");
    assert_eq!(
        out.refs[0].reference_name, "reveal",
        "@PreAuthorize(\"…\") has parens and a string, and it is still not a handler"
    );
}

#[test]
fn properties_files_are_config_too_and_a_non_config_yaml_is_ignored() {
    let out = Spring.extract(
        "src/main/resources/application-prod.properties",
        "# a comment\napp.cache-list=true\nserver.port: 8080\n",
        Language::Properties,
    );
    let keys: Vec<&str> = out
        .nodes
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    assert_eq!(keys, vec!["app.cache-list", "server.port"]);
    assert_eq!(
        out.nodes[0].name, "cache-list",
        "the leaf is the node's name"
    );

    // A `docker-compose.yml` is not Spring config. Indexing every yaml key in a
    // repo turns the key index into noise.
    let noise = Spring.extract(
        "docker-compose.yml",
        "services:\n  db:\n    image: x\n",
        Language::Yaml,
    );
    assert!(noise.nodes.is_empty());
}

/// The DI conventions are a **candidate**, not a verdict: they compete at step 7
/// with the name matcher below them. Tested at the unit level, which is where the
/// convention itself lives.
#[test]
fn di_conventions_prefer_the_conventional_directory() {
    let decoy = "src/main/java/com/example/legacy/ArticleService.java";
    let ctx = FakeContext::new()
        .with_node(jvm_class(
            "class:decoy",
            "ArticleService",
            decoy,
            Language::Java,
        ))
        .with_node(jvm_class(
            "class:real",
            "ArticleService",
            SVC,
            Language::Java,
        ));

    let hit = Spring
        .resolve(
            &a_ref(
                "class:ArticleController",
                "ArticleService",
                "references",
                CTRL,
                Language::Java,
            ),
            &ctx,
        )
        .expect("a *Service name is a Spring bean");
    assert_eq!(
        hit.target_node_id, "class:real",
        "`/service/` breaks the tie — a field is INJECTED, never assigned, so the \
         naming convention is the only signal a parser has"
    );
    assert_eq!(hit.confidence, 0.85);

    // A bare PascalCase name is a weaker claim, and says so.
    let entity = FakeContext::new().with_node(jvm_class(
        "class:Article",
        "Article",
        "src/main/java/com/example/domain/Article.java",
        Language::Java,
    ));
    let hit = Spring
        .resolve(
            &a_ref(
                "class:ArticleController",
                "Article",
                "references",
                CTRL,
                Language::Java,
            ),
            &entity,
        )
        .unwrap();
    assert_eq!(hit.confidence, 0.70);
}

#[test]
fn detection_from_a_build_file_or_from_the_annotations_alone() {
    let by_manifest = FakeContext::new().with_file(
        "pom.xml",
        "<dependency><groupId>org.springframework.boot</groupId></dependency>",
    );
    assert!(Spring.detect(&by_manifest));

    // A vendored module with no build file: the annotations are the signal.
    let by_annotation = FakeContext::new().with_file(CTRL, CONTROLLER_JAVA);
    assert!(Spring.detect(&by_annotation));

    let plain_java = FakeContext::new().with_file("Main.java", "public class Main {}");
    assert!(!Spring.detect(&plain_java));
}

#[test]
fn spring_declares_the_config_languages_and_sits_in_registry_order() {
    let langs = Spring.languages().unwrap();
    for l in [
        Language::Java,
        Language::Kotlin,
        Language::Yaml,
        Language::Properties,
    ] {
        assert!(
            langs.contains(&l),
            "{l:?} missing — without yaml/properties, `extract` never runs on \
             application.yml, the keys are never nodes, and every @Value dangles"
        );
    }

    // Position, not the whole list: Tasks 12–20 each append a resolver, and a test
    // that pins the full vector would redden on every one of them for no reason.
    // What matters is that spring is registered, and that registry order (which IS
    // resolve precedence) matches REGISTRY_ORDER's declaration.
    let names: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    let spring = names.iter().position(|n| *n == "spring");
    assert!(spring.is_some(), "spring is registered");
    assert!(
        names.iter().position(|n| *n == "fastapi") < spring,
        "REGISTRY_ORDER declares fastapi before spring"
    );
}
