# Phase 2 — extraction parity vs. CodeGraph TS

**Gate: GREEN, tolerance 0.** The Rust port (`selene-extract`) reproduces the real
CodeGraph TypeScript extractor's node/edge/reference counts on a shared, byte-identical
fixture corpus, with exactly one justified deviation (a TS false positive we deliberately
do not reproduce).

| | value |
|---|---|
| **Baseline** | codegraph `9ccf5d022cdc4c5f33f2cd374f23fa70401e62f0` (`src/` pristine) |
| **Corpus** | 41 fixtures, 13 languages, `crates/selene-extract/tests/fixtures/parity/` |
| **Gate** | `crates/selene-extract/tests/parity_gate.rs` — asserts EVERY counter, tolerance 0 |
| **Deviations** | 1 (C++; see §4) |

---

## 1. Headline

| | nodes | edges | refs | total |
|---|---:|---:|---:|---:|
| **TS (codegraph 9ccf5d0)** | 202 | 167 | 139 | **508** |
| **Rust (selene-extract)** | 202 | 167 | 138 | **507** |
| delta | 0 | 0 | −1 | **−1 (0.2%)** |

The single `−1` ref is the C++ deviation in §4 — TS emits a phantom `extends` from a class
that has no base clause. Every other counter matches exactly.

## 2. Per-language

Counts are the TS baseline; Rust matches all of them (except the one C++ ref).

| Language | fixtures | nodes | edges | refs |
|---|--:|--:|--:|--:|
| c | 1 | 7 | 6 | 4 |
| cpp | 3 | 16 | 15 | 10 |
| csharp | 2 | 16 | 14 | 7 |
| go | 4 | 19 | 17 | 17 |
| java | 4 | 20 | 16 | 9 |
| javascript | 3 | 13 | 10 | 8 |
| kotlin | 4 | 16 | 12 | 6 |
| php | 2 | 8 | 6 | 7 |
| python | 4 | 20 | 16 | 14 |
| ruby | 3 | 13 | 10 | 6 |
| rust | 5 | 19 | 16 | 15 |
| tsx | 2 | 9 | 7 | 6 |
| typescript | 4 | 26 | 22 | 30 |
| **total** | **41** | **202** | **167** | **139** |

## 3. How the gate cannot pass vacuously

`extractFromSource` with an unloaded grammar does **not** throw — it returns an empty result
plus a `parser_error` (`tree-sitter.ts:427-450`). A baseline generated without grammar init
would be all zeros, and the gate would pass forever against a Rust side that was equally
broken. Three independent defenses, all tested:

1. **The dumper refuses to write** (`tools/parity/dump-ts-extraction.mjs`) if any fixture
   yields 0 nodes or any error. Proven by sabotage: with grammar init commented out, every
   fixture produced `0 nodes`, the script exited 1, and no file was written.
2. **`baseline_is_not_vacuous`** re-asserts from the Rust side: ≥25 files, ≥100 nodes,
   ≥50 edges, ≥50 refs, `codegraphCommit != "unknown"`, and every fixture non-empty on
   *both* sides. The dumper now derives the SHA from `git rev-parse` rather than trusting an
   env var, so a baseline can no longer be written without provenance.
3. **`harness_catches_a_synthetic_mismatch`** proves the differ differs: identical inputs ⇒
   0 diffs; perturb one counter ⇒ exactly 1 diff, correctly attributed.

**Stale deviations fail the gate too** — an entry matching no observed difference is
reported and fails, so a fixed divergence cannot leave a permanent whitelist that silently
re-permits a regression.

## 4. The one deviation — TS is wrong, Rust is right

`cpp/f.cpp` — `refs.extends` ts=1 rust=0 (plus its rollup into `refs`).

TS emits `extends:Widget` for:

```cpp
class Factory { public: static Widget create(); };
```

There is **no base clause in that source at all**.

**Root cause (found while closing the inheritance gap).** TS's `extractInheritance` has an
arm for Go struct embedding — a `field_declaration` with no `field_identifier`, where the
type *is* the name (`tree-sitter.ts:5359-5376`). That arm is **not language-gated**, and C++
spells a member declaration `field_declaration` too. `static Widget create();` has no
`field_identifier` (the name lives inside the `function_declarator`), so TS reads the
member's **return type** as an embedded base class and emits a phantom supertype.

We gate that arm to Go. Emitting the ref would be a false inheritance edge, which the Global
Constraints' **"silent beats wrong"** rule forbids. `cpp_member_return_type_is_not_a_base_class`
(`tests/lang_cpp_test.rs`) is the regression guard.

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

The dumper runs under **`vite-node`**, not `npx tsx`: `web-tree-sitter` 0.25.10 ships an
Emscripten CJS artifact Node's ESM lexer cannot statically analyze, so under tsx
`Parser === undefined` and `Parser.init()` throws. Vite's resolver handles the interop —
it is the loader codegraph's own vitest suite uses.

## 6. Known coverage limits

The corpus gates what it contains. These TS behaviors are **not yet exercised** by a fixture,
so parity on them is unmeasured: Lombok member synthesis, Ruby DSL block bodies, Ruby module
docstrings, PHP class-constant visibility, Python stacked `@staticmethod`, C++ stack
construction, the value-reference cap (`MAX_VALUE_REF_NODES`) and `SELENE_VALUE_REFS=0`.
Adding fixtures for them is the natural next expansion — the inheritance arms were in exactly
this state until the fixtures in §2 were added, and adding them turned up nine real gaps.
