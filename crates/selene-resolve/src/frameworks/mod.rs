//! The framework registry (Task 11) — the seam every framework resolver plugs
//! into, and the pass that emits their route/config nodes.
//!
//! # Data-driven, not a pile of `if lang == …`
//!
//! A framework is a [`FrameworkResolver`] impl and a row in [`REGISTRY_ORDER`].
//! Adding one is adding a file and a row; the resolver core never names a
//! framework. (This mirrors the TS build's shape, and it is what keeps
//! `resolve_one`'s ladder readable as the phase grows to eleven frameworks.)
//!
//! # Registry order IS resolve precedence
//!
//! [`all_framework_resolvers`] returns a **stable, ordered** slice, never a
//! `HashMap`'s iteration order. `resolve_one` walks it in order and the first
//! result with confidence ≥ 0.9 short-circuits, so a reordering is a silent
//! behavior change. [`REGISTRY_ORDER`] is the one place that order is declared.
//!
//! # Two passes, and they are not the same pass
//!
//! - [`run_framework_extract`] — **emission**. Walks the already-indexed files
//!   and asks each detected framework for the route/config nodes it can see in
//!   the source. Runs **before** resolution: a reference cannot bind to a route
//!   node that does not exist yet.
//! - [`run_post_extract`] — **cross-file finalize**, after everything else.
//!
//! # Why emission lives here and not in `selene-extract` (decision D2)
//!
//! The pipeline is extract → resolve. Putting a framework hook *inside* the
//! extractor would make `selene-extract` depend on `selene-resolve` — backwards
//! layering, and a literal dependency cycle. So route emission is a
//! `selene-resolve` pass **over the already-indexed files**, reading source
//! through [`ResolutionContext::read_file`]. The extractor never learns that
//! frameworks exist.
//!
//! # Errors are collected, never thrown
//!
//! A framework that panics in `detect` or `extract` is caught, contributes a
//! warning, and is skipped. One broken resolver must never fail an index — the
//! blast radius of a bad regex is one framework, not the whole graph.

pub mod routes;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;

use selene_core::{Language, Node, UnresolvedRef};
use selene_db::GraphStore;

use crate::Result;
use crate::context::ResolutionContext;
use crate::types::ResolvedRef;

pub use routes::{RouteSpec, find_route, route_node, route_node_in};

/// Nodes + references a framework found in one file.
#[derive(Debug, Default, Clone)]
pub struct FrameworkExtraction {
    /// Route/config nodes (`NodeKind::Route`, `NodeKind::Constant`, …).
    pub nodes: Vec<Node>,
    /// References the framework wants resolved later (route → handler).
    pub refs: Vec<UnresolvedRef>,
}

/// What [`run_framework_extract`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FrameworkExtractStats {
    /// Nodes inserted.
    pub nodes: u64,
    /// Unresolved references inserted.
    pub refs: u64,
    /// One per framework×file that failed. Never fatal.
    pub warnings: Vec<String>,
}

/// A framework's knowledge: how to spot it, what nodes it can see in a file,
/// and how to bind the references it emits.
///
/// Every method has a default except [`FrameworkResolver::name`],
/// [`FrameworkResolver::languages`], [`FrameworkResolver::detect`] and
/// [`FrameworkResolver::resolve`] — a framework that only emits routes
/// implements `extract` and leaves `resolve` returning `None`, and vice versa.
pub trait FrameworkResolver: Send + Sync {
    /// Stable identifier — `"express"`. It is written into every node this
    /// framework emits (`Node::framework`) and is the key for
    /// [`framework_resolver`].
    fn name(&self) -> &'static str;

    /// Languages this framework applies to. `None` = **all** languages (in the
    /// TS build only `vue` does that, via path-only extraction; unused in v0).
    fn languages(&self) -> Option<&'static [Language]>;

    /// Is this framework present in the project? Evaluated **once per index**,
    /// not per file (it reads manifests and probes paths — doing it per file
    /// would be quadratic).
    fn detect(&self, ctx: &dyn ResolutionContext) -> bool;

    /// Bind one reference. `None` = "not mine" — never a guess.
    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef>;

    /// Opt a name past the resolver's "no such symbol exists" pre-filter.
    ///
    /// Without this hook a reference whose name matches no declared symbol is
    /// dropped **before** `resolve` is ever called — which silently deletes
    /// every rails route (`articles#index` names nothing), every laravel route
    /// (`UserController@index`), and django's `_iterable_class`. It is the
    /// single most easily-missed part of this trait; the TS build shipped the
    /// bug twice.
    fn claims_reference(&self, _name: &str) -> bool {
        false
    }

