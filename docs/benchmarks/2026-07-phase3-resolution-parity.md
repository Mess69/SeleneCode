# Phase 3 — resolution parity + dispatch coverage (results)

**Date:** 2026-07-13 · **Branch:** `feat/phase3-selene-resolve`
**Baseline:** the real CodeGraph pipeline (`CodeGraph.init()` + `indexAll()`), commit
recorded in `expected.json`.

---

## The two gates

| Gate | Test | State |
|---|---|---|
| **Resolution parity** — TS ⇄ Rust edge identity, tolerance 0 | `crates/selene-resolve/tests/resolution_parity_gate.rs` | **GREEN** — 303 edges, tolerance 0 |
| **Dispatch coverage** — *THE* Phase 3 gate | `crates/selene-resolve/tests/dispatch_coverage_gate.rs` | **GREEN** — 11 framework + 4 synthesizer flows, 3 controls clean |

### Parity, in numbers

Corpus: 18 project trees — 11 framework, 4 synthesizer, 3 control.

```
TS baseline:  303 edges    contains 175 · imports 55 · calls 39 · references 31 · instantiates 3
                           tree-sitter 298 · heuristic 5
                           synthesized: callback, event-emitter, react-render, jsx-render

Rust:         303 matched · 0 missing · 0 extra     — tolerance 0, all five channels
```

The gate went from RED (1 missing, the `jsx-render` heuristic edge) to GREEN when
synthesis was actually **wired into the pipeline** — see the third seam, below.

### Coverage, in flows

All eleven, walked hop-by-hop on the real pipeline with real `detect()`:

| Framework | Entry point | Chain |
|---|---|---|
| express | `POST /users/login` | → `login` → `hashPassword` |
| react | `/article/:slug` | → `Article` → `useArticle` → `fetchArticle` |
| django | `articles/<slug>/` | → `ArticleDetail` → `get` → `get_article` |
| flask | `POST /articles` | → `create` → `create_article` |
| fastapi | `GET /articles` | → `index` → `list_articles` |
| spring | `GET /articles` | → `getAll` → `listArticles` |
| **spring (config)** | `@Value("${app.greeting}")` | → **`app.greeting` in `application.yml`** |
| go (gin) | `POST /articles` *(un-prefixed — see below)* | → `CreateArticle` → `Create` |
| rust (axum) | `POST /articles` | → `create_article` → `create` |
| laravel | `GET /articles` | → `index` → `listArticles` |
| rails | `GET /articles` | → `index` → `recent` |
| aspnet | `GET /api/articles` | → `GetAll` → `ListAsync` |

Zero half-bridged flows.

---

## What the gate caught — THREE inert seams, one bug class

This is the headline, and it is worth stating plainly: **three of the resolver's seams
were stubs that returned "nothing found", and every unit test passed anyway.**

