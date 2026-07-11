# Tests + Benchmarks Map

## File inventory

All paths relative to `codegraph/`. 135 `*.test.ts` files (flat `__tests__/`, plus `__tests__/integration/` and `__tests__/evaluation/`). Families are grouped where files share one pattern.

| Path | LOC | Responsibility |
|---|---|---|
| `__tests__/extraction.test.ts` | 10,941 | Golden extractor suite: language detection, per-language node/edge extraction (TS, Python, Go, Rust, Java, C#, PHP, Swift, Kotlin, Dart, C/C++/CUDA/Metal, Pascal/Delphi, Erlang, Nix, ArkTS…), imports per language, docstring capture, C++ macro-recovery regressions |
| `__tests__/resolution.test.ts` | 4,597 | Name matcher, import resolver, tsconfig path aliases, JVM FQN imports, re-export chains, chained static-factory call resolution (one describe per language, #645/#608 mechanism), same-name disambiguation (#1079), receiver-type inference (#1108), ubiquitous-name ceiling (#999) |
| `__tests__/frameworks.test.ts` | 1,743 | Per-framework route extraction units: Django/Flask/FastAPI/Express/NestJS/Laravel/Rails/Spring/Play/Gin/GoFrame/Axum-cargo/ASP.NET/Vapor/React Router/Svelte/Astro; commented-out routes ignored |
| `__tests__/frameworks-integration.test.ts` | 1,338 | End-to-end framework resolution on synthetic projects (regression anchor — do not rename) |
| `__tests__/installer-targets.test.ts` | 1,711 | ~95 parameterized contract tests across all 8 agent targets × global/local (see below) |
| `__tests__/installer.test.ts` | 104 | Legacy installer helpers |
| `__tests__/pr19-improvements.test.ts` | 719 | Regression bundle for PR #19 (name anchors to git history — do not rename) |
| `__tests__/graph.test.ts` | 615 | GraphTraverser/QueryManager: traverse, callers/callees, impact radius, findPath, ancestors/children, circular deps, dead code, node metrics, traversal limits (#1086–#1090) |
| `__tests__/context.test.ts` | 374 | ContextBuilder: getCode, findRelevantContext, buildContext markdown/JSON structure |
| `__tests__/context-ranking.test.ts` | 189 | Context relevance ranking |
| `__tests__/security.test.ts` | 687 | FileLock, path-traversal + symlink-escape prevention (#527), sensitive-dir blocking, MCP input validation, atomic writes, JSON.parse error boundaries, symlink cycles |
| `__tests__/adaptive-explore-sizing.test.ts` | 394 | Sibling skeletonization in `codegraph_explore` (MIN_SIBLINGS=3, on-spine exemplar full, `· skeleton`/`· focused` markers, `CODEGRAPH_ADAPTIVE_EXPLORE=0` escape) |
| `__tests__/explore-output-budget.test.ts` | 280 | Pins per-tier explore budgets (#185): tier breakpoints, monotonicity, gated meta-text, line numbers, language-neutral omission markers, `normalizeQuerySpelling` |
| `__tests__/explore-*.test.ts` (blast-radius, corroboration-ranking, nl-stopword-collision, result-count, synth-constant-endpoints) | ~500 | Explore ranking/output edge cases |
| `__tests__/mcp-unindexed.test.ts` | 265 | #964 no-root-index policy: tools always listed, success-shaped guidance, isError reserved |
| `__tests__/mcp-initialize.test.ts` | 183 | initialize responds before heavy init (#172, 30s handshake) |
| `__tests__/mcp-tool-allowlist.test.ts` | 63 | `CODEGRAPH_MCP_TOOLS` allowlist; default surface = explore only |
| `__tests__/mcp-*.test.ts` (roots, daemon, catchup-gate, staleness-banner, ppid-watchdog, startup-orphan, require-project-path, files-path-normalization, tool-annotations, debounce-env) | ~1,700 | Spawned-binary JSON-RPC contract tests over stdio |
| `__tests__/integration/full-pipeline.test.ts` | 272 | init → indexAll → resolve → query → sync on a generated ~120-module TS project; parse errors don't abort batch |
| `__tests__/integration/mcp-input-limits.test.ts` / `lru-cache.test.ts` | 205 | Input size limits; project LRU cache |
| Synthesizer suite: `c-fnptr`, `celery-dispatch`, `closure-collection`, `erlang-behaviour`, `laravel-event`, `mediatr-dispatch`, `nix-option`, `object-registry`, `pinia-store`, `redux-thunk`, `rtk-query`, `sidekiq-dispatch`, `spring-event`, `vuex-dispatch`, `swift-objc-bridge(-resolver)`, `react-hoc-component`, `react-native-bridge`, `rn-event-channel`, `gin-middleware-chain`, `dynamic-boundaries`, `synthesis-tail-scaling` | ~3,400 | Each: temp project with the dispatch pattern → index → assert synthesized edges by `metadata.synthesizedBy` via SQL, incl. precision negatives (no cross-bleed, non-matching project is no-op) |
| `__tests__/value-reference-edges.test.ts` | 724 | `references` edges with `metadata.valueRef` from readers to file-scope consts |
| `__tests__/function-ref.test.ts` | 790 | Callback/value function-reference edges |
| Language-specific resolution: `arkts-resolution`, `cfml-inheritance/receiver`, `php-property-receiver`, `lombok`, `drupal`, `goframe`, `expo-modules`, `fabric-view`, `mybatis-extractor-robustness`, `vue-store-extraction`, `object-literal-methods`, `ts-field-classification` | ~2,600 | Per-language/framework extraction+resolution regressions |
| Daemon suite: `daemon-*` (manager, registry, socket-fallback, bind-failure, client-liveness, attach-log), `ppid-watchdog`, `liveness-watchdog`, `index-orphan-watchdog`, `stdin-teardown`, `startup-handshake`, `concurrent-locking`, `proxy-connect` | ~1,800 | Process lifecycle: socket bind/fallback, PPID watchdog, orphan reaping, lock contention |
| DB suite: `sqlite-backend` (44), `node-sqlite-backend` (71), `db-perf` (361), `db-reopen-on-replace`, `wal-deferral`, `query-pool`, `iterate-nodes-by-kind`, `orphaned-refs-sweep` | ~1,300 | Backend pin (`getBackend()==='node-sqlite'`, WAL on), perf floors, reopen-on-replace |
| Scanning/config: `include-config`, `exclude-config`, `include-ignored-config`, `android-res-exclusion`, `generated-detection`, `is-test-file`, `extension-mapping`, `worktree-detection`, `multi-repo-workspace`, `unsafe-index-root` | ~1,600 | File-discovery rules, .gitignore semantics, monorepo/worktree detection |
| CLI: `cli-version`, `cli-node-command`, `cli-query-command`, `cli-affected-paths`, `index-command`, `status-json`, `node-file-view`, `symbol-lookup`, `search-query-parser`, `identifier-segments`, `segment-vocab`, `same-name-disambiguation`, `strip-comments`, `glyphs`, `cooperative-yield` | ~1,900 | CLI output contracts (esp. `status --json` fields), FTS query parser, identifier segmentation |
| Sync/watch: `sync.test.ts` (759), `watcher.test.ts` (712), `watch-policy`, `git-hooks`, `frontload-hook` | ~1,900 | Incremental sync add/modify/remove, debounce, git-hook install |
| Packaging/runtime: `npm-shim`, `npm-sdk`, `node-version-check`, `wasm-runtime-flags`, `grammar-wasm-bytes`, `remove-binary`, `install-sh-prune`, `update-check`, `upgrade`, `prepare-release`, `telemetry`, `config-secret-redaction`, `fatal-handler`, `parse-pool`, `subprocess-timeouts`, `foundation` | ~3,200 | Distribution mechanics (mostly TS/npm-specific) |
| `__tests__/evaluation/{runner,scoring,test-cases,types}.ts` | 335 | Retrieval-quality eval harness (not vitest; run via `npx tsx` against a pre-indexed repo) |

## Public interface

What the tests consume (this *is* the API surface a Rust port must expose to write equivalent tests):

```ts
// src/index.ts
class CodeGraph {
  static init(path: string, opts?: { index?: boolean; silent?: boolean; config?: {include: string[]; exclude: string[]} }): Promise<CodeGraph>;
  static initSync(path, opts): CodeGraph;
  static openSync(path): CodeGraph;
  indexAll(): Promise<void>;  close(): void;  destroy(): void;
  searchNodes(query: string, opts?: { limit?: number; kinds?: NodeKind[] }): Array<{ node: Node; score: number }>;
  findRelevantContext(query: string, opts: { searchLimit; traversalDepth; maxNodes; minScore }): Promise<{ nodes: Map<string,Node>; edges: Edge[]; roots: string[] }>;
  getNode(id): Node | undefined;  getIncomingEdges(id): Edge[];
  getCallers/getCallees/getImpactRadius/…
}

// src/mcp/tools.ts
class ToolHandler {
  constructor(cg: CodeGraph | null);          // null = no project loaded
  getTools(): Array<{ name: string; … }>;
  execute(name: string, args: object): Promise<{ content: [{ text: string }]; isError?: boolean }>;
}
function getExploreBudget(fileCount: number): number;                       // call budget 1..5
function getExploreOutputBudget(fileCount: number): {
  maxOutputChars; defaultMaxFiles; maxCharsPerFile; maxSymbolsInFileHeader; gapThreshold;
  includeAdditionalFiles; includeCompletenessSignal; includeBudgetNote; includeRelationships: boolean };
function normalizeQuerySpelling(q: string): string;

// src/extraction
function extractFromSource(fileName: string, source: string): { nodes: Node[]; edges: Edge[] };  // pure, no FS/DB
function detectLanguage(path: string, contents?: string): string;  isSourceFile(path): boolean;

// src/installer/targets
interface AgentTarget { id; supportsLocation(l:'global'|'local'): boolean;
  detect(l): { alreadyConfigured: boolean };
  install(l, { autoAllow }): { files: Array<{ path: string; action: 'created'|'updated'|'unchanged' }> };
  uninstall(l): void;  printConfig(l): string;  describePaths(l): string[] }
const ALL_TARGETS: AgentTarget[];  // claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro

// __tests__/evaluation
const PASS_THRESHOLD = 0.5;
scoreSearchNodes(caseId, expectedSymbols, results, latencyMs): EvalResult;   // recall + MRR
scoreFindRelevantContext(caseId, expectedSymbols, subgraph, latencyMs): EvalResult; // recall + edgeDensity
```

## Key algorithms & data flow

**Fixture strategy — no fixture directories at all.** Every test creates `fs.mkdtempSync(os.tmpdir() + prefix)`, writes **inline source strings** (a handful of small files encoding exactly the pattern under test, each body carrying a unique sentinel like `BRIDGE_BODY_MARKER`), indexes with the real pipeline against **real SQLite** (no DB mocking anywhere), asserts, then `rmSync` in `afterEach`. Pure-extraction tests skip the FS entirely via `extractFromSource('file.ts', code)`. Integration generates a synthetic ~120-module chain project programmatically (module N imports and calls N−1). Only `evaluation/` and the A/B scripts use *external* real repos (pre-indexed; path via `EVAL_CODEBASE`).

**Explore budget constants (pinned by tests + CLAUDE.md — the contract):**
- `getExploreBudget(files)`: `<500→1, <5000→2, <15000→3, <25000→4, ≥25000→5` calls.
- Output tiers: `<150` very-tiny, `150–499` small, `500–4999` medium, `5000–14999` large, `≥15000` xlarge. Small tiers step **~13k → 18k → 24k** `maxOutputChars`; medium and large **share** the ~24k inline ceiling (tests pin `≥20000 && ≤25000`; very-tiny `≤20000`); scaling beyond that lives in the *call* budget, never a fatter response (a >~25k response gets externalized by the host and Read back — measured regression).
- **Invariant:** `maxCharsPerFile` monotonic non-decreasing with tier (`<5000` tier once had 2500 < `<500`'s 3800 → forced Reads on god-file repos).
- Tiers `<500`: `includeAdditionalFiles=false, includeCompletenessSignal=false, includeBudgetNote=false, includeRelationships=false`; `≥500`: all true. Smaller `maxSymbolsInFileHeader` and `gapThreshold` on small tiers.
- Boundary tests assert exact off-by-one behavior at 149/150, 499/500, 4999/5000, 14999/15000.

**Skeletonization (adaptive explore):** a file collapses to a signature skeleton iff it is (a) off the synthesized flow spine AND (b) a polymorphic sibling — its class implements/extends a supertype with **≥ MIN_SIBLINGS = 3** implementers. On-spine exemplar stays full. A file whose *callable* the agent named stays full; naming only the *type* doesn't spare it; naming a *shared* polymorphic method (5 defs) doesn't spare siblings. A base-class+subclasses family file collapses to `· focused` (named base method body kept, non-named subclass bodies → signatures). `CODEGRAPH_ADAPTIVE_EXPLORE=0` disables.

**`normalizeQuerySpelling`:** `mod:fn/arity` (Erlang) → `mod.fn`; bare `init/2` → `init`; Lua `logger:log` → `logger.log`; must NOT touch query-language fields (`kind:`, `lang:`, `path:`, `name:`), `Foo::bar`, URLs, `12:30`, `src/2fa/handler.ts`.

**Eval harness:** for each case (query → expected symbols), call `searchNodes` (limit 10) or `findRelevantContext` (defaults `searchLimit:8, traversalDepth:3, maxNodes:80, minScore:0.2`); score recall = found/expected (case-insensitive name match), MRR = 1/rank-of-first-found-expected; **pass = recall ≥ 0.5**; print table, write JSON report to `evaluation/results/<ISO-ts>.json`, exit 1 on any failure.

**A/B methodology SeleneCode must be able to re-run** (from the three benchmark docs):
1. **With/without matrix** (`codegraph-ab-matrix.md`): headless `claude -p "<flow question>" --output-format stream-json --verbose --permission-mode bypassPermissions --strict-mcp-config --mcp-config <cfg> --max-budget-usd 4`; **with** arm = codegraph-only MCP config `{"mcpServers":{"codegraph":{"command":…,"args":["serve","--mcp","--path",<repo>]}}}`, **without** = `{"mcpServers":{}}`. Same model + prompt; the graph server is the only variable. Neutralize any prompt hook (`CODEGRAPH_NO_PROMPT_HOOK=1`) in both arms. Fresh re-index per cell. Parse the stream-json (`parse-run.mjs`): tool sequence, Read/Grep/Glob/Bash/Task counts, codegraph-call count, duration, cost. Matrix = language × S/M/L real repos, one canonical flow question each. **Standing model policy: `--model sonnet --effort high` on both arms, ≥2 runs/arm** (variance is large; never conclude from n=1).
2. **Call-sequence mining** (`call-sequence-analysis.md`): re-mine the same jsonl logs for per-call **payload size** and sequence. Ablation arms are built with the server-side **`CODEGRAPH_MCP_TOOLS` allowlist** (tool genuinely absent from ListTools, not denied-on-call). Key measurement gotcha to preserve: **sum per-turn assistant `usage`** — `result.usage` is last-turn-only. Headline the port must be able to reproduce: 7-repo README bench, median of 4 runs/arm → **35% cost / 57% tokens / 46% time / 71% tool-calls saved**. Hard-won verdicts encoded there: *sufficiency stops the agent; steering doesn't* (arms G/H regressed; arm I — body-inlining trace + destination callees — was the only shippable win); MCP instructions/tool-descriptions cannot deliver append-prompt salience; connectivity (dyn-dispatch coverage) is the multiplier.
3. **Interactive/delegation A/B** (`answer-directly-vs-explore-agent.md`): drive the real TUI via tmux (`itrun.sh`) — headless spawns 0 Explore agents so it can't measure delegation. n=3/arm, parse main **and** sub-agent transcripts, sum reads across both; metric = main-session context (found scale-invariant ~50k with the graph server).
4. **Deterministic probes** (pre-A/B gate): `probe-{explore,node,trace}.mjs` against the built binary — flow connects end-to-end in explore's Flow section; `select count(*) from nodes` stable across re-index (no node explosion); `provenance='heuristic'` edge precision spot-check via SQL.
5. **New-vs-baseline** (`ab-new-vs-baseline.sh`): both arms codegraph-on, different builds; bakes in daemon pre-warm (`CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` high, wait for `daemon.sock`, `CODEGRAPH_WASM_RELAUNCHED=1` to skip re-exec) so MCP attach latency doesn't contaminate results. Optional forced-Read-0 sufficiency proof via block-read hook (`hook-settings.json`).

## Wire/contract surfaces

Exact strings/shapes tests pin — the Rust port must not drift:

- **Explore output**: flow header `**Flow (call path among the symbols you queried)`; file sections open `` **`<path>`** `` (bold labels, NOT `####` ATX headings — issue #778); skeleton tag contains `· skeleton (signatures only`; focused tag `· focused`; gated meta-text strings `### Additional relevant files`, `Complete source code is included above`, `Explore budget:`; source lines prefixed `<lineno>\t` (cat -n style) by default, `CODEGRAPH_EXPLORE_LINENUMS=0` disables; omission markers must be language-neutral — never `// ... (gap)` / `// ... trimmed`; output must never say "use Read".
- **isError semantics** (the single most important invariant): unindexed `projectPath` → success + text matching `/isn't indexed/`, `/codegraph init/`, `/built-in tools/`; no default project → `/No CodeGraph project is loaded/` + `/projectPath/`; sensitive path (`/etc`) → `isError: true` and must NOT contain `retry the call once`; genuine malfunctions carry a retry-once note; invalid input (null/empty query, non-string symbol) → `isError: true` + `non-empty string`; `limit` clamped to 100; oversized output truncated with a sentinel.
- **MCP surface**: `tools/list` non-empty even at unindexed root; default listed set is exactly `['codegraph_explore']` (`DEFAULT_MCP_TOOLS`); `CODEGRAPH_MCP_TOOLS` accepts short or `codegraph_`-qualified names, trims whitespace, empty ⇒ unset; disabled tool on execute → isError `disabled via CODEGRAPH_MCP_TOOLS`. `initialize` result carries `instructions`: indexed root ⇒ full playbook (contains `How to query`); unindexed root ⇒ per-project variant (mentions `projectPath`, `codegraph_explore`, `codegraph init`; NOT `/inactive/i`, NOT `## How to query`). initialize must respond **before** heavy init (30s host timeout). Protocol version used in tests: `2025-11-25`; transport is newline-delimited JSON-RPC on stdio.
- **Installer contract** (per target × location): install writes files & flips `detect().alreadyConfigured`; re-install byte-identical (every file action `unchanged`); sibling MCP entries survive (`mcpServers.other`; opencode shape is `mcp: { name: { type:'local', command:[…], enabled:true } }` in `.jsonc` preferred); uninstall restores `alreadyConfigured=false`; `printConfig` writes nothing. Legacy instructions block delimited by `<!-- CODEGRAPH_START -->` / `<!-- CODEGRAPH_END -->` is stripped on install/uninstall. Codex: TOML `[mcp_servers.codegraph]` with sibling tables preserved verbatim, plus `~/.codex/AGENTS.md` `## CodeGraph` block containing `codegraph explore`. Cursor gets `--path` injected (absolute local / `${workspaceFolder}` global). HOME redirect in tests is via `HOME`/`USERPROFILE`/`APPDATA`/`XDG_CONFIG_HOME` env.
- **CLI**: `status --json` prints exactly one JSON line to stdout with fields `version`, `indexPath`, `lastIndexed` (plus backend id `node-sqlite` in TS — Selene will report its own backend string).
- **DB/edges**: synthesized edges carry `provenance:'heuristic'` + `metadata.synthesizedBy` (e.g. `'fn-pointer-dispatch'`) + `registeredAt`; value refs carry `metadata.valueRef: true`. Tests query these via raw SQL (`json_extract(e.metadata,'$.synthesizedBy')`).
- **Eval JSON report**: `{ timestamp, codebasePath, codegraphSha, summary:{total,passed,failed,meanRecall,meanMRR}, results:[{caseId,pass,recall,mrr,foundSymbols,missedSymbols,nodeCount?,edgeCount?,edgeDensity?,latencyMs}] }`.
- **Env vars that are contract**: `CODEGRAPH_ADAPTIVE_EXPLORE`, `CODEGRAPH_EXPLORE_LINENUMS`, `CODEGRAPH_MCP_TOOLS`, `CODEGRAPH_VALUE_REFS`, `CODEGRAPH_NO_DAEMON`, `CODEGRAPH_NO_PROMPT_HOOK`, `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`, `CODEGRAPH_TELEMETRY` (Selene equivalents should keep 1:1 semantics under a `SELENE_` prefix, with the A/B harness updated in lockstep).

## Test coverage

- **Vitest config quirks**: `globals: true` (bare `describe/it`); suite-wide `env: { CODEGRAPH_ALLOW_UNSAFE_NODE:'1', CODEGRAPH_TELEMETRY:'0' }` so spawned binaries don't hard-exit on unsupported Node or pollute real `~/.codegraph`; include `__tests__/**/*.test.ts`. Spawn-based suites hard-code `BIN = dist/bin/codegraph.js` ⇒ **build before test**. Quirk: `npm run test:eval` targets `__tests__/evaluation/` which contains **no `.test.ts` files** — it's effectively dead; the real entry is `npm run eval` (tsx runner against a pre-indexed external repo). Windows-specific behavior is gated with `it.runIf(process.platform === 'win32')` (and POSIX inverse); known pre-existing Windows failures: symlink test (privileges) and mcp-initialize/mcp-roots teardown EPERM (child holds cwd/SQLite handle).
- **Highest-value suites to port (contract tests, in priority order):** (1) `extraction.test.ts` per-language golden assertions — the wire contract of NodeKind/EdgeKind/name/signature/docstring per language; (2) `resolution.test.ts` — chained-call resolution matrix + disambiguation ceilings; (3) `installer-targets.test.ts` — the full 8-target contract matrix; (4) `explore-output-budget.test.ts` + `adaptive-explore-sizing.test.ts` — budget tiers, monotonicity invariant, skeletonization gates; (5) `mcp-unindexed.test.ts` + `mcp-tool-allowlist.test.ts` + `security.test.ts` (MCP Input Validation) — the isError-reservation policy; (6) the synthesizer family (one file per dispatch channel, incl. precision negatives); (7) `integration/full-pipeline.test.ts`; (8) `status-json`, `sqlite-backend` (as "backend pin" for SurrealDB), `sync`/`watcher`.
- **Skip/TS-only:** `npm-shim`, `npm-sdk`, `node-version-check`, `wasm-runtime-flags`, `grammar-wasm-bytes` (native grammars in Rust), `remove-binary`, `upgrade`, `prepare-release`, `parse-pool` (worker threads), `cooperative-yield` (event-loop yielding).

## Rust port notes

- **Fixture layout**: keep the inline-source-in-tempdir strategy — it's what makes 135 suites hermetic and parallel. Use `tempfile::TempDir` + a small builder helper (`write("src/a.ts", indoc!{…})`) in a shared **`selene-test-support`** dev-only crate (path dep, not published). Do NOT invent checked-in sample repos; CodeGraph never needed them below the eval layer.
- **Crate placement**: extraction goldens → `selene-extract/tests/`; resolution/chained-call/synthesizers → `selene-resolve/tests/`; traversal → `selene-graph/tests/`; budget-tier unit tests → in-module `#[cfg(test)]` in `selene-mcp` (pure functions); explore/node output + skeletonization + isError policy → `selene-mcp/tests/` driving `ToolHandler` in-process; spawned JSON-RPC handshake tests → `selene/tests/` using `env!("CARGO_BIN_EXE_selene")` (replaces the `dist/bin` path dependence — no pre-build step needed); installer matrix → `selene-installer/tests/` parameterized with `rstest` over `ALL_TARGETS × [Global, Local]`; full pipeline → a workspace-level `tests/` integration crate or `selene/tests/`.
- **Snapshots (`insta`)**: use snapshots for `server-instructions`, tool descriptions, and the `status --json` shape — full-text contracts. Do **not** snapshot whole explore outputs: the TS tests deliberately assert *stable markers* (`· skeleton (signatures only`, section headers, sentinel absence) precisely because wording churns; port those as substring/regex assertions, or the suite becomes a churn amplifier.
- **Env-var mutation is the biggest TS idiom to redesign.** TS tests freely mutate `process.env` (`CODEGRAPH_ADAPTIVE_EXPLORE`, `CODEGRAPH_MCP_TOOLS`, `CODEGRAPH_EXPLORE_LINENUMS`) and `process.chdir`/`$HOME` between tests — vitest runs files in isolated workers so this is safe there. Rust tests share one process across threads: read these knobs **once into an injectable config struct** (constructor parameter on `ToolHandler`/budget functions), with env only as the production default; installer targets must take explicit `home_dir`/`cwd` (never `std::env::set_current_dir` in tests). Reserve `serial_test` for the few真 spawn tests.
- **Platform gating**: `#[cfg(unix)]`/`#[cfg(windows)]` replaces `it.runIf(process.platform…)`; the `/etc`-is-sensitive assertion is POSIX-only.
- **Eval + A/B**: port `evaluation/` as an `xtask`/`cargo run -p selene-eval` binary (never inside `cargo test`), keeping `PASS_THRESHOLD=0.5`, the recall/MRR scoring, and the JSON report shape byte-compatible so historical results stay comparable. Port `scripts/agent-eval/` shell + parser scripts nearly verbatim (they drive `claude`, not node) — only the MCP config command changes to the selene binary; keep the sonnet/effort-high policy, ≥2 runs/arm, sum-per-turn token accounting, allowlist-driven ablation arms, and the daemon pre-warm recipe.
- **Looks buggy/dead in TS** (don't blindly port): (a) `evaluation/scoring.ts` MRR uses the rank of the *first expected symbol found in expected-list order*, not the best rank over all expected symbols — replicate exactly if comparability matters, but flag it; (b) `runner.ts` mean-MRR filter `r.mrr > 0 || caseId.startsWith('search-')` silently couples scoring to case-ID naming; (c) `npm run test:eval` matches zero files (dead script); (d) `installer-targets` sibling-preservation test silently `return`s (passes) for targets without a JSON config — in Rust make that an explicit `rstest` skip so coverage gaps are visible.
- **DB assertions**: several suites reach into raw SQL (`json_extract(metadata,'$.synthesizedBy')`) through a private handle (`(cg as any).db.db`). Give `GraphStore` a first-class test affordance instead (e.g., `edges_by_synthesizer(name)` / a `#[cfg(feature="test-util")]` raw-query hook) — SurrealQL vs SQL differences make private-handle poking non-portable, and this choice feeds the PRD §5.4 trait-freeze decision.