    /// The route/config nodes visible in one file's source. Runs in the
    /// [`run_framework_extract`] pass (see the module docs on decision D2).
    fn extract(&self, _path: &str, _content: &str, _language: Language) -> FrameworkExtraction {
        FrameworkExtraction::default()
    }

    /// Cross-file finalize, after every index and every incremental sync.
    /// Returns **mutated** nodes (id and qualified_name preserved) to persist.
    /// No v0 framework needs it; NestJS's RouterModule prefixing does, so the
    /// hook is part of the contract (Phase 8).
    fn post_extract(&self, _ctx: &dyn ResolutionContext) -> Vec<Node> {
        Vec::new()
    }
}

/// **The one place framework precedence is declared.**
///
/// `resolve_one` walks the registry in this order and the first hit with
/// confidence ≥ 0.9 wins outright, so this list is behavior. It is *not*
/// alphabetical and must not be "tidied" into alphabetical: it is
/// first-match-wins order.
///
/// Tasks 12–20 each add their resolver to [`builtin_resolvers`]; this list is
/// the contract they must match, and `registry_order_matches_the_contract`
/// (tests/fw_registry_test.rs) fails if a resolver is registered out of order
/// or under a name not listed here.
pub const REGISTRY_ORDER: &[&str] = &[
    "express", "react", "django", "flask", "fastapi", "spring", "go", "rust", "laravel", "rails",
    "aspnet",
];

/// The registered resolvers, in [`REGISTRY_ORDER`].
///
/// Empty until Tasks 12–20 land — Task 11 builds the seam, not the frameworks.
/// Each of those tasks appends its resolver here, in the order above.
fn builtin_resolvers() -> Vec<&'static dyn FrameworkResolver> {
    Vec::new()
}

static REGISTRY: LazyLock<Vec<&'static dyn FrameworkResolver>> = LazyLock::new(builtin_resolvers);

/// Every registered resolver, in [`REGISTRY_ORDER`]. Stable across calls.
pub fn all_framework_resolvers() -> &'static [&'static dyn FrameworkResolver] {
    &REGISTRY
}

/// Look one up by [`FrameworkResolver::name`].
pub fn framework_resolver(name: &str) -> Option<&'static dyn FrameworkResolver> {
    REGISTRY.iter().copied().find(|r| r.name() == name)
}

/// The frameworks actually present in this project.
///
/// A resolver whose `detect` **panics** is caught and excluded — one framework
/// with a bad manifest regex must not fail the index. Use
/// [`detect_frameworks_among`] if you want to see the warnings.
pub fn detect_frameworks(ctx: &dyn ResolutionContext) -> Vec<&'static dyn FrameworkResolver> {
    detect_frameworks_among(all_framework_resolvers(), ctx).0
}

/// [`detect_frameworks`] over an explicit resolver list, returning the warnings
/// alongside the survivors.
///
/// The explicit list is also the seam the registry tests inject through — an
/// integration test lives in another crate and cannot reach a `#[cfg(test)]`
/// hook inside the lib.
pub fn detect_frameworks_among<'a>(
    resolvers: &[&'a dyn FrameworkResolver],
    ctx: &dyn ResolutionContext,
) -> (Vec<&'a dyn FrameworkResolver>, Vec<String>) {
    let mut kept = Vec::new();
    let mut warnings = Vec::new();
    for r in resolvers.iter().copied() {
        match catch_unwind(AssertUnwindSafe(|| r.detect(ctx))) {
            Ok(true) => kept.push(r),
            Ok(false) => {}
            Err(_) => warnings.push(format!(
                "framework '{}' panicked in detect() — skipped (never fatal)",
                r.name()
            )),
        }
    }
    (kept, warnings)
}

/// Of `detected`, those applicable to `language` — preserving registry order.
/// A resolver with `languages() == None` matches every language.
pub fn applicable_frameworks<'a>(
    detected: &[&'a dyn FrameworkResolver],
    language: Language,
) -> Vec<&'a dyn FrameworkResolver> {
    detected
        .iter()
        .copied()
        .filter(|r| match r.languages() {
            None => true,
            Some(langs) => langs.contains(&language),
        })
        .collect()
}

