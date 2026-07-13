#![allow(clippy::unwrap_used, clippy::expect_used)]
//! **THE Phase 3 gate — dispatch coverage.**
//!
//! The roadmap's gate for this phase is one sentence: *the dispatch-coverage
//! fixtures resolve end-to-end — no half-bridged flow.* This file is that sentence,
//! executable.
//!
//! # A flow is closed or it is a failure. There is no partial credit.
//!
//! Every row of [`FLOWS`] names an **entry point** (a route, addressed by its
//! semantics — never by a parsed id), a **terminal** (the symbol the agent was
//! ultimately looking for: the service call, the model method, the config key), and
//! the **hops in between**. The gate walks the persisted graph and demands the whole
//! chain, in order. A flow that resolves 3 of its 4 hops **FAILS** — per PRD §8.2 a
//! half-bridged flow is *worse* than no bridge, because it advertises a hop the agent
//! then has to Read to finish, which is the exact read-displacement this product
//! exists to prevent.
//!
//! # Three rules this table encodes, each learned the hard way
//!
//! **1. Gin's entry point is addressed by its UN-PREFIXED path.** `v1.POST("/articles")`
//! under `r.Group("/api/v1")` yields `route_path = "/articles"`, not
//! `/api/v1/articles` — the group's prefix lives at its *declaration*, arbitrarily far
//! from the registration, and joining them needs dataflow this pass does not have (TS
//! parity, carried deliberately; see `frameworks/go.rs`). So the Go row below asks for
//! `"/articles"`. **Do not "fix" this by loosening the route lookup** — a fuzzy lookup
//! would make the gate pass on a route it never actually found, which is the failure
//! this gate exists to catch.
//!
//! **2. A class-level fallback is never a `via` pin.** Laravel and Rails fall back to
//! the controller *class* when the action method cannot be found — honest (it lands the
//! agent in the right file) but strictly weaker than the action itself. If the gate
//! pinned the class, a resolver that lost the *action* would still pass, and a
//! half-bridged flow would ship. So every `via` below names the **action**, never its
//! class.
//!
//! **3. Spring's config bridge runs on the REAL pipeline.** It is the one
//! cross-language hop in the phase (`@Value("${app.greeting}")` in Java → a key in
//! `application.yml`), and it is exactly the hop where an extraction→store assumption
//! could be wrong while a `FakeContext` never noticed. It gets its own test, against
//! the real store.
//!
//! # This gate drives the PRODUCTION driver
//!
//! Every flow below runs through `resolve_and_persist_batched` — the same call an indexer
//! makes. A gate that composes its own pipeline proves the library works; only a gate that
//! drives the product proves the product runs. (This crate shipped four seams whose unit
//! tests passed while nothing called them, so the distinction is not academic.)
//!
//! # Completeness — no framework ships ungated
//!
//! [`every_registered_framework_is_gated`] keys on `all_framework_resolvers()`: a
//! framework registered without a row in [`FLOWS`] fails the gate. A gate you can
//! silently opt out of is not a gate.
//!
//! [`every_registered_synthesizer_is_gated`] does the same for the dispatch channels,
//! keyed on `registered_synthesizers()`. Both halves of the phase are now covered: a
//! framework OR a synthesizer that ships without a closed flow fails this gate.
//!
//! # The controls are half the proof
//!
//! Every positive assertion here is satisfied by a synthesizer that bridges
//! *everything*. Only a **control** — ordinary code containing none of the dispatch
//! shapes — catches one that guesses. [`synthesis_emits_nothing_on_the_controls`] is
//! that half, and it is the cheapest precision test that exists.

use selene_resolve::frameworks::all_framework_resolvers;
use selene_resolve::synth::registered_synthesizers;

mod pipeline;

