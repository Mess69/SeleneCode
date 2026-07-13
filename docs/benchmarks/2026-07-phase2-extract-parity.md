# Phase 2 — extraction parity vs. CodeGraph TS

**Gate: GREEN, tolerance 0, on COUNTS and NAMES.** The Rust port (`selene-extract`)
reproduces the real CodeGraph TypeScript extractor's node/edge/reference counts *and the
identity of every symbol it names* over a shared, byte-identical fixture corpus — with two
justified deviations, both cases where TS is wrong and we deliberately refuse to reproduce it.

| | value |
|---|---|
| **Baseline** | codegraph `9ccf5d022cdc4c5f33f2cd374f23fa70401e62f0` (`src/` pristine) |
| **Corpus** | 68 fixtures, 13 languages, `crates/selene-extract/tests/fixtures/parity/` |
| **Gate** | `crates/selene-extract/tests/parity_gate.rs` — 8 assertions, tolerance 0 |
| **Deviations** | 2 (C++ phantom bases; C# record base name) — see §4 |

---

## 1. Headline

| | nodes | edges | refs | total |
|---|---:|---:|---:|---:|
| **TS (codegraph 9ccf5d0)** | 380 | 325 | 299 | **1004** |
| **Rust (selene-extract)** | 380 | 325 | 297 | **1002** |
| delta | 0 | 0 | −2 | **−2 (0.2%)** |

The `−2` refs are the two C++ phantom base classes of §4 — inheritance TS invents from
classes that have no base clause. Every other counter, and every other *name*, matches exactly.

## 2. Per-language

Counts are the TS baseline; Rust matches all of them except the two C++ phantoms.

| Language | fixtures | nodes | edges | refs |
|---|--:|--:|--:|--:|
| c | 3 | 22 | 19 | 12 |
| cpp | 7 | 40 | 37 | 24 |
| csharp | 4 | 35 | 31 | 16 |
| go | 6 | 31 | 28 | 28 |
| java | 6 | 38 | 32 | 22 |
| javascript | 5 | 23 | 18 | 18 |
| kotlin | 5 | 24 | 19 | 16 |
| php | 4 | 23 | 20 | 20 |
| python | 6 | 32 | 26 | 26 |
| ruby | 5 | 23 | 18 | 14 |
| rust | 6 | 29 | 28 | 33 |
| tsx | 5 | 19 | 14 | 22 |
| typescript | 6 | 41 | 35 | 48 |
| **total** | **68** | **380** | **325** | **299** |

## 3. What the gate asserts — and the holes it used to have

Eight assertions. Five of them exist because the gate was, at various points, **green while
comparing nothing**. Each is a hole that was actually open, not a hypothetical.

| Assertion | The hole it closes |
|---|---|
| `ts_rust_extraction_count_parity` | arity drift |
| `ts_rust_extraction_name_parity` | **identity drift.** A count gate cannot see `extends:Base` becoming `extends:Base(A)` — same count, different thing. A port under count-pressure is exactly what manufactures those. This half found a Ruby bug the count half could not: `calls:@db.query` had been silently truncated to `calls:@db`. |
| `every_fixture_on_disk_is_gated` | **ungated fixtures.** The diff iterates the *baseline*, so a fixture added but never dumped is compared by nobody. Ten heritage fixtures sat in exactly that state behind a green gate. |
| `language_detection_agrees` | **comparing two different extractors.** TS detects language from the PATH; Rust from path *and content*. A disagreement (a C fixture re-detected as C++) would compare two engines' output and call it parity. |
| `baseline_is_not_vacuous` | **the all-zeros baseline.** `extractFromSource` with an unloaded grammar does not throw — it returns an empty result (tree-sitter.ts:427-450). A baseline dumped without grammar init would be all zeros and the gate would pass forever. Also asserts the name sets are populated, so the name half cannot pass by comparing empty vectors. |
| `harness_catches_a_synthetic_mismatch` / `name_harness_...` | **a differ that doesn't diff.** Both perturb known-good inputs (a bumped count, a rename, an over-emission, a duplicate) and require the harness to report exactly the injected fault. |
| `every_deviation_is_justified` | a deviation without a cited cause is an unexamined bug wearing a deviation's clothes. |

Two further defenses live outside the Rust test:

- **The dumper refuses to write** a baseline in which any fixture yields 0 nodes or any error.
  Proven by sabotage: with grammar init commented out, every fixture produced `0 nodes`, the
  script exited 1, nothing was written.
- **The dumper derives `codegraphCommit` from `git rev-parse`** rather than trusting an env
  var. Forgetting to export it silently stamped `"unknown"` and cost a regeneration cycle.

**Stale deviations fail the gate too** — an entry matching no observed difference is reported
and fails, so a fixed divergence cannot leave a whitelist that silently re-permits a regression.

## 4. The two deviations — TS is wrong, we stay silent

`deviations.toml` is the authority; both entries carry TS line numbers.

### 4.1 C++ phantom base classes (`cpp/f.cpp`, `cpp/service.cpp`)

TS emits `extends` refs from classes that have **no base clause at all**:

```cpp
class Factory { public: static Widget create(); };   // TS: extends:Widget
class Service { private: Client *client_; };         // TS: extends:Client
```

**Root cause.** TS's `extractInheritance` has an arm for **Go struct embedding**
(tree-sitter.ts:5359-5376) — a `field_declaration` with no `field_identifier` means the type
*is* the name. That arm is **not language-gated**, and C++ spells member declarations
`field_declaration` too. Compounding it, TS's check
`child.namedChildren.some(c => c.type === 'field_identifier')` only inspects **direct** named
children — and in C++ a member's name is nested inside its *declarator*
(`function_declarator`, `pointer_declarator`, …), so it is never a direct child. Both members
above therefore look "embedded" to TS, which takes their **type** as a base class.

So any C++ member whose declarator nests its name and whose type is a bare `type_identifier`
becomes a phantom supertype — a return type or a field type, indifferently. Primitive members
(`int x;`) are spared only because `int` is a `primitive_type`.

We gate the arm to Go. A false inheritance edge is what **"silent beats wrong"** forbids: it
would tell an agent that `Factory` derives from `Widget`.
Guard: `cpp_member_return_type_is_not_a_base_class` (`tests/lang_cpp_test.rs`).

### 4.2 C# record base name (`csharp/Records.cs`) — invisible to the count gate

```csharp
public record DerivedRec(int A, string B) : SimplePositional(A);
```

TS emits `extends:SimplePositional(A)` — the raw `primary_constructor_base_type` text,
argument list included — a name no symbol carries and no resolver can match. We unwrap to the
type head and emit `extends:SimplePositional`, which resolves.

`refs.extends` is **1 on both sides**. Only the name half of the gate can see this at all; it
is the reason that half exists.

## 5. Reproducing

```bash
# 1. TS baseline (needs codegraph's deps: `npm ci` in ../codegraph)
cd ../codegraph
npx vite-node <selene>/tools/parity/dump-ts-extraction.mjs \
    <selene>/crates/selene-extract/tests/fixtures/parity \
    <selene>/crates/selene-extract/tests/fixtures/parity/expected.json

# 2. The gate
cd <selene> && cargo test -p selene-extract --test parity_gate
```

Regenerate the baseline after **any** corpus change — `every_fixture_on_disk_is_gated` will
fail loudly if you forget.

The dumper runs under **`vite-node`**, not `npx tsx`: `web-tree-sitter` 0.25.10 ships an
Emscripten CJS artifact Node's ESM lexer cannot statically analyze, so under tsx
`Parser === undefined` and `Parser.init()` throws. Vite's resolver handles the interop — it is
the loader codegraph's own vitest suite uses.

## 6. Known coverage limits

The gate gates what the corpus contains, and nothing else. These TS behaviors are **not yet
exercised by any fixture**, so parity on them is unmeasured: Lombok member synthesis, Ruby
module docstrings, PHP class-constant visibility, Python stacked `@staticmethod`, C++ stack
construction, the value-reference cap (`MAX_VALUE_REF_NODES`) and `SELENE_VALUE_REFS=0`.

This list should be read as a to-do, not a footnote. Class inheritance sat on exactly such a
list — and when fixtures were finally written for it, it turned out to be **unimplemented in
nine languages at once**, invisible behind a green gate. Every subsequent corpus extension has
found real bugs (the decorator over-emission, the Ruby call-name truncation, the PHP import
spelling). The pattern is reliable enough to plan around: **write the fixture first.**
