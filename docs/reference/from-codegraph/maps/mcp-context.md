# MCP + Context Map

## File inventory

| Path | LOC | Responsibility |
|---|---|---|
| `src/mcp/tools.ts` | 4685 | Tool definitions + `ToolHandler` (all 7 tool handlers, budgets, flow builder, ranking, formatting) — the heart of the subsystem |
| `src/mcp/index.ts` | 489 | `MCPServer` — start-mode decision (direct / proxy / daemon), detached daemon spawn, PPID watchdog wiring |
| `src/mcp/transport.ts` | 436 | Newline-delimited JSON-RPC 2.0 transports: `StdioTransport`, `SocketTransport`, shared `LineBasedJsonRpcTransport` |
| `src/mcp/session.ts` | 350 | Per-connection protocol state machine: `initialize` / `tools/list` / `tools/call` / `roots/list`, instructions-variant pick |
| `src/mcp/engine.ts` | 334 | `MCPEngine` — shared heavyweight state (CodeGraph handle, watcher, ToolHandler, lazy init, optional worker QueryPool) |
| `src/mcp/dynamic-boundaries.ts` | 398 | `scanDynamicDispatch` — regex detection of dynamic-dispatch sites over comment/string-stripped bodies (#687) |
| `src/mcp/server-instructions.ts` | 103 | `SERVER_INSTRUCTIONS` + `SERVER_INSTRUCTIONS_NO_ROOT_INDEX` — the single source of agent guidance |
| `src/mcp/proxy.ts` | 596 | Proxy mode: local handshake, stdio↔daemon-socket pipe, in-process fallback |
| `src/mcp/daemon.ts` | 867 | Shared detached daemon: O_EXCL lock, socket server, client refcount + idle timeout |
| `src/mcp/daemon-paths.ts` / `daemon-registry.ts` / `daemon-manager.ts` | 140/199/117 | Socket candidate paths (tmpdir fallback #997), registry, lifecycle |
| `src/mcp/query-pool.ts` / `query-worker.ts` | 326/103 | Worker-thread pool off-loading read tools in daemon mode |
| `src/mcp/ppid-watchdog.ts` / `early-ppid.ts` / `liveness-watchdog.ts` / `startup-handshake.ts` / `stdin-teardown.ts` | 95/25/242/71/46 | Process-lifecycle guards (#277, #850, #1185, #799) |
| `src/mcp/version.ts` | 36 | `CodeGraphPackageVersion` (real package version in `serverInfo`) |
| `src/context/index.ts` | 1372 | `ContextBuilder` — hybrid search (`findRelevantContext`), code extraction, call-paths section, low-confidence handoff |
| `src/context/formatter.ts` | 290 | `formatContextAsMarkdown` / `formatContextAsJson` / `formatSubgraphTree` |
| `src/context/markers.ts` | 19 | `LOW_CONFIDENCE_MARKER = '### ⚠️ Low-confidence match'` — dependency-free sentinel shared with MCP layer |

## Public interface

```ts
// src/mcp/index.ts
class MCPServer { constructor(projectPath?: string); start(): Promise<void>; stop(): void }
export { StdioTransport, tools, ToolHandler, Daemon, CodeGraphPackageVersion }

// src/mcp/tools.ts
export class NotIndexedError extends Error {}     // → SUCCESS-shaped guidance
export class PathRefusalError extends Error {}    // → isError, no retry note
export function normalizeQuerySpelling(query: string): string
export function getExploreBudget(fileCount: number): number
export interface ExploreOutputBudget { maxOutputChars; defaultMaxFiles; maxCharsPerFile; gapThreshold;
  maxSymbolsInFileHeader; maxEdgesPerRelationshipKind; includeRelationships; includeAdditionalFiles;
  includeCompletenessSignal; includeBudgetNote; excludeLowValueFiles }   // all number/boolean
export function getExploreOutputBudget(fileCount: number): ExploreOutputBudget
export function formatStaleBanner(stale: PendingFile[]): string
export function formatStaleFooter(stale: PendingFile[]): string
export function formatDegradedBanner(reason: string | null): string
export interface ToolDefinition { name; description; inputSchema: {type:'object'; properties; required?}; annotations? }
export interface ToolAnnotations { title?; readOnlyHint?; destructiveHint?; idempotentHint?; openWorldHint? }
export interface ToolResult { content: Array<{type:'text'; text:string}>; isError?: boolean }
export const tools: ToolDefinition[]              // 8 defs: search, callers, callees, impact, node, explore, status, files
export function getStaticTools(): ToolDefinition[] // allowlist-filtered, no engine needed (proxy pre-open tools/list)
export class ToolHandler {
  constructor(cg: CodeGraph | null)
  setQueryPool(pool: QueryPool | null); setDefaultCodeGraph(cg); setCatchUpGate(p: Promise<void>|null)
  setDefaultProjectHint(searchedPath: string); hasDefaultCodeGraph(): boolean
  getTools(): ToolDefinition[]                     // dynamic descriptions + tiny-repo gating + required-projectPath variant
  execute(toolName: string, args: Record<string, unknown>): Promise<ToolResult>       // gates + banners
  executeReadTool(toolName, args): Promise<ToolResult>                                // worker entry, no banners
  closeAll(): void
}

// src/mcp/session.ts
export const SERVER_INFO = { name: 'codegraph', version: CodeGraphPackageVersion }
export const PROTOCOL_VERSION = '2024-11-05'
export function initializeInstructions(base: string, notice?: string | null): string
export class MCPSession { constructor(transport: JsonRpcTransport, engine: MCPEngine, opts?: {explicitProjectPath?: string|null});
  start(): void; stop(): void; getTransport(): JsonRpcTransport }

// src/mcp/transport.ts
export interface JsonRpcTransport { start(handler); stop(); send(resp); notify(method, params?);
  request(method, params?, timeoutMs?=5000): Promise<unknown>; sendResult(id, result); sendError(id, code, message, data?) }
export const ErrorCodes = { ParseError:-32700, InvalidRequest:-32600, MethodNotFound:-32601, InvalidParams:-32602, InternalError:-32603 }
export class StdioTransport implements JsonRpcTransport  // opts { exitOnClose=true, onClose }
export class SocketTransport implements JsonRpcTransport  // + onClose(handler), writeRaw(line)

// src/mcp/dynamic-boundaries.ts
export interface BoundaryMatch { form; label; snippet; line; key?; keyIsType?; moreSites? }
export function scanDynamicDispatch(body: string, language: string, fileStartLine: number): BoundaryMatch[]
export function blankStringContents(text: string): string

// src/context/index.ts
export class ContextBuilder {
  constructor(projectRoot: string, queries: QueryBuilder, traverser: GraphTraverser)
  buildContext(input: TaskInput, options?: BuildContextOptions): Promise<TaskContext | string>
  findRelevantContext(query: string, options?: FindRelevantContextOptions): Promise<Subgraph>  // Subgraph gains confidence?: 'high'|'low'
  getCode(nodeId: string): Promise<string | null>
}
export function createContextBuilder(...): ContextBuilder
export { formatContextAsMarkdown, formatContextAsJson, LOW_CONFIDENCE_MARKER }
```

`CodeGraph` methods the handlers call (the trait surface selene-db/graph must provide): `getStats()` (`{fileCount,nodeCount,edgeCount,dbSizeBytes,nodesByKind,filesByLanguage}`), `searchNodes(q,{limit,kinds})`, `getNodesByName`, `getNodesByNamePrefix(p,limit)`, `getNodesInFile`, `getNode(id)`, `getCallers(id)`/`getCallees(id)` (→ `{node,edge}[]`), `getIncomingEdges(id)`/`getOutgoingEdges(id,[kinds]?)`, `getImpactRadius(id,depth)`, `findRelevantContext`, `getCode(id)`, `getChildren(id)`, `getFiles()` (`{path,language,nodeCount}[]`), `getFileDependents(path)`, `getProjectRoot()`, `getProjectNameTokens()`, `getJournalMode()`, `getPendingFiles()`, `getPendingReferenceCount()`, `isWatcherDegraded()`, `getWatcherDegradedReason()`, `reopenIfReplaced()`, `openSync(root)`.

## Key algorithms & data flow

**Session protocol.** Wire = newline-delimited JSON-RPC 2.0 over stdin/stdout (or a Unix socket / named pipe in daemon mode). Methods handled: `initialize`, `initialized` (no-op), `tools/list`, `tools/call`, `ping` (→`{}`), `resources/list` (→`{resources:[]}`), `resources/templates/list` (→`{resourceTemplates:[]}`), `prompts/list` (→`{prompts:[]}`) — the empty-list replies exist so client probes never see `-32601` (#621). Anything else → `-32601`. Malformed JSON → `-32700` with `id:null`. Server-initiated `roots/list` (id prefix `cg-srv-N`, timeout 5000 ms) resolves the workspace root when the client passed no `rootUri`; fallback order: in-flight init → `roots/list` (one-shot) → `process.cwd()` → sync re-walk (`retryInitializeSync`, picks up post-start `init`). `initialize` responds **before** any heavy init (#172) with `{protocolVersion:'2024-11-05', capabilities:{tools:{}}, serverInfo, instructions}`; the instructions variant is picked by a cheap sync walk-up: nearest `.codegraph/` found from `rootUri`/`workspaceFolders[0]`/`--path`/cwd → `SERVER_INSTRUCTIONS`, else `SERVER_INSTRUCTIONS_NO_ROOT_INDEX`. Update notice (if cached) is appended: `` `${base}\n\n---\n${notice} This server keeps running the old version until the user upgrades — mention it when convenient; do not run the upgrade yourself.` ``

**Start-mode decision (`MCPServer.start`)**: (1) `CODEGRAPH_DAEMON_INTERNAL=1` → become the daemon (O_EXCL lock, `TAKEOVER_MAX_RETRIES=5`, delay 100 ms); (2) `CODEGRAPH_NO_DAEMON` truthy → direct; (3) no `.codegraph/` reachable → direct; (4) else proxy: answer handshake locally, connect-or-spawn detached daemon in background (poll 240×25 ms ≈ 6 s), fall back in-process on failure. Direct mode installs SIGINT/SIGTERM, stdin-close/error teardown (#799), startup-handshake timeout (`CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS`, #1185), PPID watchdog (`CODEGRAPH_PPID_POLL_MS`, #277), main-thread liveness watchdog (#850).

**Tool visibility (`getTools`)**: default surface is **explore ONLY** (`DEFAULT_MCP_TOOLS = {'explore'}`). `CODEGRAPH_MCP_TOOLS` (comma-separated short names, `codegraph_` prefix stripped) replaces the default entirely. With a project open: tiny-repo gate — `fileCount < 500` (`TINY_REPO_FILE_THRESHOLD`) restricts to `{codegraph_explore, codegraph_search, codegraph_node}`; explore's description gets `" Budget: make at most ${budget} calls for this project (${fileCount.toLocaleString()} files indexed)."` appended. With **no** default project (#993): every tool exposing `projectPath` gets it added to `required` (pure schema clone). Every tool carries `annotations: {readOnlyHint:true, destructiveHint:false, idempotentHint:true, openWorldHint:false}` (#1018). Tools are **always** listed even at an un-indexed root (#964).

**Dispatch (`ToolHandler.execute`)**: (1) await catch-up gate once, time-boxed `DEFAULT_CATCHUP_GATE_TIMEOUT_MS=3000` (`CODEGRAPH_CATCHUP_GATE_TIMEOUT_MS`, 0 = unbounded, #905); (2) allowlist check; (3) length validation — free-form strings `MAX_INPUT_LENGTH=10_000`, path-likes `MAX_PATH_LENGTH=4_096`; (4) `codegraph_status` runs on the main thread and skips banners; (5) other tools optionally run on the worker pool; (6) result wrapped with worktree-mismatch notice (#155, cached per `(startPath,indexRoot)`) then staleness notice (#403: per-file banner for pending paths substring-matched in the response, footer max 5 others, or the degraded banner #876). Catch: `NotIndexedError → textResult` (success-shaped), `PathRefusalError → errorResult`, else `errorResult("Tool execution failed: ${msg}. This is an internal codegraph error — retry the call once; if it persists, continue without codegraph for this task.")`. Project resolution: re-walk nearest `.codegraph/` every call (#926), `validateProjectPath` only on existing paths (#238), connection cache keyed by resolved root, `reopenIfReplaced()` self-heal (#925), default instance reused when paths coincide.

**Budgets** (invariants — do not regress):
- `getExploreBudget(fileCount)`: `<500→1, <5000→2, <15000→3, <25000→4, else 5`.
- `getExploreOutputBudget(fileCount)` tiers:

| tier | maxOutputChars | defaultMaxFiles | maxCharsPerFile | gapThreshold | maxSymbolsInFileHeader | maxEdgesPerRelKind | relationships/additional/completeness/budgetNote | excludeLowValueFiles |
|---|---|---|---|---|---|---|---|---|
| `<150` | 13000 | 4 | 3800 | 7 | 5 | 4 | all false | true |
| `<500` | 18000 | 5 | 3800 | 8 | 6 | 6 | all false | true |
| `<5000` | 24000 | 8 | 6500 | 12 | 10 | 10 | all true | false |
| `<15000` | 24000 | 8 | 7000 | 15 | 15 | 15 | all true | false |
| `≥15000` | 24000 | 8 | 7000 | 15 | 15 | 15 | all true | false |

**Monotonicity invariant: a larger tier must never get a smaller `maxCharsPerFile`.** All tiers cap at ~24K because the host externalizes inline results >~25K. Final hard ceiling `min(round(maxOutputChars*1.5), 25000)`, cut at the last `\n**\`` file-section boundary if it lies past 50% of the ceiling, appending: `... (output truncated to budget; the source above is complete and verbatim — treat it as already Read. For any area not covered, run another codegraph_explore with the specific names — do NOT Read these files.)`. Other constants: `MAX_OUTPUT_LENGTH=15000` (generic `truncateOutput`, cut at last newline if >80% in), node-mode overload `BODY_BUDGET=12000` / `HARD_CAP=16` bodies / `LIST_CAP=20`; file-view `CHAR_BUDGET=38000`, `DEFAULT_LIMIT=2000` lines.

**`handleExplore` pipeline** (query first passes `normalizeQuerySpelling`: `fn/3→fn` via `/\b([A-Za-z_][\w@]*)\/(\d{1,3})(?=$|[\s,()[\]/])/g`; `mod:fn→mod.fn` via `/(^|[\s,()[\]])(?!(?:kind|lang|language|path|name):)([a-z_][\w@]*):([A-Za-z_][\w@]*)(?=$|[\s,()[\]])/g`):
1. `findRelevantContext(query, {searchLimit:8, traversalDepth:3, maxNodes:200, minScore:0.2})`; empty → `No relevant code found for "${query}"` (success-shaped).
2. **Glue**: callers/callees of root nodes in already-surfaced files, cap 60.
3. **Named-symbol seeding**: tokenize on `[\s,()[\]]+`, strip file extensions (regex over 30+ extensions), keep tokens ≥3 chars matching `^[A-Za-z_$][\w$]*(?:(?:::|\.)[\w$]+)*$`, max 16. Per token: qualified → `findAllSymbols`, bare → `getNodesByName` (full enumeration beats FTS cut, tokio `poll`); filter CALLABLE={method,function,component,constructor}, non-test path (`/(^|\/)(tests?|specs?|__tests__|testdata|mocks?|fixtures?)\//i` or `\.(test|spec)\.[a-z]+$`), sort substantive-first. NL-stopword guard: bare lowercase words seed only when co-named (another query token is a symbol in the same file); shape-precise tokens (`[._$]|::|/`, camelCase, leading capital) seed unconditionally. ≤3 defs → all picked; tier = most-substantive + co-named defs with callers ≥ 25% of max. >3 defs → overloads whose file/qualifiedName contains a PascalCase query token (`^[A-Z][A-Za-z0-9]{3,}`, project-name tokens excluded #720), cap 4, else single most-substantive.
4. **File scoring**: named seed +50, entry +10, connected-to-entry +3, else +1; skip import/export and config-leaf nodes (#383 secret guard); keep files with score ≥3.
5. Test/low-value hard-exclude on all tiers unless query matches `\b(test|tests|testing|spec|verify|verifies)\b/i`, only if ≥2 non-test files remain. `isLowValue` regexes include `/\/(tests?|__tests?__|spec)\//`, `_test.go$`, `test_*.py`, `_spec.rb`, `\.(test|spec)\.[jt]sx?$`, `\bicons?\b`, `\bi18n\b`, etc.
6. **RWR ranking** (`computeGraphRelevance`): undirected adjacency over edge kinds `{calls,references,extends,implements,overrides,instantiates,returns,type_of,imports}`, restart α=0.25 to seeds, 25 power iterations, dangling keeps mass. Central files = top-2 by mass with ≥1 term hit.
7. **Change-surface rescue** (#1064): signature types (`references|type_of|returns` edges) of tier seeds whose file is buried (graph mass < maxGraph×0.06 AND term hits <2) get injected, score max 45, force-kept and tiered.
8. **Relevance gate**: keep file iff mass ≥ maxGraph×0.06 OR central OR entry OR change-surface OR ≥2 distinct term hits; never prune below 2 files.
9. **Sort**: named-seed files first → corroborated (entry/central + ≥2 term hits, disable with `CODEGRAPH_RANK_NO_MULTITERM=1`) → graph mass (epsilon maxGraph×0.01) → term hits → low-value last → generated last → score → node count.
10. **`buildFlowFromNamedSymbols`** (Flow section): resolve ≤16 tokens; ambiguous simple name (>3 callable defs) kept only if its container segment (2nd-last of qualifiedName split on `::|.`) is in the query's segment pool (**co-naming disambiguation**); keep ≤6 per token, `named` cap 40; `uniqueNamedNodeIds` = tokens with ≤3 defs (drives spares + overload bias). Non-callable `{constant,variable,field,property}` endpoints with a heuristic edge go to `dynNamed` (cap 12, per-token 4). BFS over `calls` edges from ≤8 seeds, `MAX_HOPS=7`, frontier cap 1500, **`MAX_BRIDGE=1` — at most 1 consecutive unnamed hop** (never wander a god-function's fan-out); accept only named sinks; longest chain wins; renders only if ≥3 nodes. `spineCallSites` maps spine node → line of its call to the next hop (windows god-methods). Supplements: **dynamic-dispatch links** (≤6 heuristic edges incident to named/dynNamed, skipping in-chain hops), **dynamic boundaries** (#687: `buildDynamicBoundaries`, `MAX_NOTES=4`, `MAX_SCAN=8` bodies, `MAX_TOTAL_CHARS=200_000`, fires only for uncovered tokens, scans dead end first then unique-named-first; `boundaryCandidates` probes `key`, `on${Cap}`, `handle${Cap}`, `${key}Handler`, `handle_${key}` (type keys: `${key}Handler`, `key`), then FTS limit 12, normalized-containment filter, ≤4 shown, handler methods `/^(handle|handleAsync|execute|executeAsync|consume|consumeAsync|run|__invoke)$/i`), and **polymorphic boundaries** (`POLY_MIN_FAMILY=8`, `MIN_SUPPORT=2`, `SAMPLE=40`, `MAX_NOTES=3`, `MIN_IMPL=8` — ranked by true graph-wide implementer count, not sample frequency).
11. **Source rendering** per file: budget-90% soft stop for incidental files (necessary = defines entry/spine/unique-named symbol; those render past cap). **Adaptive sizing** (`CODEGRAPH_ADAPTIVE_EXPLORE`, default on): skeletonize iff spine exists AND (on-spine god-file with off-path named bodies > `maxCharsPerFile` OR (off-spine AND polymorphic sibling (`MIN_SIBLINGS=3` implementers of a shared supertype) AND not spared)); spared = unique-named callable in file UNLESS file defines a ≥3-impl supertype. Focused view: bodies by priority (spine 0, unique-named 1, family-base co-named 2) greedy under `bodyCap = maxCharsPerFile*1.5`; rest as signature lines (scan ≤4 lines forward past decorators; `SIG_MAX = max(12, maxSymbolsInFileHeader*2)`). **Whole-file rule**: ≤`WHOLE_FILE_MAX_LINES` (central 280 / other 220) and ≤ char cap (central `min(remaining, maxCharsPerFile*1.5)`, other `maxCharsPerFile*3`) → dump whole. Else **clustering**: ranges from nodes (skip envelope containers spanning >50% of file; `ENVELOPE_KINDS` = file,module,class,struct,interface,enum,namespace,protocol,trait,component) + edge-source lines (importance 2); importance entry 10 / named 9 / glue 6 / connected 3 / else 1; merge within `gapThreshold`; rank spine-first → maxImportance → density → score → span; select under `fileBudget = min(maxCharsPerFile, remaining)`, spine clusters up to `SPINE_CEILING = min(maxCharsPerFile*2.5, remaining)`; always take top-1. Oversize spine cluster (>`OVERSIZE_SPINE_LINES=200`) windowed to `SPINE_WINDOW=28` lines around the call site + ≤5-line signature head. Context padding 3 lines; gap marker `'\n\n... (gap) ...\n\n'` (language-neutral). Line numbers `<n>\t<line>` (cat -n; disable `CODEGRAPH_EXPLORE_LINENUMS=0`).
12. Header sentinel `[[codegraph-explore-summary]]` replaced post-truncation with `Found N symbols across M files.` counted from surviving sections (#1046). Blast-radius section (`ROOT_CAP=5`, `FILE_CAP=4`, `⚠️ no covering tests found` when none). Never tells the agent to Read.

**`handleNode`**: file-mode (`file` without `symbol`) = Read parity — exact/suffix/substring resolution (ambiguous → list ≤25 candidates), `offset`/`limit` semantics identical to Read, config-leaf languages return keys only (#383), reads through `validatePathWithinRoot` (#527). Symbol-mode: `findSymbolMatches` (bare name → `getNodesByName` full enumeration, generated files last; qualified → FTS then `matchesSymbol` — suffix match on `::`-joined qualifiedName or file-path segment containment with `RUST_PATH_PREFIXES={crate,super,self}` stripped; qualified with no exact match returns [], #173); `file`/`line` narrowing (line prefers containing def, else nearest start); 1 match → details + optional body (containers = `CONTAINER_NODE_KINDS` {class,struct,interface,trait,protocol,enum,namespace,module} get a member outline instead) + **trail** (`TRAIL_CAP=12` callees/callers with `file:line`, synth edges annotated); N matches with `includeCode` → all bodies packed under BODY_BUDGET.

**callers/callees/impact**: `findAllSymbols` (FTS limit 50, colon-fallback re-search by tail, nix option-path special case `^[a-z][\w'-]*(?:\.[\w'-]+)+$`), grouped into distinct definitions by `(filePath,qualifiedName)` (#764), optional `file` narrowing (miss → note + show all), per-definition sections when >1 group, limits clamped 1–100 (impact depth 1–10, default 2).

**ContextBuilder.findRelevantContext** (used by explore step 1): extract symbols from query (camelCase/snake_case/SCREAMING/acronym/dotted/lowercase≥3 patterns minus a ~100-word stopword list) → exact-name lookup with co-location boost (+20/extra co-named symbol per file) → TitleCase prefix search over definition kinds (+15+brevity) → per-term FTS with multi-term boost (+5/extra hit) → test-file dampen ×0.3 → dominant-file core-dir boost +25 (when dominant edge count ≥3× next) → term-group co-occurrence rerank (≥2 groups → ×(1+0.5n); distinctive exact matches exempt; common-word exact matches ×0.3; else ×0.6) → CamelCase-boundary LIKE matches (limit 200, score `8+brevity+pathScore`, scaled ×(1+termCount)+30(termCount−1)) → compound ≥2-term LIKE matches (`10+20(terms−1)+path+brevity`) → sort, slice `searchLimit*3`, min-score filter, resolve imports→definitions, cap to `searchLimit` roots → confidence low iff ≥2 terms and no result with 2 term hits or a distinctive name → type-hierarchy expansion (budget maxNodes/4, 2 passes for siblings) → BFS both directions per root (limit maxNodes/roots) → trim to maxNodes (roots + neighbors prioritized) → per-file cap `max(5, ceil(maxNodes*0.2))` → non-prod cap `max(3, ceil(maxNodes*0.15))` → edge recovery via `findEdgesBetweenNodes` over `{calls,extends,implements,references,overrides}`. Defaults: build `{maxNodes:20,maxCodeBlocks:5,maxCodeBlockSize:1500,searchLimit:3,traversalDepth:1,minScore:0.3}`; find adds `nodeKinds=HIGH_VALUE_NODE_KINDS` (excludes import/export/parameter/etc.). `buildContext` appends `## Call paths` (DFS over subgraph `calls` edges, MAX_HOPS=6, budget 2000, chains ≥3 nodes with ≥2 roots, keep ≤3 non-subpath, synth hops annotated `→[callback via \`x\` @file:line]`) and the low-confidence handoff.

## Wire/contract surfaces

**`SERVER_INSTRUCTIONS` (verbatim; sent in `initialize` when the root is indexed):**

````
# Codegraph — code intelligence over an indexed knowledge graph

Codegraph is a SQLite knowledge graph of every symbol, edge, and file in
the workspace — pre-computed structure you would otherwise re-derive by
reading files (cached intelligence: thousands of parse/trace decisions you
don't pay to re-reason each run). Reads are sub-millisecond; the index lags
writes by ~1s through the file watcher. Reach for it BEFORE *and* while
writing or editing code — not just for questions: one call returns the
verbatim source PLUS who calls it and what it affects, so you edit with the
blast radius in view. More accurate context, in far fewer tokens and
round-trips than reading files yourself.

## One tool: codegraph_explore — use it instead of reading files

There is a single tool, `codegraph_explore`, and it is Read-equivalent. It
takes either a natural-language question or a bag of symbol/file names and
returns the **verbatim, line-numbered source** of the relevant symbols
grouped by file — the same `<n>\t<line>` shape `Read` gives you, safe to
`Edit` from — PLUS the call path among them (including dynamic-dispatch hops
like callbacks, React re-render, and JSX children that grep can't follow) and
a blast-radius summary of what depends on them.

Whether you're answering "how does X work" or implementing a change (fixing a
bug, adding a feature), call `codegraph_explore` before you Read. ONE call
usually answers the whole question. Codegraph IS the pre-built search index —
so running your own grep + read loop, or delegating the lookup to a separate
file-reading sub-task/agent, repeats work codegraph already did and costs more
for the same answer. A direct codegraph answer is typically one to a few
calls; a grep/read exploration is dozens.

## How to query

- **Almost any question — "how does X work", architecture, a bug, "what/where is X", or surveying an area** → `codegraph_explore` with a natural-language question or the relevant names. ONE capped call returns the verbatim source grouped by file; most often the ONLY call you need.
- **"How does X reach/become Y? / the flow / the path from X to Y"** → `codegraph_explore`, naming the symbols that span the flow (e.g. `mutateElement renderScene`) — it surfaces the call path among them, riding dynamic-dispatch hops, and returns their source.
- **Reading or editing a file/symbol you can name** → put its name or file path in the `codegraph_explore` query — it returns that current line-numbered source (safe to `Edit` from) with the call path and blast radius attached, so you don't Read it separately. For an overloaded name it returns every matching definition's body in one call.
- **Need more?** Call `codegraph_explore` again with more specific names — treat the source it returns as already Read.

## Anti-patterns

- **Trust codegraph's results — don't re-verify them with grep.** They come from a full AST parse; re-checking with grep is slower, less accurate, and wastes context.
- **Don't grep or Read first** to find or understand indexed code — ONE `codegraph_explore` returns the relevant symbols' source together in a single round-trip. Reach for raw `Read`/`Grep` only to confirm a specific detail codegraph didn't cover, or for what codegraph doesn't index (configs, docs).
- **Don't reconstruct a flow by hand** — name the endpoints in one `codegraph_explore` and it surfaces the path between them, dynamic-dispatch hops included.
- **After editing, check the staleness banner.** When a tool response starts with "⚠️ Some files referenced below were edited since the last index sync…", the listed files are pending re-index — Read those specific files for accurate content. Every file NOT in that banner is fresh, so still trust codegraph. A different, rarer banner — "⚠️ CodeGraph auto-sync is DISABLED…" — means live watching stopped entirely (the whole index is frozen, not just a few files); until it's resolved, Read files directly to confirm anything that may have changed.

## Limitations

- If a tool reports a project isn't indexed (no `.codegraph/`), stop calling codegraph tools for that project for the rest of the session and use your built-in tools there instead. Indexing is the user's decision — mention they can run `codegraph init` if it comes up, but don't run it yourself.
- Index lags file writes by ~1 second.
- Cross-file resolution is best-effort name matching; ambiguous calls may return multiple candidates.
- No live correctness validation — that's still the TypeScript compiler / test suite / linter's job. Codegraph supplements those with structural context they don't have.
````

**`SERVER_INSTRUCTIONS_NO_ROOT_INDEX` (verbatim; sent when the server's root has no index):**

````
# Codegraph — available (per-project; pass projectPath)

Codegraph is a SQLite knowledge graph of a codebase's symbols, edges, and
files: one `codegraph_explore` call returns the verbatim, line-numbered source
of the relevant symbols PLUS the call paths between them and a blast-radius
summary — replacing a grep + Read loop with one round-trip.

This server started somewhere with no `.codegraph/` of its own, so there is no
default project — but the tools are available and work **per project**:

- To query a project that HAS a `.codegraph/` index (e.g. a service inside a
  monorepo, or a second repo), pass its path as `projectPath` to
  `codegraph_explore` (and any other codegraph tool). Codegraph resolves the
  nearest `.codegraph/` at or above that path and answers from it — for as many
  projects as you like in one session.
- For a project with no `.codegraph/`, use your built-in tools (Read/Grep/Glob)
  for that project. Indexing is the user's decision — don't run it yourself, but
  if it comes up they can run `codegraph init` in a project to enable codegraph
  there (a new index is picked up live, no restart).
````

**isError contract (hard invariant):** `isError:true` ONLY for `PathRefusalError` (sensitive path), input-validation failures (`"Error: ${name} must be a non-empty string"`, length overruns), disabled-tool calls, unknown tool names, and genuine malfunctions (which carry the retry-once note). ALL of these are success-shaped: not indexed (both no-default and explicit-path variants — long guidance texts quoted in `getCodeGraph`), `No results found for "${q}"`, `Symbol "${s}" not found in the codebase`, `No callers/callees found for "${s}"`, `No relevant code found for "${q}"`, `No files indexed. Run \`codegraph index\` first.`, file-not-matched, ambiguous-file lists, offset-past-end.

**Exact output markers agents/tests key on:** staleness banner starts `⚠️ Some files referenced below were edited since the last index sync — their codegraph entries may be stale:` with lines `  - ${path} (edited ${ms}ms ago, ${'indexing in progress'|'pending sync'})` and tail `For accurate content of those specific files, Read them directly. The rest of this response is fresh.`; footer `(Note: N file(s) elsewhere in this project are pending index sync but were not referenced above: …)` (max 5 + `…and N more`); degraded banner `⚠️ CodeGraph auto-sync is DISABLED — live file watching stopped, so the index is frozen and any file edited since then is stale here. Read files directly to confirm current content before relying on it.` + optional `  Reason: ${reason}`. File section header: `` **`path`** — suffix `` (`FILE_SECTION_PREFIX = '**\`'` — unique greppable truncation boundary; no ATX headings anywhere, #778). Explore leads with `**Exploration: ${query}**`, source preamble `> The code below is the **verbatim, current on-disk source** …`, Flow section `**Flow (call path among the symbols you queried)**` with numbered steps `${i}. name (file:line)` and `   ↓ calls` / `   ↓ dynamic: …` arrows. Synth edge labels (from `metadata.synthesizedBy`): `callback`, `event-emitter`, `react-render`, `jsx-render`, `vue-handler`, `interface-impl`, `closure-collection`, `fn-pointer-dispatch`, `goframe-route`, plus generic fallback `${kind.replace(/-/g,' ')} (dynamic dispatch)`; compact forms like `dynamic: callback via \`x\` @file:line`. Search results `**Search Results (N found)**`; node lists `- name (kind) - file:line — via label`; impact `**Impact: "${s}" affects N symbols**`; status fields (`**Files indexed:**`, `**Backend:** node:sqlite (Node built-in) — full WAL + FTS5`, `**Journal mode:** wal (concurrent reads safe)`, …). ContextBuilder markdown: `## Code Context`, `### Entry Points`, `### Related Symbols` (≤10), `### Code`, `## Call paths`, `### ⚠️ Low-confidence match` (the `LOW_CONFIDENCE_MARKER` sentinel). Read-parity numbering: `` `${n}\t${line}` `` no padding, trailing empty line kept.

**Env vars:** `CODEGRAPH_MCP_TOOLS`, `CODEGRAPH_NO_DAEMON`, `CODEGRAPH_DAEMON_INTERNAL`, `CODEGRAPH_MCP_DEBUG`, `CODEGRAPH_EXPLORE_LINENUMS`, `CODEGRAPH_ADAPTIVE_EXPLORE`, `CODEGRAPH_RANK_NO_MULTITERM`, `CODEGRAPH_CATCHUP_GATE_TIMEOUT_MS`, `CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS`, `CODEGRAPH_PPID_POLL_MS`, `CODEGRAPH_QUERY_POOL_SIZE`, `CODEGRAPH_NO_UPDATE_CHECK`, `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`, `CODEGRAPH_WASM_RELAUNCHED`, `CODEGRAPH_HOST_PPID`.

## Test coverage

Contract tests that **must** be ported: `explore-output-budget.test.ts` (tier values, boundary file counts off-by-one, monotonicity, meta-text gating, line numbers on/off, language-neutral gap markers, envelope filter, `normalizeQuerySpelling` incl. field-prefix and Lua cases); `mcp-unindexed.test.ts` (**the isError policy**: unindexed → success-shaped, per-project instructions variant, tools listed at unindexed root #964, monorepo `projectPath` reach-through); `mcp-initialize.test.ts` (handshake answered before init #172, empty `resources/prompts` lists #621); `mcp-tool-annotations.test.ts` (read-only annotations on every surface, survive schema clone + dynamic description #1018); `mcp-require-project-path.test.ts` (#993 required-projectPath surface); `mcp-tool-allowlist.test.ts`; `mcp-staleness-banner.test.ts` (#403 banner/footer text); `mcp-catchup-gate.test.ts` (#905 timeout); `node-file-view.test.ts` (9 cases, **byte-for-byte Read parity** incl. `^1000\t  const v998` unpadded and trailing-newline handling); `adaptive-explore-sizing.test.ts` (7 cases: named-callable spare, supertype-family override); `explore-blast-radius.test.ts`, `explore-corroboration-ranking.test.ts`, `explore-nl-stopword-collision.test.ts`, `explore-result-count.test.ts` (#1046 curated header), `explore-synth-constant-endpoints.test.ts` (#687 RTK constants); `dynamic-boundaries.test.ts` (form regexes, string/comment blanking); `mcp-files-path-normalization.test.ts` (#426 `/`, `.`, `./`, backslash filters); `context.test.ts` + `context-ranking.test.ts` (buildContext markdown/JSON shape, maxNodes, truncation, entry points, ranking). Roots/daemon/watchdog behavior: `mcp-roots.test.ts`, `mcp-daemon.test.ts`, `mcp-ppid-watchdog.test.ts`, `mcp-startup-orphan.test.ts`. All use real temp dirs + real SQLite, no mocks.

## Rust port notes

- **Crate placement:** transports + session + engine + tool schemas/handlers → `selene-mcp`; explore ranking/flow/clustering logic is large enough to split — the flow builder, RWR, budgets, and cluster assembly could live in `selene-context` (they are pure functions over the graph API) with `selene-mcp` owning only schemas, dispatch, banners, and error classification. `ContextBuilder`/formatter → `selene-context`. `scanDynamicDispatch` → `selene-resolve` or `selene-context` (it reuses the resolver's comment stripper). Daemon/proxy/watchdogs are Node-process-model workarounds — in Rust a single static binary with tokio + a Unix-socket listener collapses most of `daemon.ts`/`proxy.ts`/`query-pool.ts` (no worker-thread pool needed; use a blocking-task pool), but keep the socket-path candidate walk (#997 ExFAT/WSL2 fallback) and the version-hello handshake.
- **Depends on the `GraphStore` trait freeze (PRD §5.4):** the handlers call ~25 fine-grained graph methods (listed above) plus `findRelevantContext`; decide which become SurrealQL vs portable primitives before porting `handleExplore` — RWR, BFS-with-bridge-cap, and cluster selection are cheap in code and should stay code (they run over an already-bounded subgraph).
- **TS idioms needing redesign:** pervasive `try { } catch { /* best-effort */ }` swallowing → `Result` + explicit "never break a tool call" combinators; JS regex features used: sticky `lastIndex` loops, `\b` word boundaries (Unicode differences — use `regex` crate with care), replacement `$1$2.$3`; float sort comparators with epsilon ties; `toLocaleString()` in the budget note (thousands separators — pick a fixed format and pin it in tests); `Map` insertion-order iteration is relied on in grouped output ordering (Rust: `IndexMap`); `string.includes` substring staleness matching; JSON-RPC ids may be string or number.
- **Wire fidelity:** the instructions strings, banner texts, tool names/descriptions/schemas, `PROTOCOL_VERSION '2024-11-05'`, annotations object, and the `<n>\t<line>` numbering are the compatibility contract — byte-for-byte. Keep the `[[codegraph-explore-summary]]` sentinel + post-truncation substitution pattern.
- **Suspicious/dead code:** `getTools()` comment says "the default 4-tool surface" but `DEFAULT_MCP_TOOLS` is `{'explore'}` (stale comment — the set is authoritative); `formatSubgraphTree` in formatter.ts appears unused by MCP (only CLI/legacy — verify before porting); in `buildFlowFromNamedSymbols` the `named.size > 40` break exits the token loop mid-way (later tokens silently unresolved — port as-is, it's the shipped behavior); `findAllSymbols`'s `exactMatches.length <= 1` branch silently falls back to `results[0]` (fuzzy) for callers/callees even for qualified misses, unlike `findSymbolMatches` (#173 fixed only node-mode) — intentional divergence, keep it; `handleFileView`'s `CHAR_BUDGET=38000` comment references "explore's proven-safe ~38k ceiling" which predates the 24K/25K externalization cap — file-view results CAN exceed the inline cap (known, accepted for Read parity).
- **Doc reconciliation:** `adaptive-explore-sizing.md` matches the code (spare/override/per-symbol focused view/uniqueness gate all present; the doc's "skeleton … Read for a full body" header text is stale — code now says `codegraph_explore a name for its full body; do NOT Read`). `agent-codegraph-adoption.md`: P1 resolved via file-view Read parity (shipped, hook rejected — do not port a Read-deny hook); P2 resolved via `getStaticTools` local `tools/list` — the Rust port should preserve instant handshake + static tool list decoupled from index open.
- **Security invariants to keep:** `validatePathWithinRoot` on every disk read (#527), config-leaf key-only rendering (#383), `validateProjectPath` refusal → isError, input length caps (10 000 / 4 096).
