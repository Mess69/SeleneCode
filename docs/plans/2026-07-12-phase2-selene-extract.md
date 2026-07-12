# Phase 2 — `selene-extract`: tree-sitter core + v0 language wave — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the `selene-extract` crate — the generic tree-sitter AST-walker engine (port of
CodeGraph's `TreeSitterExtractor`), per-language extractor configs for the **v0 wave
(13 grammars: TypeScript, TSX, JavaScript, Python, Rust, Go, Java, Kotlin, C, C++, C#,
PHP, Ruby)**, node-id/qualified-name/docstring helpers, the scan pipeline (git fast path,
ScopeIgnore, embedded repos, generated-file detection), Rayon parallel parse with the
ordered-commit invariant, incremental single-file re-index, and FN_REF_SPECS
function-reference capture for v0 languages. **Gate:** node/edge/ref counts on shared
fixtures match the TS build (tolerance documented in Task 19).

**Architecture:** native tree-sitter 0.26 (statically linked grammar crates — the entire
TS WASM layer disappears: no worker threads, no worker recycling, no WASM-OOM
retry/comment-strip passes, no grammar-bytes pre-read; see extraction-core.md §Rust port
notes). Extraction is **fully synchronous**; parallelism is a rayon `par_iter` fan-out;
async exists ONLY at the DB seam (`tokio::task::spawn_blocking` bridges the rayon batch,
then results commit to `GraphStore`/`SurrealStore` **strictly in original scan order** —
the #1015 determinism invariant, because resolution disambiguates same-named symbols by
insertion order). Extraction NEVER resolves cross-file: it emits nodes, same-file edges,
and `UnresolvedReference`s; edges across files are exclusively the Phase 3 resolver's job.
Language behavior = a static `NodeTypeTables` struct + a `LanguageRules` trait with
default-implemented hooks (the Rust shape of TS's struct-of-optional-closures
`LanguageExtractor`, per extraction-core.md §Rust port notes).

**Tech Stack:** tree-sitter 0.26.x (workspace pin) + 12 grammar crates pinned exact
(`=x.y.z`, covering 13 grammars — `tree-sitter-typescript` provides both TS and TSX);
sha2; rayon; `ignore` crate (gitignore matching); regex; thiserror 2; dev: insta,
tempfile. tokio only in the orchestrator's DB seam. selene-db (GraphStore + SurrealStore)
as the storage consumer.

**Reference (in priority order):**
- `docs/reference/from-codegraph/maps/extraction-core.md` — THE parity contract
  (walker §8, node id §9, calls §10, configs §11, scan §1, pipeline §2, store §3,
  incremental §4, grammars §6, dispatch §7, Wire/contract surfaces, Rust port notes).
- `docs/reference/from-codegraph/maps/extraction-langs.md` — per-language configs
  (§Public interface = the `LanguageExtractor` contract; §Per-language highlights;
  §Wire/contract surfaces incl. the full EXTENSION_MAP).
- `docs/reference/from-codegraph/design/function-ref-capture.md` — FN_REF_SPECS.
- `docs/reference/rust-ecosystem-2026-07.md` §3 — grammar crate pins; §5 practices.
- TS parity source `../codegraph` (relative to repo root): consult ONLY the specific
  file a task names (e.g. `src/extraction/tree-sitter-helpers.ts`), or a named
  describe-block of `__tests__/extraction.test.ts` — never at large.

## Global Constraints

- **Node id (EXACT, byte-for-byte):**
  `"<kind>:" + hex(sha256("{filePath}:{kind}:{name}:{line}"))[..32]` where `kind` is the
  `NodeKind::as_str()` wire string and `line` is the 1-based startLine. **Exception:** the
  file node id is the UNHASHED literal `file:<filePath>`. The `kind:` prefix is
  load-bearing — downstream code key-matches on id prefixes (`file:`, `class:`, …).
- File content hash: sha256 hex of the file text. Sync-phase comparison uses
  `floor(mtime-in-millis)` — millisecond truncation, match it.