/// One end-to-end flow: entry point → hops → terminal.
struct Flow {
    /// The fixture project under `tests/fixtures/dispatch/`.
    fixture: &'static str,
    framework: &'static str,
    /// `None` for a path-only router (django `path()`, react-router).
    method: Option<&'static str>,
    /// **As the framework stores it** — see rule 1 about Gin.
    path: &'static str,
    /// The symbol the agent was actually looking for.
    terminal: &'static str,
    /// Every hop between the route and the terminal, in order. Never a class that a
    /// resolver could have fallen back to — see rule 2.
    via: &'static [&'static str],
    /// A class-based dispatch (django's CBV, laravel/rails/aspnet/spring controllers)
    /// legitimately traverses `contains` from the class to its method. Everything else
    /// uses the strict kinds, so `contains` cannot become a false-green machine.
    class_dispatch: bool,
}

/// **The flow table.** One row per framework; a framework missing from here fails
/// `every_registered_framework_is_gated`.
const FLOWS: &[Flow] = &[
    Flow {
        fixture: "express",
        framework: "express",
        method: Some("POST"),
        path: "/users/login",
        terminal: "hashPassword",
        via: &["login"],
        class_dispatch: false,
    },
    Flow {
        fixture: "react",
        framework: "react",
        method: None,
        path: "/article/:slug",
        terminal: "fetchArticle",
        via: &["Article", "useArticle"],
        class_dispatch: false,
    },
    Flow {
        fixture: "django",
        framework: "django",
        method: None,
        path: "articles/<slug>/",
        terminal: "get_article",
        // `ArticleDetail` is a CBV: the route names the CLASS and the request is
        // answered by a METHOD of it. That hop genuinely is containment.
        via: &["ArticleDetail", "get"],
        class_dispatch: true,
    },
    Flow {
        fixture: "flask",
        framework: "flask",
        method: Some("POST"),
        path: "/articles",
        terminal: "create_article",
        via: &["create"],
        class_dispatch: false,
    },
    Flow {
        fixture: "fastapi",
        framework: "fastapi",
        method: Some("GET"),
        path: "/articles",
        terminal: "list_articles",
        via: &["index"],
        class_dispatch: false,
    },
    Flow {
        fixture: "spring",
        framework: "spring",
        method: Some("GET"),
        path: "/articles",
        terminal: "listArticles",
        via: &["getAll"],
        class_dispatch: true,
    },
    Flow {
        // ⚠ RULE 1: the group prefix is NOT part of the stored path. `/articles`,
        // never `/api/v1/articles`.
        fixture: "gin",
        framework: "go",
        method: Some("POST"),
        path: "/articles",
        terminal: "Create",
        via: &["CreateArticle"],
        class_dispatch: false,
    },
    Flow {
        fixture: "axum",
        framework: "rust",
        method: Some("POST"),
        path: "/articles",
        terminal: "create",
        via: &["create_article"],
        class_dispatch: false,
    },
    Flow {
        fixture: "laravel",
        framework: "laravel",
        method: Some("GET"),
        path: "/articles",
        terminal: "listArticles",
        // RULE 2: `index` (the ACTION), never `ArticleController` (the class the
        // resolver is allowed to fall back to).
        via: &["index"],
        class_dispatch: true,
    },
    Flow {
        fixture: "rails",
        framework: "rails",
        method: Some("GET"),
        path: "/articles",
        terminal: "recent",
        via: &["index"],
        class_dispatch: true,
    },
    Flow {
        fixture: "aspnet",
        framework: "aspnet",
        method: Some("GET"),
        path: "/api/articles",
        terminal: "ListAsync",
        via: &["GetAll"],
        class_dispatch: true,
    },
];

fn fixture_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dispatch")
        .join(name)
}

// =============================================================================
// THE GATE
// =============================================================================

/// Every flow, end to end. One failure fails the phase.
#[tokio::test(flavor = "multi_thread")]
async fn every_dispatch_flow_is_closed_end_to_end() {
    for flow in FLOWS {
        let p = pipeline::index_and_drive(&fixture_dir(flow.fixture)).await;

        let route = p.route(flow.framework, flow.method, flow.path).await;
        let kinds = if flow.class_dispatch {
            pipeline::CBV_FLOW_KINDS
        } else {
            pipeline::FLOW_KINDS
        };

        p.assert_flow_kinds(
            &route.id,
            flow.terminal,
            flow.via,
            kinds,
            &format!(
                "{} ({}): {} {} → {} → {}",
                flow.framework,
                flow.fixture,
                flow.method.unwrap_or(""),
                flow.path,
                flow.via.join(" → "),
                flow.terminal
            ),
        )
        .await;
    }
}

