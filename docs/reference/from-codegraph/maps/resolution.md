# Resolution Core Map

Subsystem: `/Users/messaoudabdelatif/IdeaProjects/AI_playGround/codegraph/src/resolution/` — orchestrator, import resolution, name matching, chained calls. Excludes `frameworks/` and the synthesizers (`callback-synthesizer.ts`, `c-fnptr-synthesizer.ts`, `goframe-synthesizer.ts`, `swift-objc-bridge.ts`).

## File inventory

| path | LOC | responsibility |
|---|---|---|
| `src/resolution/index.ts` | 1832 | `ReferenceResolver` orchestrator: caches, built-in filters, strategy pipeline, batching, deferred conformance passes, edge creation/persistence |
| `src/resolution/types.ts` | 272 | `UnresolvedRef`, `ResolvedRef`, `ResolutionResult`, `ResolutionContext`, `FrameworkResolver`, `ImportMapping`, `ReExport` |
| `src/resolution/name-matcher.ts` | 2030 | All name-based strategies: exact/qualified/method/fuzzy/file-path/function-ref matching, receiver-type inference, chained-call resolution, language families, scoring |
| `src/resolution/import-resolver.ts` | 2092 | Module-specifier → file resolution per ecosystem, import-mapping extraction (regex), re-export chasing, JVM/Go/Python/Rust/Lua/PHP/C-C++/COBOL/Nix specializations |
| `src/resolution/path-aliases.ts` | 242 | tsconfig/jsconfig `compilerOptions.paths` loading (JSONC-tolerant) + alias rewriting |
| `src/resolution/workspace-packages.ts` | 324 | npm/yarn/bun/pnpm workspace member map + ohpm (`oh-package.json5`) `file:` deps; bare-specifier → member-dir rewrite |
| `src/resolution/go-module.ts` | 47 | `go.mod` `module` directive → `GoModule { modulePath, rootDir }` |
| `src/resolution/lru-cache.ts` | 62 | Insertion-ordered-Map LRU (`get` refreshes recency; `set` evicts oldest) |
| `src/resolution/cooperative-yield.ts` | 41 | `createYielder(budgetMs=250)` → `maybeYield()` that `setImmediate`-yields only when >250 ms elapsed (liveness-watchdog heartbeat, #850/#1091) |
| `src/resolution/strip-comments.ts` | 528 | `stripCommentsForRegex` — used by frameworks/synthesizers + MCP, not by the core files above |

## Public interface

```ts
// index.ts
export class ReferenceResolver {
  constructor(projectRoot: string, queries: QueryBuilder);
  initialize(): void;                         // detectFrameworks + clearCaches
  runPostExtract(): number;                   // per-framework postExtract → updateNode; returns count
  warmCaches(): void;  async warmCachesYielding(onYield: MaybeYield): Promise<void>;
  clearCaches(): void;
  resolveAll(refs: UnresolvedReference[], onProgress?): ResolutionResult;
  resolveOne(ref: UnresolvedRef): ResolvedRef | null;
  createEdges(resolved: ResolvedRef[]): Edge[];
  resolveAndPersist(refs: UnresolvedReference[], onProgress?): ResolutionResult;
  async resolveAndPersistListYielding(refs: UnresolvedReference[]): Promise<ResolutionResult>;
  async resolveAndPersistBatched(onProgress?, batchSize = 5000): Promise<ResolutionResult>;
  async resolveChainedCallsViaConformance(): Promise<number>;   // drains deferredChainRefs
  async resolveDeferredThisMemberRefs(): Promise<number>;       // drains deferredThisMemberRefs
  getDetectedFrameworks(): string[];
}
export function createResolver(projectRoot: string, queries: QueryBuilder): ReferenceResolver;

// types.ts
interface UnresolvedRef { fromNodeId; referenceName; referenceKind: ReferenceKind; line; column; filePath; language; candidates?: string[] }
interface ResolvedRef { original: UnresolvedRef; targetNodeId: string; confidence: number;
  resolvedBy: 'exact-match'|'import'|'qualified-name'|'framework'|'fuzzy'|'instance-method'|'file-path'|'function-ref' }
interface ResolutionResult { resolved; unresolved; stats: { total; resolved; unresolved; byMethod: Record<string,number> } }
interface ResolutionContext { getNodesInFile; getNodesByName; getNodesByQualifiedName; getNodesByKind; iterateNodesByKind?;
  fileExists; readFile; getFileLines?; getMethodMatches?; getProjectRoot; getAllFiles; getNodesByLowerName; getSupertypes?;
  getNodeById?; getImportMappings; getProjectAliases?; getGoModule?; getWorkspacePackages?; getReExports?; listDirectories?; getCppIncludeDirs? }
interface ImportMapping { localName; exportedName; source; isDefault; isNamespace; resolvedPath? }
type ReExport = { kind:'named'; exportedName; originalName; source } | { kind:'wildcard'; source }

// name-matcher.ts
export function matchReference(ref, ctx): ResolvedRef | null;           // strategy dispatcher
export function matchByFilePath / matchByExactName / matchByQualifiedName / matchMethodCall / matchFuzzy / matchFunctionRef;
export function matchCppCallChain / matchScopedCallChain / matchDottedCallChain;
export function resolveMethodOnType(typeName, methodName, ref, ctx, confidence, resolvedBy, preferredFqn?, depth=0);
export function preferCallSiteFile(nodes: Node[], callSiteFile: string): Node[];
export function sameLanguageFamily(a,b): boolean;  isKnownLanguageFamily(l): boolean;  crossesKnownFamily(a,b): boolean;

// import-resolver.ts
export function resolveImportPath(importPath, fromFile, language, ctx): string | null;
export function resolveViaImport(ref, ctx): ResolvedRef | null;
export function resolveJvmImport(ref, ctx): ResolvedRef | null;
export function extractImportMappings(filePath, content, language): ImportMapping[];
export function extractReExports(content, language): ReExport[];
export function loadCppIncludeDirs(projectRoot): string[];  clearCppIncludeDirCache(); clearImportMappingCache();
export function isPhpIncludePathRef / isCobolCopybookRef / isNixPathImportRef (ref): boolean;

// path-aliases.ts
export interface AliasPattern { prefix; suffix; hasWildcard; replacements: string[] }
export interface AliasMap { baseUrl: string /*abs*/; patterns: AliasPattern[] }
export function loadProjectAliases(projectRoot): AliasMap | null;
export function applyAliases(importPath, aliases, projectRoot): string[];

// workspace-packages.ts
export interface WorkspacePackages { byName: Map<string,string>; entryByName?: Map<string,string> }
export function loadWorkspacePackages(projectRoot): WorkspacePackages | null;
export function resolveWorkspaceImport(importPath, ws): string | null;

// go-module.ts
export interface GoModule { modulePath: string; rootDir: string }
export function loadGoModule(projectRoot): GoModule | null;
```

## Key algorithms & data flow

### Pass ordering (driven by `CodeGraph` in `src/index.ts`)
1. Extraction writes nodes + `unresolved_refs` rows. Then `resolver.initialize()` (re-detect frameworks against the populated index) and `resolver.runPostExtract()` (framework cross-file finalization; persists via `updateNode`, `id` must be preserved, caches cleared before/after).
2. `resolveAndPersistBatched()` — the main pass.
3. `resolveChainedCallsViaConformance()` — second pass over in-memory `deferredChainRefs` now that `implements`/`extends` edges exist (#750).
4. `resolveDeferredThisMemberRefs()` — second pass for inherited `this.<member>` function refs (#808).
Sync: scoped `resolveAndPersist(getUnresolvedReferencesByFiles(changed))` → failed-ref retry `resolveAndPersistListYielding(getRetryableFailedReferences(namesOfChangedFiles))` (#1240) → orphan sweep (`getUnresolvedReferencesCount() > 0` → batched pass, #1187) → both conformance passes.

### Batching (`resolveAndPersistBatched`, batchSize **5000**)
- `warmCachesYielding`: `knownFiles = Set(getAllFilePaths())`; `knownNames` streamed via `iterateNodeNames()` yielding every 8192 names (`(++scanned & 8191) === 0`).
- Loop: `getUnresolvedReferencesBatch(0, 5000)` — **always offset 0** (processed rows leave the pending set: resolved rows deleted, unresolvable flipped to `status='failed'`). Per-ref `await maybeYield()` (250 ms budget).
- Persistence per batch in `PERSIST_CHUNK = 1000` sub-transactions with yields: `insertEdges(chunk)` → `deleteSpecificResolvedReferences(keys)` (key = `fromNodeId+referenceName+referenceKind`) → `markReferencesFailed(keys)`.
- **Non-progress guard**: if `getUnresolvedReferencesCount() >= prevRemaining`, break (prevents the 5M-edge/1.4 GB runaway when a resolver returned a mutated `original.referenceName` so the keyed delete no-oped).
- Progress in `resolveAll` reported at 1% granularity. Final step: `synthesizeCallbackEdges` (best-effort, count into `stats.byMethod['callback-synthesis']`).

### Caches (all per-resolver-instance)
`DEFAULT_CACHE_LIMIT = 5000`, env `CODEGRAPH_RESOLVER_CACHE_SIZE` (positive int). Content-bearing caches (file text, split lines) get `max(64, floor(limit/5))`. LRU caches: nodeCache (file→nodes), fileCache (file→content|null), importMappingCache, reExportCache, nameCache, lowerNameCache, qualifiedNameCache, fileLinesCache, methodMatchCache (key `` `${language} ${type}::${method}` ``). Plain Map: nodesByKindCache (~24 kinds, #1180). Lazy singletons: `projectAliases`, `goModule`, `workspacePackages` (undefined=uncomputed, null=absent, immutable for resolver lifetime). `razorUsingsCache` (plain Map).

### `resolveOne` pipeline (order is a contract)
1. **Built-in/external filter** (`isBuiltInOrExternal`): JS/TS(+tsx/jsx/arkts) `JS_BUILT_INS`, `console./Math./JSON.` prefixes, `REACT_HOOKS`; ArkTS `$r`/`$rawfile`; Python `PYTHON_BUILT_INS`, dotted `PYTHON_BUILT_IN_TYPES` receiver or `PYTHON_BUILT_IN_METHODS` member unless capitalized receiver ∈ knownNames; bare builtin-method name only if not ∈ knownNames; Go `GO_STDLIB_PACKAGES` (receiver before first `.`) and `GO_BUILT_INS`; Pascal `PASCAL_UNIT_PREFIXES` (startsWith) + `PASCAL_BUILT_INS`; C/C++ `std::` prefix unconditional, `C_BUILT_INS`/`CPP_BUILT_INS` only when `!hasAnyPossibleMatch(name)` (user shadowing wins). Exact sets are in index.ts lines 71–196 — port verbatim.
2. **CFML component-path inheritance** (#1152): language `cfml|cfscript`, kind `extends|implements`, name contains `.` or `/`. Relative form (`../base`): resolve against ref dir, exact case-insensitive filePath match → conf **0.95** `file-path`. Dotted form: candidates = class/interface nodes named last segment; score = count of right-to-left case-insensitive matching parent-dir segments; require `score > 0`, unique max (tie → null) → conf **0.9** `qualified-name`. **No fallthrough on miss.**
3. **Fast pre-filter**: skip ref unless `hasAnyPossibleMatch(name)` (direct name; around `.`: receiver, member, capitalized receiver, last-dot tail; around `::`: receiver, member, last-`::` tail; around `:`(no `::`) and `$`: member/receiver/capitalized receiver; after last `/`: filename) OR `matchesAnyImport(ref)` (localName == name or name startsWith `localName+'.'`) OR any framework `claimsReference(name)`. ArkTS names starting `.` are existence-checked with the dot stripped. Nix path imports bypass the check.
4. **`function_ref`** (#756) dedicated path: `this.`-prefixed → `resolveThisMemberFnRef` only; else `resolveViaImport` (accepted only if target kind function/method) → `matchFunctionRef`. Both language-gated. Never reaches frameworks/fuzzy.
5. **`resolveJvmImport`** (java/kotlin `imports` FQN → `pkg::sym` qualifiedName lookup, conf **0.95**; multi-candidate → `pickClosestJvmCandidate`: max shared leading dir segments, tie prefers node with `decorators` containing `'expect'`).
6. **Razor `@using`** resolution: collect file's `@using` (regex `/^\s*@using\s+(?:static\s+)?([A-Za-z_][\w.]*)/gm`) + every `_Imports.razor` walking up to root; resolve `ns::Name` per namespace; exactly ONE distinct hit → conf **0.9** `import`.
7. **Frameworks** loop (`gateFrameworkLanguage`): result with confidence ≥ **0.9** returns immediately; else added to candidates.
8. **`resolveViaImport`** (`gateLanguage`): ≥ 0.9 returns immediately; else candidate.
9. **Path-only refs**: `isPhpIncludePathRef` (php + imports + name has `/` or `.`), `isCobolCopybookRef` (cobol + imports), `isNixPathImportRef` (nix + imports + starts `./`|`../` + no `/[\s{}()[\];"'<>$]/`), or `language === 'terraform'` → return best candidate so far or null. **Never fall through to name-matching** (wrong edge worse than none, #660).
10. **`matchReference`** (`gateLanguage`); Nix post-filter: nix ref → target must be same file; non-nix ref → target must not be nix.
11. If no candidates: **defer** for the conformance pass when kind `calls` and (language ∈ `CHAIN_LANGUAGES = {java,kotlin,csharp,swift,rust,go,scala,dart,objc,pascal}` and name matches `CHAIN_SHAPE = /^(.+)\(\)\.(\w+)$/`) or (php and `PHP_PROP_SHAPE = /^this->\w+\.\w+$/`).
12. Return **highest-confidence** candidate.

Language gates: `gateLanguage` — for `references`/`function_ref` drop unless `sameLanguageFamily(target, ref)`; for `imports` drop if `crossesKnownFamily`. `gateFrameworkLanguage` — only for `references`/`imports`, drop if `crossesKnownFamily` (preserves `calls` and config↔code bridges). `LANGUAGE_FAMILY = { java/kotlin/scala→jvm, swift/objc→apple, typescript/tsx/javascript/jsx/arkts→web, c/cpp→c, csharp/razor→dotnet }`; everything else singleton.

### `matchReference` strategy order (name-matcher)
`function_ref` → `matchFunctionRef` only. ArkTS leading-dot attr → only `@Extend|@Styles|@AnimatableExtend|@Builder`-decorated arkts functions, preferCallSiteFile, must end at exactly 1 → **0.85** `exact-match`. Erlang `implements` or ref from `*.app`/`*.app.src` → erlang `namespace` nodes only, preferCallSiteFile → **0.9**. Then: (0) `matchByFilePath` (1) `matchByQualifiedName` (1b) cpp/c `matchCppCallChain` (1c) php/rust `matchScopedCallChain` (1d) java/kotlin/csharp/swift/go/scala/dart/objc/pascal `matchDottedCallChain` (2) `matchMethodCall` (3) `matchByExactName` (4) `matchFuzzy`.

### Confidence / scoring constants (contract — do not drift)
- `matchByFilePath` (requires `/` in name or extension regex `/\.[A-Za-z][A-Za-z0-9]{0,3}$/`): exact qualifiedName/filePath **0.95**; suffix match **0.85** via `pickClosestFileNode` (same-dir pool first; score = pathProximity + 5 if sameLanguageFamily); single file node **0.7**.
- `matchByQualifiedName`: single exact **0.95**; ambiguous-exact same-file **0.95**; suffix partial (split on `[:.]`, candidates named last part with `qualifiedName.endsWith(referenceName)`, preferCallSiteFile) **0.85**. For `calls` refs, drop `constant` nodes with language `yaml|properties` (#1180).
- `matchByExactName` (excludes `kind==='import'` nodes, #915): single **0.9** (cross-language **0.5**); >`AMBIGUOUS_NAME_CEILING` (**500**, env `CODEGRAPH_AMBIGUOUS_NAME_CEILING`) → decline (#999); else `findBestMatch` → proximity ≥ 30 ? **0.7** : **0.4**.
- `findBestMatch` weights: same file **+100**; dir proximity **15/shared leading segment, cap 80**; same language **+50** else **−80** (and same-language candidates existing skips cross-language entirely); `calls`→fn/method **+25**; `instantiates`→class/struct/interface **+25**; `decorates`→fn/method **+25**, class/interface **+15**; `isExported` **+10**; same-file line distance `max(0, 20 − distance/10)`.
- `matchMethodCall` shapes: dotted `/^([\w.]+)\.(\w+:?(?:\w+:)*)$/` (ObjC selectors), `::` `/^(\w+)::(\w+)$/`, lua/luau `receiver:method`, R `receiver$method`, php `this->prop.method` (exclusive typed path). Order: PHP property typed-inference (**0.9**) → local receiver inference (cpp dedicated / shared) `resolveMethodOnType` **0.9** `instance-method` (java/kotlin pass imported FQN) → Java/Kotlin field-signature inference **0.9** → Strategy 1 class-name in-file method (`preferCallSiteFile`, same language) **0.85** `qualified-name` → Strategy 2 capitalized receiver **0.8** `instance-method` → Strategy 3 method-name (ceiling 500; single same-language **0.7**; else camelCase word-overlap score, +1 same language, need `bestScore ≥ 2`, preferCallSiteFile for ties → **0.65**).
- `matchFuzzy`: lowercase lookup, kinds `{function,method,class}`, language-gated, same-language preferred; unique → **0.5** (cross-language **0.3**).
- `matchFunctionRef`: `bareFnOnly` languages `{typescript,tsx,javascript,jsx,arkts,cpp,python,php}` restrict bare names to `function` kind. `::`-qualified member pointer: same-family fn/method with qualifiedName == name or endsWith `::name`; same-file pool, cross-file only when unique → **0.9**. Swift implicit-self: bare method candidates must share the from-symbol's class prefix (suffix either way); same-file >1 all-method → refuse. Same-file: earliest startLine → **0.95** (unique) / **0.9** (overloads). Cross-file unique → **0.8**. Self-registration (`n.id === fromNodeId`) excluded.
- `resolveMethodOnType`: matches = method nodes, same language, qualifiedName `== type::method` or endsWith `::type::method` (memoized via `getMethodMatches`). Empty → conformance walk via `getSupertypes` (union of implements/extends targets of same-named supertype-bearing kinds `{class,struct,interface,trait,protocol,enum}`), recursion `depth < 4`. Multi-match: `preferredFqn` → file suffix `fqn.replace('.','/') + ('.kt'|'.java')` wins (#314); else `preferCallSiteFile` first (#1079).
- Chains: `matchCppCallChain` **0.85**; `matchScopedCallChain` (inner must contain `::`; `self` return marker → factory's own class) **0.85**; `matchDottedCallChain` **0.85** — Go bare `New().M`: return-type path 0.85, else bare-name fallback via `matchByExactName ?? matchFuzzy` on synthetic ref **but returned with the ORIGINAL ref as `.original`** (keyed-delete invariant); bare capitalized ctor only for `CONSTRUCTS_VIA_BARE_CALL = {kotlin,swift,scala,dart,pascal}`; ObjC `[X alloc]`-style fallback (`/^[A-Z]/` receiver, no captured return type) **0.8**; Pascal `/^[TI]/` constructor fallback **0.8**. All chains funnel through `resolveMethodOnType` — **absent method ⇒ no edge, never a wrong one**.
- `resolveThisMemberFnRef` **0.95** `function-ref` (class scope = own qualifiedName for kinds ∈ SUPERTYPE_BEARING ∪ module, else strip last `::` segment; candidates `${classPrefix}::${member}` fn/method same-file, earliest startLine); miss → deferred. `resolveDeferredThisMemberRefs`: node-anchored BFS over implements/extends edges, **depth < 5**, member lookup via `contains` edges, sameLanguageFamily → **0.85**.
- Import results: generic **0.9**; C/C++ same-dir sibling include **0.92**; Python module-member **0.85**; JVM FQN **0.95**; Lua require **0.9**; CFML relative **0.95**.

### Receiver-type inference
- **C++** (`inferCppReceiverType`): backward line scan from call line; declarator regex `` `([A-Za-z_][\w:]*(?:\s*<[^;=(){}]+>)?(?:\s*[*&]+)?)\s*\b${recv}\b\s*(?=[;=,)\[{(]|$)` ``; normalize strips cv-qualifiers/`<>`/`[*&]`, takes last `::` segment, rejects `CPP_NON_TYPE_TOKENS`; `auto` → initializer inference (`new T`, call/construction via `resolveCppCallResultType`: `make_unique/make_shared<T>` → T; single-level `recv.method` → receiver type then return type; recorded `returnType`; direct construction if class exists; recursion cap depth > 3). Fallback: sibling headers `.h/.hpp/.hxx`.
- **All other languages** (`inferLocalReceiverType`, #1108): per-language regex tables (name-matcher lines 1092–1237 — port verbatim; e.g. TS `= new T` and `: PascalCase`; Java/C#/Dart `Type recv[=;,)]`; Rust `let (mut) r … = &(mut) T` and `r : &(mut) T`; Go `:= &T{`, `var r *T`, `r *T` PascalCase param; Ruby `= T.new`; Lua `= T.new|T(` and annotation with anti-self-match lookahead `(?![\w.]|\s*[({"'\[])` (#1124); R `<- T$new`; Pascal `: T`, `:= T.Create`; CFML `new`/`createObject`/`cfargument`/`property` incl. WireBox `inject`). Scan **backward** from call line to `enclosingScopeStartLine` (tightest fn/method containing the line); lines > **10 000** chars skipped. CFML `variables./this.` and PHP `this->` receivers strip the prefix, scan whole file (forward sweep after backward miss); PHP uses property-only patterns (`(private|protected|public|readonly|static|final)(\(set\))? ?Type $prop`, `$this->prop = new T`) plus second-chance `$this->prop = $var` → `$var`'s typed declaration bounded by the enclosing `function` line. `normalizeInferredTypeName`: strip `<…>`/`[&*]`, last `[.:]` segment, reject `NON_TYPE_RECEIVER_TOKENS`.
- **Java/Kotlin fields** (`inferJavaFieldReceiverType`): enclosing class by line range (tightest by latest start), field node with matching name inside range; type from `signature` ("Type name"): strip generics, `[]`, varargs, take last `[.\s]` part, must start uppercase.

### Import resolution per ecosystem (`import-resolver.ts`)
- **Extension/index conventions** `EXTENSION_RESOLUTION` (string-appended, incl. `/index.*` entries): typescript `['.ts','.tsx','.d.ts','.js','.jsx','/index.ts','/index.tsx','/index.js']`; arkts `['.ets','.ts','.d.ts','.js','/Index.ets','/index.ets','/index.ts','/index.js']`; javascript `['.js','.jsx','.mjs','.cjs','/index.js','/index.jsx']`; tsx; jsx; svelte/vue/astro (`.ts,.js,<own>,.tsx,.jsx,/index.ts,/index.js,/index.<own>`); python `['.py','/__init__.py']`; go `['.go']`; rust `['.rs','/mod.rs']`; java `['.java']`; c `['.h','.c']`; cpp `['.h','.hpp','.hxx','.cpp','.cc','.cxx']`; csharp/php/ruby; objc `['.h','.m','.mm']`; nix `['.nix','/default.nix']`.
- **`resolveImportPath`**: COBOL copybook first (basename-stem index per context via WeakMap; score `.cpy` +4, `.cbl/.cob/.cobol` +2, same dir +1); `isExternalImport` → null (JS: node builtins `[fs,path,os,crypto,http,https,url,util,events,stream,child_process,buffer]`, workspace-map escape, tsconfig-alias-prefix escape, otherwise bare specifiers external unless prefix `@/`,`~/`,`src/`; Python stdlib first-segment `[os,sys,json,re,math,datetime,collections,typing,pathlib,logging]`; Go: local iff `== modulePath` or `startsWith(modulePath+'/')` or contains `/internal/`, else external; C/C++ stdlib header set incl. `.h`-stripped form); relative (`.`) — Python dotted-relative translated (N leading dots = N−1 `../`, remaining dots → `/`); aliased: tsconfig `applyAliases` → workspace rewrite → hard-coded fallback `{'@/':'src/','~/':'src/','@src/':'src/','src/':'src/','@app/':'app/','app/':'app/'}` → direct path; C/C++ last resort: `-I` dir scan (compile_commands.json at `[., build, cmake-build-debug, cmake-build-release, out]`, `-I<d>`/`-I d`/`-isystem d` via mini shlex; heuristic fallback: top-level `[include,src,lib,api,inc]` + any top-level dir containing `/\.(h|hpp|hxx|hh)$/i`).
- **`resolveViaImport` branch order**: C/C++ `imports` (same-dir sibling **0.92**, resolved path → file node **0.9**); COBOL (file **0.9**, no fallthrough); PHP include path (relative to includer, extension retry, **0.9**, no fallthrough); Nix path (**0.9**, no fallthrough); Go cross-package (`pkg.Member`: alias→in-module import→pkgDir = source minus modulePath; candidate must be exported Go node whose **immediate parent dir equals pkgDir** → **0.9**); Java/Kotlin (`Foo.bar`/bare `Foo` matching an import: FQN→path suffix `com/example/Foo.java|.kt`, member lookup by name filtered by file suffix; `import static` owner-path variant → **0.9**); Python module-member (`certs.where` via binding: namespace → source, named → `source(.)localName`; member = first segment; top-level `function|class|variable|constant`, **never method** → **0.85**) and absolute dotted module (`import a.b.c` → file `a/b/c.py` or `a/b/c/__init__.py` suffix-matched → **0.9**); Rust `A::B::C` (module prefix → file: anchor `crate` (walk up ≤64 dirs to `lib.rs|main.rs`), `self` (`mod.rs|lib.rs|main.rs` own dir else `foo/`), `super`×N; bare path tries self-relative **then** crate-relative; each segment `<seg>.rs` else `<seg>/mod.rs`; leaf must be fn/struct/enum/trait/type_alias/constant/method/class/interface in that file → **0.9**); Lua/Luau require (suffixes `[base.lua, base.luau, base/init.lua, base/init.luau]` where base = dots→slashes; suffix-match all files; sort by longest shared char-prefix with the requiring file → **0.9**); whole-module import → file node (namespace/default TS/JS + Python submodule → **0.9**); finally the generic loop: for each mapping with `localName == name` or `name.startsWith(localName+'.')`, resolve source path and `findExportedSymbol` (depth cap **`REEXPORT_MAX_DEPTH = 8`**, visited set; direct hit — default prefers `component` kind then exported function/class (#629); named re-export follows rename; wildcard re-exports last), then static-member descent (#825): containers `{class,struct,interface,enum,trait,protocol}`, member = first segment after receiver, lookup `${container.qualifiedName}::${member}` filtered to container's file, `calls` prefers callable → **0.9**.
- **Import-mapping extraction** (regex over raw content): JS ES6 `import\s+(?:(\w+)\s*,?\s*)?(?:\{([^}]+)\})?\s*(?:(\*)\s+as\s+(\w+))?\s*from\s*['"]([^'"]+)['"]` + `require()` destructuring; SFC (svelte/vue/astro) reuse the JS regex over the whole file; Python `from X import Y` + `^import X (as A)?`; Go single + block imports (namespace, localName = alias or last path segment); Java/Kotlin `^\s*import\s+(static\s+)?([\w.]+(?:\.\*)?)\s*;` after comment strip (wildcards skipped); PHP `use FQN (as Alias);`; C/C++ `^\s*#\s*include\s+[<"]([^>"]+)[>"]` (namespace, localName = basename sans header extension). Re-exports: `export\s*\*(?:\s+as\s+\w+)?\s*from\s*['"]…['"]` and `export\s*\{([^}]+)\}\s*from\s*['"]…['"]` after `stripJsComments` (string-aware scanner). Re-export parsing is keyed to the **barrel's own extension** (`/\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?|ets)$/i` → treated as typescript) not the consumer's language (#629).
- **path-aliases**: JSONC strip (string-aware) + trailing-comma removal; `tsconfig.json` then `jsconfig.json`; `baseUrl` default `'.'`; patterns sorted longer-prefix-first, literal-before-wildcard; `applyAliases` fills single `*`, resolves against absolute baseUrl, drops candidates escaping project root.
- **workspace-packages**: `package.json` `workspaces` (array or `{packages}`) + `pnpm-workspace.yaml` (minimal line parser); one-level `*` glob expansion (skip dotdirs/node_modules); first declaration wins. ohpm: bounded BFS (depth ≤ 6, ≤ 8000 dirs, skip set `{node_modules,oh_modules,.git,.codegraph,.hvigor,.preview,build,dist,out,…}`) collecting `file:` deps; a name mapped to different dirs is dropped as ambiguous; `entryByName` from ohpm `main`. `resolveWorkspaceImport`: longest matching package name; bare name → entry file if declared, else dir + subpath.

### Edge creation (`createEdges`)
`kind = referenceKind`, except: `function_ref` → **`references`**; `extends` → **`implements`** when target is interface/protocol and source isn't; `calls` → **`instantiates`** when target is class/struct. `metadata`: `confidence`, `resolvedBy`, `refName` (original referenceName — resurrection contract #1240), `refKind` (only when promotion changed the kind), `fnRef: true` for function_ref.

## Wire/contract surfaces

- `resolvedBy` strings (persist in edge metadata): `exact-match | import | qualified-name | framework | fuzzy | instance-method | file-path | function-ref`. Stats key `callback-synthesis` added by the batched pass.
- Edge metadata keys `confidence/resolvedBy/refName/refKind/fnRef` — read by explore/node output, edge resurrection (#1240) and validation tooling.
- `unresolved_refs` lifecycle: pending → deleted (resolved, keyed by `fromNodeId+referenceName+referenceKind`) or `status='failed'` (retryable via name-triggered sync retry). Invariant: after a completed pass no processed row is pending (orphan-sweep keys off pending count).
- `ResolvedRef.original.referenceName` **must equal the stored row's name** or the keyed delete no-ops and the batch loop detects no progress (Go-fallback runaway contract).
- Confidence **0.9** is the "return immediately" threshold in `resolveOne`; final pick is max-confidence with first-wins tie-break (`reduce` keeps earlier on equal).
- Env vars: `CODEGRAPH_RESOLVER_CACHE_SIZE`, `CODEGRAPH_AMBIGUOUS_NAME_CEILING`, `CODEGRAPH_SYNTH_TIMINGS` (timing logs only).
- All exact string sets/regexes called out above (built-ins, families, extension tables, alias fallback map, chain shapes, receiver-pattern tables) are load-bearing precision/recall contracts.

## Test coverage

- `__tests__/resolution.test.ts` (4597 lines) — the contract suite; port its assertions: name-matcher basics (exact, cross-module confidence lowering, same-module preference, qualified names); Erlang behaviour→namespace-only; ubiquitous-name ceiling #999 (decline above, same-file still resolves, just-below unchanged); relative/parent imports; JS/Python mapping extraction; JVM FQN import resolution (collision disambiguation, wildcard/unqualified/non-JVM/non-import nulls); calls→instantiates promotion (Python, C++ #1035); static member #825; Go cross-package + aliased imports #388, stdlib stays external; Python module-attribute #578; Java import disambiguation #314; same-file preference #1079 (`preferCallSiteFile`, qualified-name, C++ end-to-end); watchdog memo #1122 (getMethodMatches, per-ref yields, getFileLines, >10K-char line skip); local receiver inference #1108/#1125/#1124 matrix across ~14 languages; kind bias for `instantiates`/`decorates`; tsconfig aliases (aliased import beats same-named file; graceful absence); re-export chains (3-hop, rename, svelte default #629, astro #768, bare-dir import, workspace subpath barrel, Vue SFC); C/C++ include resolution (same dir, .hpp, subdir, -I dirs, multi-extension, system headers null, compile_commands parsing, heuristic dirs, end-to-end); C++ templated bases #1043; PHP includes #660 (shape predicate, file→file edge, no mis-connect); chained-call resolution **per language** (C++ #645, PHP #608, Java, Kotlin, C#, Swift, Rust, Go incl. variable-inner fallback without graph explosion, Scala, Dart, ObjC incl. instancetype singleton, Pascal incl. paren-less/typecast) — every language block includes the **"creates NO edge when the type lacks the method"** safety test; conformance pass #750 (superclass/interface-default/trait/embedded-struct + safety); Nix (path imports, default.nix, no cross-language, lexical-only, no dynamic imports).
- `__tests__/same-name-disambiguation.test.ts` (#764) — no cross-app edges; per-definition callers/impact.
- `__tests__/php-property-receiver-resolution.test.ts` — `$this->prop->method()` typed-property/promoted-param/assignment inference.
- `__tests__/cfml-inheritance-resolution.test.ts` — component-path extends.
- `__tests__/arkts-resolution.test.ts` — leading-dot attributes, `$r`, ohpm workspace.
- `__tests__/multi-repo-workspace.test.ts`, `__tests__/gin-middleware-chain.test.ts` — end-to-end workspace + Go-chain regressions.
- `__tests__/pr19-improvements.test.ts` "Resolution Warm Caches" + "Best-Candidate Resolution"; `__tests__/cooperative-yield.test.ts` — yield budget semantics.

## Rust port notes

- **Placement**: everything above → `selene-resolve`. `ResolutionContext` should become a trait whose data methods are backed by `selene-db`'s `GraphStore` (this is exactly the PRD §5.4 code/DB-split question — `getNodesByName`/`getNodesByQualifiedName`/`getOutgoingEdges`/`iterateNodesByKind` are the store primitives resolution needs). `LRUCache` → `lru` crate; the cobol copybook `WeakMap<Context, index>` must become a resolver field. `cooperative-yield` is a Node event-loop artifact — in Rust either drop it (run resolution off the async runtime) or map to `tokio::task::yield_now` with the same 250 ms budget if a watchdog-equivalent exists.
- **Ordering is behavior**: strategy order, candidate-vs-immediate-return at ≥0.9, first-wins ties, `preferredFqn` before `preferCallSiteFile`, backward-then-forward scans — all observable in edge output. Port as a fixed pipeline, not a rules engine.
- **Regex migration**: patterns are built per-ref from escaped receiver names (`new RegExp`); in Rust pre-compile templates or use `regex::escape` + a small per-ref cache — naive per-ref compilation will dominate. Note JS specifics: `CHAIN_SHAPE`'s greedy `(.+)` binds to the **last** `().`; `String.replace('*', x)` replaces only the first `*`; `/g` + `exec` loops are stateful. Lua's negative lookahead `(?![\w\.]|\s*[({"'\[])` needs `fancy-regex` or a hand-rolled check (the `regex` crate has no lookahead).
- **Unicode/case**: `charAt(0).toUpperCase()` capitalization and `toLowerCase()` file/dir comparisons — pick a deliberate policy (ASCII-only matches the TS behavior closely enough for identifiers).
- **JSONC/JSON5**: `stripJsonc` + trailing-comma removal (tsconfig) and jsonc-parser (ohpm) → `json5`/`jsonc-parser` crates; keep tolerance identical or alias loading silently vanishes.
- **In-memory deferral**: `deferredChainRefs`/`deferredThisMemberRefs` exist because the batched pass deletes/fails rows before edges exist — the conformance passes can only run in the same resolver instance. Preserve this lifetime coupling or re-read `status='failed'` chain-shaped rows instead.
- **Mutable global caches**: `cppIncludeDirCache` (module-level, keyed by root, cleared via `clearCppIncludeDirCache`) should become resolver state. **Dead code**: `importMappingCache` in `import-resolver.ts` (line 998) is declared and cleared but never written/read — the real cache is the resolver's LRU; don't port it. `matchByExactName`'s cross-language single-candidate branch is mostly unreachable for `references` (gate already filtered) but live for `calls` — keep.
- **Scope note**: CLAUDE.md attributes "cargo workspace member globs" to this subsystem, but that lives in `frameworks/cargo-workspace.ts` (excluded here); core import-resolver's Rust support is the `crate::/self::/super::` module-path walk + `.rs`/`mod.rs` conventions only. Nested `go.mod`s (Go workspaces) and tsconfig `extends` chains are documented non-features — preserve the limitation notes.
- **Guiding invariant everywhere**: validated inference — a type guess is only accepted if the method actually exists on it (`resolveMethodOnType`), so mis-inference yields **no edge, never a wrong one**; path-shaped refs never fall back to symbol matching; ubiquitous names decline rather than guess. Regressing any of these produces wrong edges that are worse than missing ones.