- **`EXTRACTION_VERSION` decision:** Rust starts at **1** (`pub const EXTRACTION_VERSION:
  u32 = 1` in `selene-core`). Rationale: the TS counter (24) versioned the `.codegraph/`
  store; `.selene/` is a disjoint store no TS binary ever reads, so the version space
  restarts. **Bump rule (document in the const's doc comment):** ANY change to extraction
  output shape — node/edge/ref emission, id inputs, docstring cleanup, qualified-name
  spelling — bumps it; stored-version < engine-version ⇒ "re-index recommended" guidance,
  NEVER a hard error.
- **Errors collected, never thrown:** every extractor returns partial results +
  `ExtractionError` (codes: `unsupported_language`, `parser_error`, `parse_error`,
  `read_error`, `path_traversal`, `size_exceeded`; severity `error`|`warning`). No panics;
  no `unwrap`/`expect` outside `#[cfg(test)]` (workspace lints already warn).
- **Determinism:** identical input bytes ⇒ identical output (ids embed lines; walk order
  is deterministic; sort where the source order isn't inherently stable). No wall-clock in
  output except `Node.updated_at`. `pre_parse` transforms are **byte-offset preserving**:
  blank with equal-length spaces BYTE-wise (non-ASCII bytes each become one space byte),
  newlines kept — positions feed node ids.
- Lines emitted 1-based (`tree-sitter row + 1`), columns 0-based. `qualifiedName` =
  `[...cppNamespacePrefix, ...names of non-file stack nodes, name].join("::")` — never a
  file-path component (FTS pollution). Receiver-method override `Receiver::name`.
- **Chained-call marker** `inner().method` and `::` separators are extractor↔resolver
  contracts — spell them exactly.
- **"Silent beats wrong":** never emit refs for dynamic constructs (dynamic import paths,
  data-driven calls). Skip silently rather than guess.
- Env vars renamed to the `SELENE_` prefix: `SELENE_PARSE_WORKERS`, `SELENE_VALUE_REFS=0`.
  (TS's `CODEGRAPH_PARSE_TIMEOUT_MS` becomes `SELENE_PARSE_TIMEOUT_MS` if the Task 1 spike
  confirms a usable cancellation API; otherwise drop with a doc note.)
- Wire types: reuse `selene_core::{Node, Edge, NodeKind, EdgeKind, Provenance,
  Visibility}` — extraction stamps `provenance: TreeSitter` on edges it emits.
- Constants carried verbatim (cite map §1/§2/§10): `MAX_FILE_SIZE = 1 MiB`,
  `MAX_VALUE_REF_NODES = 20_000`, `EMBEDDED_REPO_SEARCH_DEPTH = 4`,
  `EMBEDDED_REPO_SEARCH_ENTRIES = 2000`, `UNINDEXED_IGNORED_REPO_HINT_CAP = 100`,
  git exec timeouts 5s/10s/30s + 50 MB output cap, parse batch = `max(4, workers*2)`,
  workers = `SELENE_PARSE_WORKERS` or `clamp(cores-1, 1, 8)`, hard max 16.
- Every task: `cargo fmt && cargo clippy --all-targets && cargo test -p selene-extract`
  green before its commit. TDD: write the ported contract test first, watch it fail, then
  implement.

## File structure (all under `crates/selene-extract/` unless noted)

```
Cargo.toml                 tree-sitter (workspace) + 12 grammar crates pinned =x.y.z
src/lib.rs                 crate docs + re-exports
src/error.rs               ExtractionError, ErrorCode, Severity
src/types.rs               ExtractionResult, UnresolvedReference, constants
src/language.rs            Language enum (full registry), EXTENSION_MAP, detect_language,
                           is_source_file, .h sniffing, file-level-only set
src/grammars.rs            v0 grammar registry: Language -> tree_sitter::Language,
                           per-thread parser reuse
src/generated.rs           is_generated_file (regex suffix classifier)
src/helpers.rs             get_node_text, get_child_by_field, get_preceding_docstring,
                           clean_comment_markers
src/walker/mod.rs          TreeSitterExtractor session: file node, dispatch ladder,
                           create_node, qualified names, scope stack, imports
src/walker/body.rs         visit_function_body: calls, instantiations, bare calls,
                           static member reads, value-ref pass
src/walker/ts_core.rs      TS/JS core machinery: HOC components, store collections,
                           re-export/import-binding refs, type-annotation refs
src/rules/mod.rs           NodeTypeTables + LanguageRules trait + rules_for(Language)
src/rules/{typescript,javascript,python,rust_lang,go,java,kotlin,c,cpp,csharp,php,ruby}.rs
src/rules/cpp_preparse.rs  the 7 C/C++ blankers + C# directive blanker + helpers
src/fnref.rs               FN_REF_SPECS (v0 rows) + candidate gate
src/scan/ignore.rs         ScopeIgnore, DEFAULT_IGNORE_DIRS, defensive gitignore reader
src/scan/mod.rs            scan_directory, git fast path, embedded repos, FS fallback
src/orchestrator.rs        Indexer: index_all / index_files / index_file, IndexResult
tests/spike_grammars.rs    Task 1 spike (kept as grammar-parity smoke test)
tests/lang_<lang>_test.rs  per-language golden tests (ported TS assertions + insta)
tests/scan_test.rs         tempfile scan/ignore tests
tests/orchestrator_test.rs end-to-end index_all/index_file against in-memory SurrealStore
tests/parity_gate.rs       the Phase 2 gate (Task 19)
tests/fixtures/parity/<lang>/...   shared fixture corpus (see Task 19)
tests/fixtures/parity/expected.json + deviations.toml
tools/parity/dump-ts-extraction.mjs   (repo root) TS-side count dumper
docs/benchmarks/2026-07-phase2-extract-parity.md   gate results
```

`node_id`, `file_node_id`, `hash_content`, `EXTRACTION_VERSION` live in **`selene-core`**
(contract constants shared with selene-db tests, selene-sync, selene-mcp status — per
extraction-core.md §Rust port notes).

⚠ **`walker/mod.rs` sequencing:** Tasks 7, 8, 13, and 15a each modify
`src/walker/mod.rs` (distinct dispatch-ladder branches). Execute them SEQUENTIALLY —
never dispatch two of them to parallel subagents/worktrees.

---

### Task 1: Spike — tree-sitter 0.26 native API + pinned v0 grammars

**Files:** Modify: `crates/selene-extract/Cargo.toml` (deps: `tree-sitter.workspace =
true`, selene-core; grammar crates pinned EXACT per `rust-ecosystem-2026-07.md` §3:
`tree-sitter-typescript = "=0.23.2"` (TS + TSX), `tree-sitter-javascript = "=0.25.0"`,
`tree-sitter-python = "=0.25.0"`, `tree-sitter-rust = "=0.24.2"`, `tree-sitter-go =
"=0.25.0"`, `tree-sitter-java = "=0.23.5"`, `tree-sitter-kotlin-ng = "=1.1.0"`,
`tree-sitter-c = "=0.24.2"`, `tree-sitter-cpp = "=0.23.4"`, `tree-sitter-c-sharp =
"=0.23.5"`, `tree-sitter-php = "=0.24.2"` (use the `php` fn, not `php_only`),
`tree-sitter-ruby = "=0.23.1"`). Create: `tests/spike_grammars.rs`.

**Interfaces:** none (throwaway knowledge, kept as a smoke/parity-guard test).

- [ ] For each of the 13 grammars, build a `Parser`, `set_language`, parse a 5–15 line
  snippet, and assert the **node-type names each config depends on** (from
  extraction-langs.md §Per-language highlights): e.g. TS `method_definition`,
  `public_field_definition`, `lexical_declaration`, `arrow_function`; Python
  `function_definition`, `decorator`; Rust `function_item`, `impl_item`, `trait_item`,
  `scoped_identifier`; Go `method_declaration`? (verify — Go methods reach the walker via
  functionTypes w/ `methodsAreTopLevel`), `composite_literal`, `type_spec`; Java
  `object_creation_expression`; Kotlin (**highest risk** — kotlin-ng is a different
  lineage than the WASM grammar TS used) `simple_identifier`,
  `function_value_parameters`, `class_declaration`, `companion_object`; C/C++
  `init_declarator`, `qualified_identifier`, `namespace_definition`; C# preprocessor
  behavior post-blanking; PHP `function_definition`? (verify actual names); Ruby `call`,
  `body_statement`. Record every divergence from the map's node-type names as a comment
  block at the top of the test — these feed the config tasks.
- [ ] Validate mechanics the walker relies on: `child_by_field_name`, `named_children`
  iteration order, `node.kind()` string comparison cost (capture: we will precompute
  node-kind-ID sets per grammar in Task 5 if cheap), rows 0-based / columns 0-based,
  **byte** offsets (parse a snippet containing multi-byte UTF-8 and assert positions).
- [ ] Validate a cancellation/timeout mechanism for the per-parse safety net:
  tree-sitter 0.26's `parse_with_options` progress callback (or whatever 0.26 exposes) —
  document in a comment which API works; if none is workable, note that the timeout is
  dropped (native parsing doesn't have the WASM failure modes).
- [ ] Two extra probe fixtures: (a) a Kotlin `fun interface` snippet — record whether
  kotlin-ng yields the ERROR-node misparse the TS recovery targets, or parses it cleanly
  (Task 11's recovery-vs-drop decision depends on this); (b) a BOM-prefixed (`\u{FEFF}`)
  source file — record how positions behave and decide strip-vs-keep (document the
  decision; ids embed lines, not columns).
- [ ] Commit: `feat(extract): spike — tree-sitter 0.26 + pinned v0 grammars smoke test`

### Task 2: Contract constants in `selene-core` — node id, content hash, EXTRACTION_VERSION

**Files:** Modify: `crates/selene-core/src/lib.rs` (or new `src/ids.rs` module + re-export),
`crates/selene-core/Cargo.toml` (add `sha2.workspace = true`).

**Interfaces (the contract):**
```rust
/// "<kind>:" + hex(sha256("{file_path}:{kind}:{name}:{line}"))[..32]
pub fn node_id(file_path: &str, kind: NodeKind, name: &str, start_line: u32) -> String;
/// The UNHASHED literal "file:<path>" (extraction-core.md §9 exception).
pub fn file_node_id(file_path: &str) -> String;
/// sha256 hex of the full text.
pub fn hash_content(text: &str) -> String;
/// Starts at 1 for the Rust engine (TS lineage: 24; stores are disjoint). Bump on ANY
/// output-shape change; version mismatch => "re-index recommended", never an error.
pub const EXTRACTION_VERSION: u32 = 1;
```

- [ ] TDD golden byte test FIRST: hard-code the expected id for
  `node_id("src/utils.ts", NodeKind::Function, "calc", 10)` — compute the expectation
  out-of-band (`printf '%s' "src/utils.ts:function:calc:10" | shasum -a 256`, first 32 hex
  chars, prefixed `function:`). Also pin `file_node_id("a/b.ts") == "file:a/b.ts"` and a
  `hash_content` vector. (The TS suite never pinned these bytes — extraction-core.md
  §Test coverage says add the golden test in Rust; #899 edge-reattachment depends on it.)
- [ ] Fix the stale doc comment on `Node.id` in selene-core ("hash of file path +
  qualified name" — the real input is `path:kind:name:startLine`, see map §9).
- [ ] Also fix the `Node.qualified_name` rustdoc example (selene-core/src/lib.rs ~L244):
  it reads `src/utils.ts::MathHelper.calculateTotal`, embedding a file path — the
  contract (map §9) is NO file-path component; change the example to
  `MathHelper::calculateTotal`.
- [ ] Commit: `feat(core): node-id + content-hash + EXTRACTION_VERSION contract helpers`

### Task 3: Extraction types, Language registry, detection, generated-file classifier

**Files:** Create: `src/error.rs`, `src/types.rs`, `src/language.rs`, `src/generated.rs`;
Modify: `src/lib.rs`. Tests: `tests/language_detect_test.rs`.

**Interfaces:**
```rust
pub enum Severity { Error, Warning }
pub enum ErrorCode { UnsupportedLanguage, ParserError, ParseError, ReadError,
    PathTraversal, SizeExceeded }   // serde: snake_case wire strings per map §Wire
pub struct ExtractionError { pub message: String, pub severity: Severity,
    pub code: ErrorCode, pub file_path: Option<String> }
pub struct UnresolvedReference { pub from_node_id: String, pub reference_name: String,
    pub reference_kind: String,   // EdgeKind wire string or "function_ref"
    pub line: Option<u32>, pub column: Option<u32>,
    pub file_path: Option<String>, pub language: Option<String> }
pub struct ExtractionResult { pub nodes: Vec<Node>, pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedReference>, pub errors: Vec<ExtractionError>,
    pub duration_ms: u64 }
pub enum Language { Typescript, Tsx, Javascript, Jsx, Python, Rust, Go, Java, Kotlin,
    C, Cpp, CSharp, Php, Ruby, /* + every wave-2 name in EXTENSION_MAP */ ..., Unknown }
impl Language { pub fn as_str(&self) -> &'static str; }  // lowercase wire strings
pub fn detect_language(file_path: &str, source: Option<&str>) -> Language;
pub fn is_source_file(file_path: &str) -> bool;
pub fn is_file_level_only(l: Language) -> bool;   // {yaml, twig, properties}
pub fn is_generated_file(path: &str) -> bool;
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;
```

- [ ] Port the **FULL** `EXTENSION_MAP` verbatim from extraction-langs.md §Wire/contract
  ("`EXTENSION_MAP` (full)") — ALL languages, not just v0; it is "the single source of
  truth for indexability" and drifting it changes which files are counted. Wave-2
  languages detect but have no grammar registered → extraction returns an
  `unsupported_language` **warning** + skip (matches TS missing-grammar semantics).
- [ ] Port detection order (map §6): special cases first (`conf/routes`→yaml,
  `/(^|\/)(templates|sections)\/.+\.json$/i`→liquid, `/\.app(?:\.src)?$/i`→erlang), then
  overrides (parameter, default empty), then lowercased last-dot extension, else Unknown.
  `.h` files detected `c` sniff first 8192 chars: C++ regex
  `\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)`
  → cpp; ObjC regex `@(?:interface|implementation|protocol|synthesize)\b` → objc.
- [ ] Port `is_generated_file` — consult `../codegraph/src/extraction/generated-detection.ts`
  (81 LOC) for the full regex suffix list (`.pb.go`, `.min.js`, `_pb2.py`, …).
- [ ] TDD: port the "Language Detection" describe-block assertions of
  `../codegraph/__tests__/extraction.test.ts` + `extension-mapping.test.ts` (all
  extensions, `.h` sniff cases incl. export-macro-class C++ detection, Metal/CUDA→cpp).
- [ ] Commit: `feat(extract): language registry, detection, generated-file classifier`

### Task 4: AST helpers — node text, field access, docstring capture/cleanup

**Files:** Create: `src/helpers.rs`. Tests colocated `#[cfg(test)]` using real grammars.

**Interfaces:**
```rust
pub fn get_node_text(node: tree_sitter::Node, source: &str) -> &str;   // byte-slice
pub fn get_child_by_field<'t>(node: tree_sitter::Node<'t>, field: &str)
    -> Option<tree_sitter::Node<'t>>;
pub fn get_preceding_docstring(node: tree_sitter::Node, source: &str) -> Option<String>;
pub fn clean_comment_markers(raw: &str) -> String;  // exposed for unit tests
```

- [ ] Consult `../codegraph/src/extraction/tree-sitter-helpers.ts` (127 LOC — the ONE
  sanctioned full-file read) for the exact docstring semantics: which preceding siblings
  count as doc comments, adjacency rules, marker stripping per comment style
  (`///`, `//!`, `/** */` + leading `*`, `//`, `#`, Python `"""` docstring-in-body if
  handled here), joining with `\n`, trimming. Output is user-visible in MCP — byte parity
  matters.
- [ ] TDD: port the #780 docstring cases (one per comment style) from the TypeScript /
  Python / Rust describe blocks of `extraction.test.ts` as unit tests on real parses.
- [ ] Commit: `feat(extract): AST helpers — node text, fields, docstring capture/cleanup`

### Task 5: `LanguageRules` model + walker core (declarations) + Python config

**Files:** Create: `src/rules/mod.rs`, `src/rules/python.rs`, `src/walker/mod.rs`;
Modify: `src/lib.rs` (`extract_from_source` entry). Tests: `tests/lang_python_test.rs`.

**Interfaces (the Rust shape of TS's `LanguageExtractor` — extraction-langs.md §Public
interface is the field-for-field contract; every capability must exist, defaults inert):**
```rust
pub struct NodeTypeTables {
    pub function_types: &'static [&'static str], pub class_types: ..., pub method_types:
    ..., pub interface_types: ..., pub struct_types: ..., pub enum_types: ...,
    pub enum_member_types: ..., pub type_alias_types: ..., pub import_types: ...,
    pub call_types: ..., pub variable_types: ..., pub field_types: ...,
    pub property_types: ..., pub extra_class_node_types: ..., pub package_types: ...,
    pub name_field: &'static str, pub body_field: &'static str,
    pub params_field: &'static str, pub return_field: Option<&'static str>,
    pub methods_are_top_level: bool,      // Go
    pub skip_bodiless_class: bool,        // C++ (#1093)
    pub interface_kind: Option<NodeKind>, // Rust: Trait
}
pub trait LanguageRules: Sync {
    fn tables(&self) -> &'static NodeTypeTables;
    // Hooks — every optional closure from the TS interface, default no-op/None:
    fn pre_parse(&self, source: &str, file_path: &str) -> Option<String>;
    fn resolve_name(...); fn recover_mangled_name(...); fn extract_property_name(...);
    fn get_signature(...); fn get_visibility(...); fn is_exported(...); fn is_async(...);
    fn is_static(...); fn is_const(...); fn extract_modifiers(...);
    fn visit_node(&self, node: Node_, ctx: &mut Session) -> bool;  // true = handled
    fn synthesize_members(&self, class_idx: usize, ctx: &mut Session);
    fn classify_class_node(...); fn classify_method_node(...); fn resolve_body(...);
    fn extract_import(...) -> Option<ImportInfo>; fn extract_variables(...);
    fn get_receiver_type(...); fn get_return_type(...); fn resolve_type_alias_kind(...);
    fn is_misparsed_function(...); fn extract_bare_call(...); fn extract_package(...);
}
pub fn rules_for(l: Language) -> Option<&'static dyn LanguageRules>;
// Session = the ExtractorContext port: owns nodes/edges/refs/errors, node_stack,
// id->name map (O(n) qualified names — fixes TS O(n^2), same behavior), cpp namespace
// prefix stack. Exposes: create_node(kind, name, node, extra) -> Option<usize>,
// visit(node), visit_function_body(body, fn_idx), add_unresolved(ref), push/pop_scope,
// file_path(), source(), node_stack(), nodes(), node_mut(idx)  // Lombok taken-scan,
// Erlang-style endLine mutation (wave 2) need read+mutate access to prior nodes.
pub fn extract_from_source(file_path: &str, source: &str, language: Language)
    -> ExtractionResult;
```

- [ ] Walker per extraction-core.md §8, THIS task covers declarations: emit the file node
  (`id = file_node_id(path)`, kind File, name = basename, qualifiedName = filePath,
  startLine 1, endLine = line count), push on stack; optional `namespace` node from a
  package header via `package_types`/`extract_package`; then `visit(root)` with the
  dispatch ladder **in map §8's exact order** (custom `visit_node` hook first, short-
  circuits when true; branches set skip-children because sub-extractors walk their own
  bodies): functionTypes (inside class-like OR in methodTypes ⇒ method) → classTypes (via
  classify → class/struct/enum/interface/trait) → extraClassNodeTypes → methodTypes
  (classify may demote to property, #808) → interfaceTypes → structTypes → enumTypes
  (+ members) → typeAliasTypes → property/fieldTypes (inside class) → variableTypes
  (top-level only) → importTypes. (Call/instantiation/fnref branches arrive Task 6;
  C++ namespace_definition branch arrives Task 13; TS branches Task 7/8.)
- [ ] `create_node`: skip empty names (return None); `node_id(...)` from selene-core;
  qualifiedName join `::` skipping the file node; `contains` edge from stack top
  (provenance TreeSitter); merge `extract_modifiers` into `decorators`; receiver-method
  qualifiedName override `Receiver::name`; `updated_at` = now-millis (the ONE wall-clock).
- [ ] Imports: `import` node + `imports` UnresolvedReference per module; Python core
  machinery: `import a, b` inline (one import node + ref per dotted_name/aliased_import)
  and per-name refs for `from m import X, Y` (map §11 python row).
- [ ] Python config verbatim from map §11: functionTypes=methodTypes=
  `[function_definition]` (method iff inside class-like); classTypes
  `[class_definition]`; imports `[import_statement, import_from_statement]`; calls
  `[call]` (wired Task 6); variables `[assignment]`; signature = params + `" -> "` +
  return; `is_async` = previous sibling `async`; `is_static` = preceding `decorator`
  containing `staticmethod`; `extract_import` handles only `import_from_statement`.
- [ ] TDD: port the Python describe-block assertions (classes/methods/functions names,
  kinds, qualifiedNames `Class::method`, docstrings, decorators, imports, file node +
  containment edges) + one insta YAML snapshot of the full `ExtractionResult` for a
  representative fixture (sorted? NO — walk order is deterministic; snapshot pins it).
- [ ] Doc note in `src/walker/mod.rs` module docs: web-tree-sitter (the TS build)
  counted column positions in UTF-16 code units; native tree-sitter counts UTF-8 BYTES —
  column values on non-ASCII lines may differ from the TS build. Node ids
  (`path:kind:name:line`) and the Task 19 count gate are unaffected.
- [ ] Commit: `feat(extract): generic walker core + LanguageRules model + Python`

### Task 6: Body walker — calls, instantiations, value references

**Files:** Create: `src/walker/body.rs`. Tests: extend `tests/lang_python_test.rs`
(Python-only — Task 6 must commit green on Python alone; the generic call-shape branches
for other languages are asserted in the task that lands each config: TS in Task 7,
Go/Rust in Task 9, C/C++ in Task 13, Kotlin/C# in Tasks 11/14).

Port extraction-core.md §10 exactly:

- [ ] `visit_function_body(body, fn_idx)` walks: **calls** — callee name for
  call-expression-likes: member receivers (`member_expression|attribute|
  selector_expression|navigation_expression|field_expression`) yield `receiver.method`
  when receiver is a simple identifier not in `{self,this,cls,super}`, else bare
  `method`; call-receivers re-encode chained factories as `inner().method` — v0 gate set
  `{c,cpp,kotlin,rust,go,csharp}` with per-language guards (Rust: only when inner is
  `scoped_identifier`; Go: only bare `identifier`; Kotlin: only capitalized inner);
  `scoped_identifier` calls keep full `Module::function` text; Go conversions normalize
  via `/^\(\s*\*?\s*([A-Za-z_][\w.]*)\s*\)$/`; C/C++ template args stripped
  (`strip_cpp_template_args`, Task 12). Emit
  `UnresolvedReference{reference_kind:"calls", line, column}` — never a cross-file edge.
- [ ] **Instantiations** — `INSTANTIATION_KINDS = {new_expression,
  object_creation_expression, instance_creation_expression, composite_literal,
  struct_expression, instance_expression}` → `instantiates` refs.
- [ ] Bare calls via the `extract_bare_call` hook (Ruby, Task 14); **static member
  reads** — MEMBER_ACCESS_TYPES receivers gated to `STATIC_MEMBER_LANGS ∩ v0 =
  {java,csharp,kotlin,php,cpp}`; local var type annotations; nested **named** functions
  become nodes; `<anonymous>` functions get NO node but their body is still walked
  (module wrappers, #528); `is_misparsed_function` skips the node, walks the body.
- [ ] **Value-reference pass** (`flush_value_refs`, default on, `SELENE_VALUE_REFS=0`
  disables): same-file `references` **edges** (metadata `{"valueRef":true}`, provenance
  TreeSitter) from function/method/const/var scopes to file/class-scope constants with
  `name.len() >= 3 && name contains [A-Z_]`, shadow-pruned by counting declarators vs
  file-scope defs, capped at `MAX_VALUE_REF_NODES = 20_000` visits.
- [ ] TDD (Python fixtures ONLY): call refs (`receiver.method` when receiver is a simple
  identifier not in the skip set, bare `method` otherwise, line/col), instantiation refs,
  value refs (incl. the shadow-prune and the cap), anonymous-wrapper body walk. The
  chained-factory / scoped_identifier / conversion branches are IMPLEMENTED here but
  tested in Tasks 7/9/13 with their languages' grammars.
- [ ] Commit: `feat(extract): body walker — calls, instantiations, value references`

### Task 7: TypeScript / TSX / JavaScript configs

**Files:** Create: `src/rules/typescript.rs`, `src/rules/javascript.rs`; Modify:
`src/walker/mod.rs` (ladder insertion points: the variableTypes branch gains arrow-const
naming from `variable_declarator`; the typeAliasTypes branch gains the #634 alias-member
`TypeAlias::member` qualifiedName override — see the ⚠ sequencing note under File
structure).
Tests: `tests/lang_typescript_test.rs`, `tests/lang_javascript_test.rs`.

- [ ] typescript config (shared by tsx) verbatim from map §11: functionTypes
  `[function_declaration, arrow_function, function_expression]`; classTypes
  `[class_declaration, abstract_class_declaration]`; methodTypes `[method_definition,
  public_field_definition]` with `classify_ts_class_member` (#808: field is a method ONLY
  if value is arrow/function-expression or a call_expression whose arguments contain one
  — HOF wrapper `throttle(()=>…)` — else property); interfaces `[interface_declaration]`;
  enums `[enum_declaration]` (members `property_identifier, enum_assignment`); typeAlias
  `[type_alias_declaration]`; imports `[import_statement]` (module = `source` field,
  quotes stripped); calls `[call_expression]`; variables `[lexical_declaration,
  variable_declaration]`; fields name/body/parameters, return `return_type`;
  `resolve_body` digs arrow bodies out of field definitions and HOF wrappers;
  `is_exported` walks ancestors for `export_statement`; `is_const` = lexical_declaration
  with a `const` child; signature = params text + `: returnType`.
- [ ] Walker core TS additions needed by the config: arrow-const naming from
  `variable_declarator` (an exported `const f = () => …` becomes a function named `f`);
  TS type-alias members get qualifiedName `TypeAlias::member` (#634).
- [ ] javascript config (shared by jsx): consult
  `../codegraph/src/extraction/languages/javascript.ts` (99 LOC) — key divergence:
  `field_definition` uses the `property` field for the name.
- [ ] TDD: port the TypeScript, "Arrow Function Export", "Type Alias" (#634
  string-literal contracts), and "File Node" describe blocks + insta snapshots for one
  ts, one tsx, one js fixture.
- [ ] Call-shape tests deferred from Task 6 (TS grammar now drives them): member-receiver
  `receiver.method` vs `this.foo()` → bare `foo` (skip set `{self,this,cls,super}`),
  `new_expression` instantiation refs, `<anonymous>` module-wrapper body walk (#528).
- [ ] Commit: `feat(extract): TypeScript/TSX/JavaScript extractors`

### Task 8: TS/JS core machinery — components, store collections, type-annotation refs

**Files:** Create: `src/walker/ts_core.rs`; Modify: `src/walker/mod.rs` (ladder
insertion point: the "TS/JS re-export & Vue-store export branches" slot per map §8 —
after the variableTypes branch, before callTypes; see the ⚠ sequencing note under File
structure). Tests: `tests/ts_machinery_test.rs`.

- [ ] React HOC components: `forwardRef|memo|React.*|/^styled\b/` wrapper + PascalCase
  const ⇒ `component` node.
- [ ] Exported object-of-functions; Zustand `create(...)`; RTK `createApi` endpoints
  (`RTK_HOOK_NAME_RE = /^use[A-Z][A-Za-z0-9]*(?:Query|Mutation)$/`); Pinia/Vuex store
  collections (`{actions,mutations,getters}` × `{defineStore,createStore}`, ≥2 hits of
  `VUE_STORE_FILE_SIGNAL`) — consult `../codegraph/src/extraction/tree-sitter.ts` ONLY
  for the `VUE_STORE_FILE_SIGNAL` definition and the exported-collection walk shape.
- [ ] Import-binding refs and re-export refs (TS/JS re-export branch of the ladder).
- [ ] Type-annotation `references` for `TYPE_ANNOTATION_LANGUAGES ∩ v0 = {typescript,
  tsx, kotlin, rust, go, java, csharp, php}`, `BUILTIN_TYPES` filtered — consult
  tree-sitter.ts for the BUILTIN_TYPES set (copy verbatim, note it in a comment).
- [ ] TDD: port "Exported Variable" (Zustand/Zod/XState) block + the v0-relevant cases of
  `../codegraph/__tests__/vue-store-extraction.test.ts` + TS type-refs assertions.
- [ ] Commit: `feat(extract): TS/JS component & store collections + type-annotation refs`

### Task 9: Go + Rust configs

**Files:** Create: `src/rules/go.rs`, `src/rules/rust_lang.rs`.
Tests: `tests/lang_go_test.rs`, `tests/lang_rust_test.rs`.

- [ ] Go (map §Per-language highlights): `methods_are_top_level: true`; receiver from the
  `receiver` field text via `/\(\s*(?:[A-Za-z_]\w*\s+)?\*?\s*([A-Za-z_]\w*)/` (#583
  generics); exported = first char A–Z; `type_spec` reclassified struct/interface via
  inner `struct_type`/`interface_type`; return type: unwrap multi-return first element,
  `*Foo`→`Foo`, strip `<…>`/`[…]`, last `.` segment, must match `/^[A-Za-z_]\w*$/`.
- [ ] Rust (map §11 rust row, verbatim): functionTypes=methodTypes=`[function_item,
  function_signature_item]`; classTypes `[]`; interfaces `[trait_item]` +
  `interface_kind: Trait`; structs `[struct_item]`; enums `[enum_item]` (members
  `enum_variant`); typeAlias `[type_item]`; imports `[use_declaration]` → ROOT crate
  segment (recursing scoped_identifier; `crate`/`super`/`self` kept) + core
  `emit_rust_use_binding_refs` per-leaf refs; variables `[let_declaration, const_item,
  static_item]`; receiver = LAST direct `type_identifier` of enclosing `impl_item`
  (generic via `generic_type`) — flips function→method, sets `Type::name`, core links a
  same-file `contains` edge from the struct/enum/trait node found by name; `-> Self` ⇒
  marker string `'self'`; return normalization to bare last segment,
  primitives/unit/tuple ⇒ None; visibility default Private, any `pub` (incl.
  `pub(crate)`) ⇒ Public — **intended TS quirk, keep it** (extraction-langs.md §port
  notes); `impl_item` ladder branch emits `implements` refs.
- [ ] TDD: port Go + Rust describe blocks (impl/trait/supertraits, receiver methods,
  uppercase exports) + snapshots.
- [ ] Call-shape tests deferred from Task 6 (Go/Rust grammars now drive them): Rust
  `scoped_identifier` calls keep full `Module::function`; Rust chained factory
  `inner().method` ONLY when inner is `scoped_identifier`; Go chained factory only for a
  bare `identifier` inner; Go conversion normalization `(*T)(x)` via
  `/^\(\s*\*?\s*([A-Za-z_][\w.]*)\s*\)$/`.
- [ ] Commit: `feat(extract): Go + Rust extractors`

### Task 10: Java config + Lombok member synthesis

**Files:** Create: `src/rules/java.rs`. Tests: `tests/lang_java_test.rs`.

- [ ] Config: package namespace via `package_declaration` → `scoped_identifier|
  identifier` (qualifiedNames `pkg::Class::method`); `annotation_type_declaration` as
  interface; `is_const` = modifiers contain both `\bstatic\b` and `\bfinal\b`.
  Consult `../codegraph/src/extraction/languages/java.ts` for the type lists the map
  doesn't enumerate.
- [ ] Lombok `synthesize_members` (#912) — full spec is in extraction-langs.md §java:
  trigger annotations `Getter/Setter/Data/Value/Builder/SuperBuilder/ToString/
  EqualsAndHashCode` + log set `{Slf4j, Log4j, Log4j2, Log, CommonsLog, JBossLog,
  Flogger, XSlf4j, CustomLog}`. Getter `getX` (`isX` kept for primitive boolean `isFoo`),
  setter `setX` (boolean `isFoo`→`setFoo`); skip static fields; setters skip final;
  `builder()` static returning `<Class>Builder`; `toString`/`equals(Object o)`/
  `hashCode`; `log` field (`Logger log`, private static). All synthesized members:
  `visibility: Public`, `decorators: ["lombok"]`, `docstring: "Lombok-generated (@Ann)"`;
  NEVER override an explicitly declared member (`taken` sets keyed `classQN::name`,
  scanned from `ctx.nodes()`).
- [ ] TDD: port the Java block (packages, anonymous classes) + Lombok cases + snapshot.
- [ ] Commit: `feat(extract): Java extractor + Lombok member synthesis`

### Task 11: Kotlin config (kotlin-ng)

**Files:** Create: `src/rules/kotlin.rs`. Tests: `tests/lang_kotlin_test.rs`.

- [ ] Config per map §kotlin: no field names in grammar → `name_field:
  "simple_identifier"` (resolve positionally); positional return type after
  `function_value_parameters`; properties via `visit_node` hook — scope walk: function
  body/lambda/init/getter/setter → local (skip); `companion_object|object_declaration` →
  `val`→constant / `var`→variable; `class_declaration` → field. `fun interface` misparse
  recovery (consult `../codegraph/src/extraction/languages/kotlin.ts` for the 2
  ERROR-node patterns — verify they still occur under kotlin-ng; if kotlin-ng parses
  them cleanly, DROP the recovery and note it in `deviations.toml` reasoning);
  `classify_class_node` by `interface`/`enum` keyword children; `extract_modifiers`
  captures `expect`/`actual`; `Unit|Nothing` returns rejected.
- [ ] **kotlin-ng drift protocol:** where Task 1's spike found node-type names differing
  from the map (which documents the WASM grammar TS used), adapt the config and add a
  `// kotlin-ng:` comment per adaptation; expect parity-gate deviations here.
- [ ] TDD: port the Kotlin block (fun interface, expect/actual, companions) + snapshot.
- [ ] Commit: `feat(extract): Kotlin extractor (kotlin-ng)`

### Task 12: C/C++/C# offset-preserving pre-parse blankers

**Files:** Create: `src/rules/cpp_preparse.rs`. Unit tests colocated.

Pure string→string functions; ALL replace with equal-length spaces, newlines preserved,
**byte-offset exact** (blank by byte ranges; non-ASCII bytes each → one space byte —
see Global Constraints; the TS code was char-based and only worked because the parser
consumed the same JS string).

- [ ] `blank_cpp_export_macros` — regex
  `/\b(class|struct)(\s+)([A-Z][A-Z0-9_]+)(?=\s+[A-Za-z_]\w*(?:\s+final)?\s*[:{])/g`.
- [ ] `blank_cpp_inline_macros` — curated ~60-token list, longest-first alternation,
  lookahead `(?=\s+[A-Za-z_])` — **copy the token list verbatim** from
  `../codegraph/src/extraction/languages/c-cpp.ts`.
- [ ] `blank_cpp_api_prefix_macros` — `/\b[A-Z][A-Z0-9_]*(?:_API|_EXPORT|_ABI)\b(?=\s+[A-Za-z_])/g`.
- [ ] `blank_cpp_inline_annotation_macros` — `UMETA|UPARAM|UE_DEPRECATED\w*`
  balanced-paren, string-aware (interleaves regex + manual char scan; port carefully —
  the `regex` crate has no lastIndex, drive offsets explicitly).
- [ ] `blank_cpp_annotation_macro_calls` — line-leading `^[ \t]*[A-Z][A-Z0-9_]{2,}\s*\(`,
  balanced parens, next non-ws char must match `[A-Za-z_~#]`.
- [ ] `blank_metal_attributes` (`.metal`) and `blank_cuda_constructs` (`.cu/.cuh` or
  `looksLikeCudaSource` = `__global__|__device__|__constant__|cudaStream_t`):
  `__launch_bounds__(…)`, dunder specifiers, `<<<[^;]{0,400}?>>>` only when braces
  balance — regexes in map §cpp row.
- [ ] `recover_mangled_cpp_name` (only names containing whitespace, not `operator…`/
  `~…`, not `Ret (name)` idiom; last token before `(`; reject the 23-entry
  `CPP_PRIMITIVE_NAMES` — copy from c-cpp.ts), `normalize_cpp_return_type` (unwrap
  `unique_ptr|shared_ptr|weak_ptr|optional<T>`, strip cv/`<>`/`*&`, reject the 27-entry
  primitive set), `strip_cpp_template_args`.
- [ ] `blank_csharp_preprocessor_directives` — per-line
  `^([ \t]*)#[ \t]*(if|elif|else|endif)\b[^\n]*` (#237, keeps both branches);
  `ensure_trailing_newline` (VB.NET, wave 2 — trivial, include now).
- [ ] TDD: dedicated unit tests per blanker — port the TS unit cases (the exported
  helpers are individually unit-tested in c-cpp.ts/csharp.ts test blocks) + a
  multi-byte-UTF-8-inside-macro-args offset test.
- [ ] Commit: `feat(extract): C/C++/C# offset-preserving pre-parse blankers`

### Task 13: C + C++ configs

**Files:** Create: `src/rules/c.rs`, `src/rules/cpp.rs`; Modify: `src/walker/mod.rs`
(C++ `namespace_definition` ladder branch: push `namespacePrefix`, NO node minted).
Tests: `tests/lang_c_test.rs`, `tests/lang_cpp_test.rs`.

- [ ] C config: structs/enums/typedef; `is_const` = named child `type_qualifier` with
  text `const`; content-gated CUDA blank in pre_parse; shares
  `recover_mangled_cpp_name`. Consult `c-cpp.ts` for the type lists.
- [ ] C++ config: `skip_bodiless_class: true` (#1093); name via `qualified_identifier`
  BFS skipping `parameter_list`/`trailing_return_type`; receiver = qualifier segments
  joined `::`; macro-defined-name recovery (`MACRO_NAME(real_name,…)`: name regex
  `/^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$/`, first param lone `type_identifier` containing
  lowercase, no other lone-ident params); `is_misparsed_function`: name starts
  `namespace`, in `{switch,if,for,while,do,case,return}`, or macro-misparsed type decl;
  pre_parse chain in EXACT order: export-macros → inline-macros → api-prefix →
  inline-annotation → annotation-calls; then `.metal` → metal blanker; `.cu/.cuh`/CUDA
  sniff → cuda blanker; return normalization from Task 12.
- [ ] TDD: port C and C++ describe blocks (free-function names, export-macro classes,
  namespaces, template-arg stripping in calls) + snapshots.
- [ ] Commit: `feat(extract): C + C++ extractors`

### Task 14: C# + PHP + Ruby configs

**Files:** Create: `src/rules/csharp.rs`, `src/rules/php.rs`, `src/rules/ruby.rs`.
Tests: `tests/lang_csharp_test.rs`, `tests/lang_php_test.rs`, `tests/lang_ruby_test.rs`.

- [ ] C#: pre_parse = `blank_csharp_preprocessor_directives`; records → class unless a
  `struct` keyword child (#831); namespaces (block + file-scoped) as package_types;
  visibility default Private; `is_const` = `const` or (`static`+`readonly`).
- [ ] PHP: include/require(`_once`) expressions as importTypes with static-string-literal
  paths ONLY (#660 — dynamic paths silently skipped); class constants + trait `use` →
  `implements` refs via visit_node hook; visibility default Public; `self|static|$this`
  return → marker `'self'`; the 19-name lowercase non-class return set (copy from
  `../codegraph/src/extraction/languages/php.ts`); file-level `namespace Foo\Bar;`
  unbraced form only.
- [ ] Ruby: `module` nodes via hook; mixins `include|extend|prepend` on receiverless
  `call` → `implements` refs from enclosing scope (args of type `constant`/
  `scope_resolution` only); `extract_bare_call`: statement-level `identifier` whose
  parent ∈ `{body_statement,then,else,do,begin,rescue,ensure,when}`, skipping
  `{true,false,nil,self,super,__FILE__,__LINE__,__dir__}` and Constants; Ruby class-scope
  `CONST=` variables branch (walker core, map §8).
- [ ] TDD: port C# (records #831), PHP, Ruby (mixins, bare calls) blocks + snapshots.
- [ ] Commit: `feat(extract): C# + PHP + Ruby extractors`

### Task 15a: FN_REF_SPECS — capture machinery + gate (C, C++, TS/JS, Python)

**Files:** Create: `src/fnref.rs`; Modify: `src/walker/mod.rs` + `body.rs` (fnRef capture
branch in the ladder + `scan_fn_ref_subtree` for consumed subtrees — see the ⚠
sequencing note under File structure). Tests: `tests/fnref_test.rs`.

The parity contract is `docs/reference/from-codegraph/design/function-ref-capture.md`;
consult `../codegraph/src/extraction/function-ref.ts` for the exact per-language spec
node-type entries. Capture side ONLY — resolution rules (unique-or-drop, class-scoped
`this.X`, overload refusal) are Phase 3.

- [ ] Machinery: FN_REF_SPECS registry keyed by Language. Capture fires from the walkers
  only — a node is visited exactly once; consumed subtrees (top-level variable
  initializers, class field initializers, custom visit_node hooks) get
  `scan_fn_ref_subtree`, which halts at nested function boundaries.
- [ ] The **gate** (`flush_fn_ref_candidates`): candidate survives only if its name
  matches a same-file function/method name ∪ imported binding names (`imports` refs
  only), name regexes `^[A-Za-z_$][A-Za-z0-9_$]*$` and
  `^[A-Za-z_$][A-Za-z0-9_$.\\]*[.\\]([A-Za-z_$][A-Za-z0-9_$]*)$` (map §10). Ungated:
  C-family FILE-scope initializer positions (constant-expression context — design doc
  rule 2); qualified `Type::member` / `this.X` candidates (rule: gate can't see their
  scope). Skips: param-forward (`o->cb = cb`, labeled arg name==value — rule 6),
  destructuring (rule 7), `is_generated_file` paths (rule 8, whole pass).
- [ ] Emit `reference_kind: "function_ref"` (internal-only kind; never persists as an
  edge kind).
- [ ] Spec rows (design doc §Per-language value positions): C (`argument_list`,
  `assignment_expression.right`, `initializer_pair.value`, `initializer_list`/
  `init_declarator.value`, `&fn` via `pointer_expression` — only the `&` operator);
  C++ (**`&` forms only** in args/rhs/varinit — `addressOfOnly`, `&fn`/`&Cls::method`;
  bare ids qualify ONLY in FILE-scope initializer tables); TS/JS (`arguments`,
  `assignment_expression.right`, `pair.value`, `array`/`variable_declarator.value`,
  `this.method` → `this.`-prefixed candidate); Python (`argument_list` +
  `keyword_argument.value`, `assignment.right`, `pair.value`, `list`, `self.method`
  attribute).
- [ ] TDD: per-position capture tests (arg / RHS / keyed init / list / wrapper) for the
  4 languages + gate tests: unknown name dropped; imported binding kept; C file-scope
  table ungated; C++ bare id in args dropped while `&fn` kept; generated file yields
  zero candidates.
- [ ] Commit: `feat(extract): function-ref capture machinery + gate (C/C++/TS/Python)`

### Task 15b: FN_REF_SPECS — remaining v0 languages (Go, Rust, Java, Kotlin, C#, Ruby, PHP)

**Files:** Modify: `src/fnref.rs` (spec rows only — NO walker changes, so this task does
not participate in the walker/mod.rs sequencing constraint). Tests: extend
`tests/fnref_test.rs`.

- [ ] Spec rows (design doc table + function-ref.ts for exact node types): Go
  (`argument_list`, assignment/short_var_declaration `expression_list`, `keyed_element`,
  `literal_value`/`var_spec.value`); Rust (`arguments`, `assignment_expression.right`,
  `field_initializer.value`, `array_expression`, `static_item`/`let_declaration.value`);
  Java (`argument_list`, `assignment_expression.right`, `variable_declarator.value`;
  wrapper form `method_reference` (`Cls::m`, `this::m`) — the only WRAPPER form per
  function-ref-capture.md's table, not the only capture position); Kotlin
  (`value_arguments`, `assignment` last child; wrappers `callable_reference` `::f`,
  `navigation_expression` `this::m`); C# (`argument`, `assignment_expression.right`
  incl. `+=`, `initializer_expression`, `variable_declarator`, `this.M`
  member_access_expression); Ruby (`method(:sym)`/`&method(:sym)` only — bare ids are
  calls/locals — plus the hook DSLs `(skip_)?(before|after|around)_*`/`validate`/
  `set_callback`/`helper_method`/`rescue_from(with:)` symbols → class-scoped
  `this.<sym>`; `validates` plural EXCLUDED — its symbols are attributes); PHP (string
  callables ONLY as args of `PHP_CALLABLE_HOFS` — copy the list from function-ref.ts;
  `[$this,'m']` → `this.m`; `[Foo::class,'m']`/`'Cls::m'` → qualified).
- [ ] TDD: one test per capture position from the table per language, incl. the Ruby
  `validates`-exclusion and PHP non-HOF-string-dropped cases.
- [ ] Commit: `feat(extract): function-ref capture specs — remaining v0 languages`

### Task 16: ScopeIgnore — default ignores + gitignore semantics

**Files:** Create: `src/scan/ignore.rs`. Tests: `tests/scan_ignore_test.rs` (tempfile).

- [ ] `DEFAULT_IGNORE_DIRS` — copy the full 60+ name list + globs (`*.egg-info/`,
  `cmake-build-*/`, `bazel-*/`, `**/res/<androidType>*/` for the 12 Android res types)
  verbatim from `../codegraph/src/extraction/index.ts`.
- [ ] Defensive `.gitignore` reader: reject NUL/invalid-UTF-8 files whole; per-line
  compile probe (try building a single-pattern matcher; drop only uncompilable lines).
- [ ] `ScopeIgnore` (map §1 semantics): defaults + root `.gitignore`; per-embedded-repo
  matchers applied longest-root-first; overrides parameter `{include, exclude}` — user
  `exclude` wins over everything; user `include` forces files back in UNLESS
  default-ignored. Config-file loading is Phase 8; take overrides as a plain parameter
  defaulting to empty. API: `ScopeIgnore::build(root, embedded_roots, overrides)` +
  `fn ignores(&self, rel: &str) -> bool`.
- [ ] Implementation: `ignore` crate `GitignoreBuilder`/`Gitignore` — but VERIFY the two
  load-bearing conventions against ported tests: (a) directory matching via the
  `rel + "/"` convention (`matched_path_or_any_parents` with `is_dir`), (b) negation
  (`!pattern`) semantics identical to npm-`ignore`.
- [ ] TDD: tempfile trees; port the relevant cases of
  `../codegraph/__tests__/android-res-exclusion.test.ts` and `exclude-config.test.ts`.
- [ ] Commit: `feat(extract): ScopeIgnore — default ignores + gitignore semantics`

### Task 17: `scan_directory` — git fast path, embedded repos, FS fallback

**Files:** Create: `src/scan/mod.rs`. Tests: `tests/scan_test.rs` (tempfile + real
`git init` repos).

**Interfaces:**
```rust
pub fn scan_directory(root: &Path, overrides: &ScanOverrides) -> Result<Vec<String>>;
    // sorted, root-relative, forward slashes
pub fn discover_embedded_repo_roots(root: &Path) -> Vec<String>;
pub fn find_unindexed_ignored_repos(root: &Path) -> Vec<String>;  // cap 100
```

- [ ] Git fast path (map §1, port exactly): `git rev-parse --show-toplevel`; if root ≠
  toplevel AND `git check-ignore -q <root>` exits 0 → FS-walk fallback. Else
  `git ls-files -z -s --recurse-submodules` — parse NUL-delimited (#541 CJK paths);
  mode-160000 gitlink entries collected separately, recursed as embedded repos when a
  real `.git` checkout exists — plus `git ls-files -z -o --exclude-standard` for
  untracked; a trailing-slash untracked dir containing a `.git` **directory** = embedded
  repo (recurse); a `.git` **file** whose `gitdir:` matches
  `/(^|[\\/])\.git[\\/](modules[\\/][^\\/]+[\\/])?worktrees[\\/]/` = worktree → skip;
  drop the `./` whole-cwd sentinel (#936). Gitignored embedded repos only when opted-in
  via overrides (`include_ignored`).
- [ ] `run_git(args, timeout, max_output)` helper: `std::process::Command` + the
  `wait-timeout` crate (**NEW workspace dep** — absent from the roadmap pins table; add
  to root `[workspace.dependencies]` with a pinned version, e.g. `wait-timeout = "0.2"`),
  timeouts 5s (rev-parse/check-ignore),
  10s, 30s (ls-files) per map §1, 50 MB output cap; on timeout/failure fall back to the
  FS walk — never error the scan.
- [ ] Non-git fallback: recursive walk with per-directory scoped `.gitignore` matchers +
  symlink-cycle guard (`fs::canonicalize` visited set).
- [ ] Embedded discovery: `EMBEDDED_REPO_SEARCH_DEPTH = 4`,
  `EMBEDDED_REPO_SEARCH_ENTRIES = 2000`, hint cap 100. All results filtered through
  `ScopeIgnore`; output sorted for determinism.
- [ ] TDD: tempfile repos exercising: tracked + untracked, gitignored file excluded,
  embedded repo recursed, worktree `.git`-file skipped, non-git fallback, CJK filename
  through `-z`, symlink cycle terminates. Port the gitdir-pointer regex cases of
  `../codegraph/__tests__/worktree-detection.test.ts` (incl. Windows separators).
- [ ] Commit: `feat(extract): scan pipeline — git fast path, embedded repos, FS fallback`

### Task 18: Orchestrator — rayon parse fan-out, ordered commit, incremental re-index

**Files:** Create: `src/orchestrator.rs`; Modify: `Cargo.toml` (rayon, tokio, selene-db).
Tests: `tests/orchestrator_test.rs` (tokio multi-thread, in-memory SurrealStore).

**Interfaces:**
```rust
pub struct IndexProgress { pub phase: Phase /* Scanning|Parsing|Storing */,
    pub current: usize, pub total: usize, pub current_file: Option<String> }
pub struct IndexResult { pub success: bool, pub files_indexed: u32,
    pub files_skipped: u32, pub files_errored: u32, pub files_discovered: u32,
    pub nodes_created: u64, pub edges_created: u64,
    pub errors: Vec<ExtractionError>, pub duration_ms: u64 }
pub struct Indexer { root: PathBuf, store: SurrealStore }  // concrete store: the
    // replace_file_extraction protocol is an inherent SurrealStore method today
    // (selene-db store.rs note: Phase 1 Task 10 may lift it onto GraphStore — if it
    // has by the time this task runs, take `S: GraphStore` instead).
impl Indexer {
    pub async fn index_all(&self, on_progress: Option<...>) -> IndexResult;
    pub async fn index_files(&self, rel_paths: &[String]) -> IndexResult;
    pub async fn index_file(&self, rel_path: &str) -> Result<ExtractionResult>;
}
```

- [ ] `index_all` (map §2, minus everything the WASM layer justified): scan → needed
  languages = set of `detect_language(file)` (+`cpp` whenever `c` present) → iterate the
  scan list in batches of `PARSE_BATCH = max(4, workers*2)` where workers =
  `SELENE_PARSE_WORKERS` or `clamp(cores-1, 1, 8)`, hard max 16 (build a dedicated rayon
  `ThreadPool` of that size): read files (skip > `MAX_FILE_SIZE` with a `size_exceeded`
  warning, counted skipped; read failures → `read_error`), `spawn_blocking` the batch
  into `pool.install(|| batch.par_iter().map(extract).collect::<Vec<_>>())` —
  order-preserving collect — then commit the batch **sequentially in original scan
  order** via `replace_file_extraction` (the #1015 ordered-commit invariant). `files
  with unchanged content_hash` skip at commit (`get_file` pre-check). Emit progress per
  phase. `success = files_indexed > 0 || no severity-Error errors`.
- [ ] The FileRecord: `path`, `content_hash = hash_content(text)`, `language.as_str()`,
  `size`, `modified_at = floor(mtime millis)`, `indexed_at = now millis`, `node_count`,
  `errors` (serialized ExtractionErrors). Convert `UnresolvedReference` →
  `selene_db::UnresolvedRef` at the seam: denormalize `file_path`/`language`, `status:
  Pending`, `candidates: vec![]`, `name_tail` = last `.`/`::` segment of
  `reference_name`.
- [ ] Persist `EXTRACTION_VERSION` via `set_meta("extraction_version", …)` after a
  successful index_all; on open, `index_all` reads it and (if stored < engine) includes
  a "re-index recommended" note in the result — never an error.
- [ ] `index_file` (incremental single-file re-index, map §4 essence): read + stat; hash
  pre-check vs `get_file` (same hash ⇒ no-op); extract; `replace_file_extraction`. NOT
  ported here: `sync()`/`getChangedFiles()` (Phase 6, selene-sync), framework detection
  (Phase 3) — pass an empty framework-name list internally so the seam exists.
- [ ] Deliberately dropped WITH a doc comment naming each (extraction-core.md §Rust port
  notes): ParseWorkerPool + recycle/crash-budget/late-result policy, PARSER_RESET_INTERVAL,
  WASM-OOM retry + comment-blank passes, wasm-runtime-flags, grammar-bytes pre-read,
  sequential grammar loading, stderr `Aborted(` filter, and `FILE_IO_BATCH_SIZE = 10`
  (map §2 — folded into PARSE_BATCH: without worker threads a separate read-batch size
  buys nothing). Keep the per-parse timeout only if Task 1 found a workable API.
- [ ] TDD: tempfile mini-project (3 languages, an ignored dir, an oversized file) →
  `index_all` against `SurrealStore::in_memory()`: counts match, oversized skipped with
  warning; run twice ⇒ second run all-skipped and byte-identical node ids; touch one
  file (shift lines) → `index_file` re-indexes only it and cross-file incoming edges
  re-attach (assert via ReplaceStats); determinism: two fresh runs produce identical
  `stats()` and id sets.
- [ ] Commit: `feat(extract): orchestrator — rayon fan-out + ordered commit + incremental re-index`

### Task 19: Count-parity gate vs the TS build (THE Phase 2 gate)

**Files:** Create: `tests/fixtures/parity/<lang>/…` (13 language dirs),
`tools/parity/dump-ts-extraction.mjs` (repo root), `tests/fixtures/parity/expected.json`,
`tests/fixtures/parity/deviations.toml`, `tests/parity_gate.rs`,
`docs/benchmarks/2026-07-phase2-extract-parity.md`.

**Fixture strategy (precise):** the TS contract suite embeds sources inline in
`extraction.test.ts`, so the "shared fixtures" are MATERIALIZED here: for each v0
language, 2–4 small source files whose contents are copied **byte-for-byte** from that
language's describe-block snippets (plus one composite file exercising imports + calls +
a class with methods), living under `crates/selene-extract/tests/fixtures/parity/<lang>/`.
These same bytes are what the TS side parses — both engines consume identical input.

- [ ] Write `tools/parity/dump-ts-extraction.mjs`: (1) **grammar init FIRST** — `await
  initGrammars(); await loadGrammarsForLanguages(neededLangs)` (from
  `src/extraction/grammars`) before any extraction; `extractFromSource` with an unloaded
  grammar returns an EMPTY result + error (tree-sitter.ts:439-447; extraction-core.md §2
  shows the orchestrator always init-loads first). `neededLangs` = set of
  `detectLanguage(f)` over the fixtures (+`cpp` whenever `c` present). (2) iterate the
  fixture tree calling `extractFromSource(relPath, source, detectLanguage(relPath))` —
  import from `../codegraph/src/extraction/tree-sitter.js`; if `npx tsx` ESM resolution
  balks at the `.js` specifier, import the `.ts` source path directly
  (`src/extraction/tree-sitter.ts` — tsx resolves TS sources). Run as
  `cd ../codegraph && npx tsx <abs script> <abs fixtures dir> <abs out.json>` so
  codegraph's deps resolve. (3) write per-file
  `{ nodesByKind, edgesByKind, refsByKind, nodeCount, edgeCount, refCount }` sorted-key
  JSON — but **REFUSE to write** `expected.json` (exit non-zero, print the offending
  files) if ANY fixture yields 0 nodes or any parser/grammar error: that is a broken
  harness, not a parity baseline. Run once; commit `expected.json` AND record the
  codegraph commit hash it was generated from at the top of the results doc.
- [ ] `tests/parity_gate.rs`: for every fixture file, run Rust `extract_from_source`,
  compare every counter against `expected.json`. **Tolerance = 0 (exact match).**
  Justification (document in the results doc): extraction is deterministic over
  byte-identical inputs, so any drift is either a config bug or a grammar-version
  divergence — exactly what the gate exists to catch; a blanket nonzero tolerance would
  mask config drift forever. Justified divergences go in `deviations.toml` instead:
  entries `{ fixture, counter, ts, rust, reason }` — each MUST name its cause (e.g.
  "kotlin-ng parses `fun interface` cleanly; TS WASM grammar misparsed → +1 interface
  node", or "TS bug not ported, see extraction-langs.md §port notes"). The gate asserts
  `rust == ts` OR an exact matching deviation entry exists; stale deviation entries
  (matching no observed diff) FAIL the gate.
- [ ] Record in `docs/benchmarks/2026-07-phase2-extract-parity.md`: per-language totals
  (TS vs Rust nodes/edges/refs), deviation count + summary, codegraph commit, grammar
  crate versions. **If deviations exceed ~2% of any language's node count, STOP and
  surface to the maintainer — that is a grammar-parity failure, not a tolerance.**
- [ ] Commit: `test(extract): TS↔Rust extraction count-parity gate on shared fixtures`

### Task 20: Facade polish

- [ ] `src/lib.rs`: crate docs (role, PRD §3/§6, the WASM-layer-deletion note, deferred
  list: standalone extractors + wave-2 langs → Phase 8, sync/watch → Phase 6, framework
  extractors + resolution → Phase 3), re-export the public surface
  (`extract_from_source`, `Language`, `detect_language`, `scan_directory`, `ScopeIgnore`,
  `Indexer`, `IndexResult`, `ExtractionResult`, error types).
- [ ] `cargo doc -p selene-extract` builds warning-free; full workspace green:
  `cargo fmt --check && cargo clippy --all-targets && cargo test`.
- [ ] Update CLAUDE.md status line (selene-extract implemented for v0) if drifted.
- [ ] Commit: `feat(extract): selene-extract public facade`

## Self-review checklist (after Task 20)

- Every extraction-core.md §Public interface item has a Rust equivalent OR an explicit
  deferral noted in lib.rs (parse-pool/worker protocol → deleted with the WASM layer;
  `sync`/`getChangedFiles` → Phase 6; `loadGrammarsForLanguages`/WASM-bytes APIs →
  obsolete; standalone extractors + `extractFromSource` non-v0 dispatch arms → Phase 8;
  framework `fw.extract` append step → Phase 3).
- No constant drifted: MAX_FILE_SIZE 1 MiB, MAX_VALUE_REF_NODES 20 000, batch
  `max(4, workers*2)`, workers clamp(cores-1,1,8) cap 16, embedded-repo 4/2000/100,
  git timeouts 5/10/30 s.
- node_id golden test pins exact bytes; file node id is the unhashed literal; the
  `kind:` prefix present on every hashed id.
- All 12 grammar crates pinned `=x.y.z`; every v0 language has (a) ported TS assertions
  and (b) at least one insta snapshot; the parity gate is green with every deviation
  justified in `deviations.toml`.
- Extraction emits ZERO cross-file edges (grep the walker: edges only between ids in the
  current file's result); `function_ref` never appears as an `Edge.kind`.
- All ExtractionError codes used somewhere reachable; no `unwrap`/`expect` outside tests;
  errors collected never thrown (a fixture with a syntax error still yields partial
  nodes + a `parse_error`).
- Determinism: `index_all` twice on the same tree ⇒ identical ids/stats (test exists);
  pre_parse blankers byte-offset-exact under multi-byte UTF-8 (test exists).
- EXTRACTION_VERSION = 1 documented with bump rule; persisted via meta and checked as
  guidance, never an error.

## Open coordination points (surfaced to maintainer; do not silently resolve)

1. **`replace_file_extraction` on the trait:** today an inherent `SurrealStore` method;
   Phase 1 Task 10 may lift it onto `GraphStore`. Task 18 codes against the concrete
   store and switches to the trait if it has been lifted — whichever is true when the
   task runs.
2. **kotlin-ng node-name drift** vs the WASM kotlin grammar the TS maps describe:
   expected source of parity deviations; budgeted in Tasks 1/11/19.
3. **EXTRACTION_VERSION restarts at 1** (TS was 24) — decided above on disjoint-store
   grounds; flag to maintainer in case cross-engine tooling was ever intended.
4. **Env prefix rename** `CODEGRAPH_*` → `SELENE_*` — assumed; only PARSE_WORKERS and
   VALUE_REFS survive the WASM deletion.