/// **Spring's config bridge, on the real pipeline** — the one cross-language hop in
/// the phase.
///
/// `@Value("${app.greeting}")` in Java must land on the `app.greeting` key in
/// `application.yml`. This is the hop most likely to half-bridge in silence: the Java
/// side resolves fine on its own, the config side simply dead-ends, and **nothing in a
/// same-language gate notices**. It was asserted through a `FakeContext` until now —
/// which is exactly the seam that hid three inert loaders from us for a whole phase.
#[tokio::test(flavor = "multi_thread")]
async fn spring_config_bridge_closes_on_the_real_pipeline() {
    use selene_core::NodeKind;

    let p = pipeline::index_and_drive(&fixture_dir("spring")).await;

    // The yaml key is a node at all — the file-level-only language actually indexed.
    let constants = p.nodes_of_kind(NodeKind::Constant).await;
    let key = constants
        .iter()
        .find(|n| n.qualified_name == "app.greeting")
        .unwrap_or_else(|| {
            panic!(
                "no `app.greeting` config node. The yaml side of the bridge does not \
                 exist, so every @Value dangles.\n  constants: {:?}",
                constants
                    .iter()
                    .map(|n| n.qualified_name.as_str())
                    .collect::<Vec<_>>()
            )
        });

    // #383 — the VALUE is never stored. The bridge must not carry the secret across.
    let secret = constants
        .iter()
        .find(|n| n.qualified_name == "app.db.password")
        .expect("the password KEY is indexed");
    for field in [
        &secret.name,
        &secret.qualified_name,
        secret.signature.as_deref().unwrap_or_default(),
        secret.docstring.as_deref().unwrap_or_default(),
    ] {
        assert!(
            !field.contains("s3cr3t"),
            "a config VALUE leaked into the graph — the index would hand secrets to \
             any agent that reads a node"
        );
    }

    // And the bridge is CLOSED: something points at the key.
    let sources = p.sources_of(&key.id).await;
    assert!(
        !sources.is_empty(),
        "NOTHING points at `app.greeting`. The @Value bind node dangles — the agent \
         tracing 'where does this value come from?' lands on the annotation and has \
         to open application.yml itself, which is the half-bridge the invariant \
         forbids."
    );
}

/// **No framework ships ungated.** A resolver registered without a row in [`FLOWS`]
/// fails here — a gate you can silently opt out of is not a gate.
#[test]
fn every_registered_framework_is_gated() {
    let registered: Vec<&str> = all_framework_resolvers().iter().map(|f| f.name()).collect();
    let gated: Vec<&str> = FLOWS.iter().map(|f| f.framework).collect();

    let ungated: Vec<&&str> = registered.iter().filter(|f| !gated.contains(f)).collect();
    assert!(
        ungated.is_empty(),
        "these frameworks are REGISTERED but have no flow in the coverage gate: {ungated:?}\n\
         Every framework must prove a closed flow, or it is shipping on the strength of \
         its unit tests alone."
    );
}

/// One synthesized dispatch flow: a registration site → the bridged hop → the terminal.
struct SynthFlow {
    /// The channel, as `registered_synthesizers()` names it.
    channel: &'static str,
    fixture: &'static str,
    /// The node an agent starts from (the dispatcher — `setState`, `emit`, the mutator).
    from: &'static str,
    /// What it must reach.
    terminal: &'static str,
    /// The hops between, in order. The SYNTHESIZED hop is one of these — the chain is
    /// deliberately longer than the bridge, so "the hop works" cannot be mistaken for
    /// "the flow closes".
    via: &'static [&'static str],
}