/// Emit every detected framework's route/config nodes over the whole project.
///
/// Runs **after** extraction and **before** resolution (route nodes must exist
/// before a reference can bind to one). Part C's driver owns the call site.
pub async fn run_framework_extract<S: GraphStore>(
    store: &S,
    ctx: &dyn ResolutionContext,
    detected: &[&'static dyn FrameworkResolver],
) -> Result<FrameworkExtractStats> {
    let files: Vec<String> = ctx
        .files_with_language()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    run_framework_extract_for_files(store, ctx, detected, &files).await
}

/// [`run_framework_extract`] restricted to `paths` — the incremental-sync path.
/// Same function, a file subset.
pub async fn run_framework_extract_for_files<S: GraphStore>(
    store: &S,
    ctx: &dyn ResolutionContext,
    detected: &[&'static dyn FrameworkResolver],
    paths: &[String],
) -> Result<FrameworkExtractStats> {
    let mut stats = FrameworkExtractStats::default();
    if detected.is_empty() || paths.is_empty() {
        return Ok(stats);
    }

    let wanted: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();

    // Sorted iteration — the emitted node order is the insert order, and the
    // insert order is observable (id ties, parity diffs). `files_with_language`
    // is already sorted; the BTreeSet keeps the subset sorted too.
    let mut nodes: Vec<Node> = Vec::new();
    let mut refs: Vec<UnresolvedRef> = Vec::new();

    for (path, language) in ctx.files_with_language() {
        if !wanted.contains(path.as_str()) {
            continue;
        }
        let applicable = applicable_frameworks(detected, *language);
        if applicable.is_empty() {
            continue;
        }
        let Some(source) = ctx.read_file(path) else {
            continue;
        };

        for fw in applicable {
            match catch_unwind(AssertUnwindSafe(|| fw.extract(path, &source, *language))) {
                Ok(mut out) => {
                    // Stamp the file's language on any node that left it blank
                    // (`route_node` does — see routes.rs).
                    for n in &mut out.nodes {
                        if n.language.is_empty() {
                            n.language = language.as_str().to_string();
                        }
                    }
                    nodes.extend(out.nodes);
                    refs.extend(out.refs);
                }
                Err(_) => stats.warnings.push(format!(
                    "framework '{}' panicked extracting '{}' — skipped (0 nodes)",
                    fw.name(),
                    path
                )),
            }
        }
    }

    // Deterministic insert order regardless of how the frameworks emitted.
    nodes.sort_by(|a, b| {
        (&a.file_path, a.start_line, &a.name, &a.id).cmp(&(
            &b.file_path,
            b.start_line,
            &b.name,
            &b.id,
        ))
    });
    refs.sort_by(|a, b| {
        (
            &a.from_node_id,
            &a.reference_name,
            &a.reference_kind,
            a.line,
        )
            .cmp(&(
                &b.from_node_id,
                &b.reference_name,
                &b.reference_kind,
                b.line,
            ))
    });

    for chunk in nodes.chunks(CHUNK) {
        store.insert_nodes(chunk).await?;
        stats.nodes += chunk.len() as u64;
    }
    for chunk in refs.chunks(CHUNK) {
        store.insert_unresolved(chunk).await?;
        stats.refs += chunk.len() as u64;
    }
    Ok(stats)
}

/// Run every detected framework's [`FrameworkResolver::post_extract`] and
/// persist the nodes it returns. Per-framework catch: one failing framework
/// contributes a warning, never a failed index.
pub async fn run_post_extract<S: GraphStore>(
    store: &S,
    ctx: &dyn ResolutionContext,
    detected: &[&'static dyn FrameworkResolver],
) -> Result<FrameworkExtractStats> {
    let mut stats = FrameworkExtractStats::default();
    let mut mutated: Vec<Node> = Vec::new();

    for fw in detected {
        match catch_unwind(AssertUnwindSafe(|| fw.post_extract(ctx))) {
            Ok(nodes) => mutated.extend(nodes),
            Err(_) => stats.warnings.push(format!(
                "framework '{}' panicked in post_extract — skipped",
                fw.name()
            )),
        }
    }

    mutated.sort_by(|a, b| a.id.cmp(&b.id));
    for chunk in mutated.chunks(CHUNK) {
        // `insert_nodes` is insert-or-replace, which is exactly the "persist the
        // mutated node, id preserved" contract post_extract promises.
        store.insert_nodes(chunk).await?;
        stats.nodes += chunk.len() as u64;
    }
    Ok(stats)
}

/// Batch size for the emission pass's inserts (mirrors selene-db's own).
const CHUNK: usize = 1000;