| Seam | What was actually inert |
|---|---|
| `StoreContext::import_mappings()` | **Ladder step 8 in its entirety.** Go cross-package (#388), JVM FQN imports (#314), barrel/renamed re-exports (#629) *and* the pre-filter's `matches_any_import` escape, path aliases, Python module members, Rust `crate::` paths, C/C++ includes |
| `StoreContext::re_exports()` | every renamed re-export (`export { signIn as login }`) |
| `StoreContext::new`'s four singletons | `go.mod` (so *every* Go cross-package call), tsconfig aliases, workspace packages, C++ include dirs |
| `synth::run_synthesis` | **all five dispatch channels.** The passes existed, their unit tests passed, and no pipeline anywhere called them |

The loaders all existed. They were written, they were tested, and **they had never once
been called in production.** Tasks 4–6 were three commits of dead code.

**Why no test noticed.** Every strategy test runs against `FakeContext`, which *injects*
the mappings directly. A seam that returns an empty list is indistinguishable from a seam
that works and found nothing — the resolver simply resolves less, and nothing anywhere
says why. Only a gate that drives the **real** context through the **real** store against
an **independent** baseline can tell those two apart. That is the whole argument for this
gate, and it paid for itself on the first run.

Fixing them moved parity **152 → 198** of the matched edges and dissolved a "TS
inconsistency" I had already written up: the same `import Article from './Article'` binds
to the *file* in two files and to the *symbol* in a third — TS was right and consistent
(a `.jsx`/`.tsx` PascalCase ref is claimed by the react resolver at step 7, *before*
imports at step 8), and with the loaders wired we reproduce it exactly. **A "TS bug" that
turns out to be our own missing wiring is the most valuable output a parity gate has.**

---

## Design decisions written into the gate (do not "fix" these)

**Endpoints compare on semantics, never id spelling.** A TS id is a literal string, ours
a sha256 of the same four components; comparing ids would diff 100% of edges and tell us
nothing. Both derive from `(file, kind, name, line)` — that is the identity.

**A route's key is `route:<name>@<file>:<line>`.** `name` is `"{METHOD} {path}"`, kept
byte-identical to TS in `routes.rs` *as a wire contract precisely so this comparison can
exist*. The line is in the key because several routes legally share one file **and** one
line (`get(h).post(h2)`, `resources :articles`), separated only by name — drop it and a
deleted verb passes unnoticed.

**`framework` is NOT in the route key.** A TS route node has no such field; the indexed
`framework`/`route_method`/`route_path` fields are our Task-11 decision. Folding it in
would compare a field we invented against a field TS does not have — our own output
against itself. Detection is asserted separately (`frameworks_detected_agree`).

**Gin's entry point is addressed by its UN-PREFIXED path.** `v1.POST("/articles")` under
`r.Group("/api/v1")` stores `/articles`. Loosening the gate's route lookup to accommodate
a prefix would make it pass on a route it never found.

**A class-level fallback is never a `via` pin.** Laravel/Rails fall back to the controller
*class* when the action is missing. Pinning the class would let a resolver that lost the
**action** still pass — a half-bridged flow, shipped green.

---

## Coverage limits (what these gates do NOT prove)

1. **The core resolver is only gated where the framework fixtures happen to exercise it.**
   The plan's `core-*` corpus (import resolution per ecosystem, a chained call, an
   ambiguous name that must **decline** rather than guess, and the negative control — a
   chained call on a type *lacking* the method ⇒ **no edge**) is not built. This is the
   largest remaining hole and it is **not blocked** by anything.
2. **There is no `batch.rs`** (plan Task 27). Both gates drive the pipeline themselves,
   running the four steps the product's driver must run — including the ordering contract
   (**build the resolution context AFTER `run_framework_extract`**, or `known_names`
   predates the route/config nodes and every framework reference is pre-filtered away).
   When `batch.rs` lands, that contract must move into it.
3. **The corpus is small** (18 projects, 303 edges). It proves the mechanisms; it does not
   prove behavior at repo scale.

### The controls are half the proof

Every positive assertion in either gate is satisfied by a synthesizer that bridges
*everything in sight*. Only a **control** — ordinary code containing none of the dispatch
shapes — catches one that guesses, and a channel that guesses is far worse than one that
misses: a wrong dispatch edge is a confident lie about how the program runs. Both the
dumper and the gate now enforce the control property from opposite ends (the dumper
refuses to *write* a baseline in which a control carries a heuristic edge; the gate
refuses to *accept* one).

## Deviations (machine-checked; a stale entry fails the gate)

`crates/selene-resolve/tests/fixtures/dispatch/deviations.toml` — 4 entries, all
`side = "rust"` (we emit an edge TS does not):

- **2 × Next.js file-route → page component.** TS emits the route node and *nothing from
  it*; by this phase's own invariant TS's route is the half-bridge. We are the correct
  side.
- **2 × `from services import x` module-source → file edge** (flask, fastapi). A true
  file→file dependency edge that TS omits for symbol imports (it emits one only when the
  imported *name* is a module). A superset, not a wrong answer — but tightenable to TS's
  narrower model on request.