/// **The synthesizer flow table.** A channel missing from here fails
/// [`every_registered_synthesizer_is_gated`].
const SYNTH_FLOWS: &[SynthFlow] = &[
    SynthFlow {
        channel: "callback",
        fixture: "callback",
        from: "mutateElement",
        terminal: "renderScene",
        via: &["triggerUpdate", "triggerRender"],
    },
    SynthFlow {
        channel: "event-emitter",
        fixture: "event",
        from: "use",
        terminal: "initApp",
        via: &["onmount"],
    },
    SynthFlow {
        // react-render and jsx-render are ONE flow on purpose: shipping the re-render
        // channel without the JSX hop measurably RAISED agent reads (the flow reached
        // `render` and stopped, which is a half-bridge). The chain below traverses both.
        channel: "react-render",
        fixture: "react-render",
        from: "handleClick",
        terminal: "renderStaticScene",
        via: &["render", "StaticCanvas"],
    },
    SynthFlow {
        channel: "jsx-render",
        fixture: "react-render",
        from: "handleClick",
        terminal: "renderStaticScene",
        via: &["render", "StaticCanvas"],
    },
];

/// Every synthesized flow, end to end, on the real pipeline.
#[tokio::test(flavor = "multi_thread")]
async fn every_synthesized_flow_is_closed_end_to_end() {
    for flow in SYNTH_FLOWS {
        let p = pipeline::index_and_drive(&fixture_dir(flow.fixture)).await;
        let from = p.node_named(flow.from).await;

        p.assert_flow(
            &from.id,
            flow.terminal,
            flow.via,
            &format!(
                "{} ({}): {} → {} → {}",
                flow.channel,
                flow.fixture,
                flow.from,
                flow.via.join(" → "),
                flow.terminal
            ),
        )
        .await;
    }
}

/// **No synthesizer ships ungated** — keyed on `registered_synthesizers()`, exactly as
/// the framework half is keyed on `all_framework_resolvers()`.
///
/// This is the assertion that replaced `the_synthesizer_half_of_this_gate_is_not_built`.
/// It is not a formality: a channel registered without a flow row is a channel whose
/// only evidence is its own unit test, and this crate has now shipped THREE seams whose
/// unit tests passed while nothing in the pipeline called them.
#[test]
fn every_registered_synthesizer_is_gated() {
    let registered = registered_synthesizers();
    let gated: Vec<&str> = SYNTH_FLOWS.iter().map(|f| f.channel).collect();

    let ungated: Vec<&&str> = registered.iter().filter(|c| !gated.contains(c)).collect();
    assert!(
        ungated.is_empty(),
        "these synthesizers are REGISTERED but have no closed flow in the coverage \
         gate: {ungated:?}\n\
         A dispatch channel with no end-to-end flow is a channel that has only ever \
         been proven against a fake."
    );
}

/// **The precision half.** Synthesis must emit **exactly zero** edges on code that
/// contains none of the dispatch shapes.
///
/// Every positive assertion in this file is satisfied by a synthesizer that bridges
/// everything in sight. Only a control fails such a synthesizer — and a channel that
/// guesses is far worse than one that misses, because a wrong dispatch edge is a
/// confident lie about how the program runs.
#[tokio::test(flavor = "multi_thread")]
async fn synthesis_emits_nothing_on_the_controls() {
    use selene_core::Provenance;

    for control in ["callback-control", "event-control", "django-orm-control"] {
        let p = pipeline::index_and_drive(&fixture_dir(control)).await;
        let synthesized = p.synthesized_edges().await;

        assert!(
            synthesized.is_empty(),
            "{control}: synthesis invented {} edge(s) in code that contains NO dispatch \
             shape:\n  {:?}\n\
             A channel that guesses is worse than one that misses — a wrong dispatch \
             edge is a confident lie about how the program runs.",
            synthesized.len(),
            synthesized
                .iter()
                .map(|e| format!(
                    "{} -> {} [{:?}]",
                    e.source,
                    e.target,
                    e.provenance.unwrap_or(Provenance::TreeSitter)
                ))
                .collect::<Vec<_>>()
        );
    }
}
