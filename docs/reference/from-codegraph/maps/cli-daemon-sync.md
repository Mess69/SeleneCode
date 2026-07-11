# CLI + Daemon + Sync Map

## File inventory

| Path | LOC | Responsibility |
|---|---|---|
| `src/bin/codegraph.ts` | 2420 | CLI entry (commander): all 22 subcommands, node-version gate, `--liftoff-only` re-exec, telemetry preAction hook |
| `src/bin/command-supervision.ts` | 96 | PPID + liveness watchdogs wrapped around long CLI commands (`index`/`init`) (#999) |
| `src/bin/fatal-handler.ts` | 93 | Last-resort uncaughtException/unhandledRejection → bounded stderr line, exit 1; never reads `.stack` (#850) |
| `src/bin/node-version-check.ts` | 76 | Banner builders for Node ≥25 block and Node <20 block; `MIN_NODE_MAJOR = 20` |
| `src/bin/uninstall.ts` | 34 | npm `preuninstall` script: global-location agent-config cleanup, never throws |
| `src/mcp/daemon.ts` | 867 | Shared detached daemon: socket listen, hello handshake, lockfile arbitration, refcount + idle timeout, client liveness sweep |
| `src/mcp/daemon-paths.ts` | 140 | Socket/pipe candidate paths, `daemon.pid` encode/decode |
| `src/mcp/daemon-registry.ts` | 199 | `~/.codegraph/daemons/` discovery records; `listDaemons`/`stopDaemonAt`/`stopAllDaemons` |
| `src/mcp/daemon-manager.ts` | 117 | Interactive picker logic behind `codegraph daemon` (pure, clack injected) |
| `src/mcp/proxy.ts` | 596 | stdio↔socket proxy; local-handshake proxy (static initialize/tools-list, daemon-forwarded calls, in-process fallback) |
| `src/mcp/ppid-watchdog.ts` | 95 | `supervisionLostReason` (#277/#692 POSIX vs Windows), env parsers |
| `src/mcp/early-ppid.ts` | 25 | `EARLY_PPID = process.ppid` captured at first import (#1185) |
| `src/mcp/startup-handshake.ts` | 71 | "no MCP traffic since startup" backstop timer (#1185) |
| `src/mcp/stdin-teardown.ts` | 46 | stdin `end`/`close`/`error` → destroy + shutdown once (#799) |
| `src/mcp/liveness-watchdog.ts` | 242 | Separate-process main-thread wedge killer (`node -e` child, heartbeat over stdin, SIGKILL) (#850, #1231) |
| `src/mcp/index.ts` | 490 | `MCPServer.start()` mode decision (direct/proxy/daemon), detached-daemon spawn, lock takeover loop |
| `src/sync/watcher.ts` | 912 | `FileWatcher`: recursive fs-watch (macOS/Win) / per-dir inotify (Linux), debounce, pending-files, degrade policy |
| `src/sync/watch-policy.ts` | 104 | Watch on/off decision (WSL2 `/mnt` auto-disable, env overrides) |
| `src/sync/git-hooks.ts` | 212 | Marker-delimited `codegraph sync` git hooks: install/remove/detect |
| `src/sync/worktree.ts` | 158 | Borrowed-worktree-index detection + warning strings |
| `src/sync/index.ts` | 33 | Re-exports of the sync module surface |

## Public interface

```ts
// sync/index.ts
class FileWatcher {
  constructor(projectRoot: string, syncFn: () => Promise<{filesChanged: number; durationMs: number}>, options?: WatchOptions);
  start(): boolean; stop(): void; isActive(): boolean;
  isDegraded(): boolean; getDegradedReason(): string | null;
  getPendingFiles(): PendingFile[];        // {path, firstSeenMs, lastSeenMs, indexing}
  waitUntilReady(timeoutMs = 10000): Promise<void>;
}
interface WatchOptions { debounceMs?: number /*2000*/; onSyncComplete?; onSyncError?; onDegraded?(reason: string); inertForTests?: boolean }
class LockUnavailableError extends Error;   // syncFn throws it → no-progress retry path
function watchDisabledReason(projectRoot: string, probe?: {env?, isWsl?}): string | null;
function detectWsl(): boolean;
type GitHookName = 'post-commit' | 'post-merge' | 'post-checkout';
const DEFAULT_SYNC_HOOKS: GitHookName[];
function installGitSyncHook(projectRoot: string, hooks?: GitHookName[]): GitHookResult; // {installed, hooksDir, skipped?}
function removeGitSyncHook(projectRoot, hooks?): GitHookResult;
function isSyncHookInstalled(projectRoot, hooks?): boolean;
function isGitRepo(projectRoot: string): boolean;
function gitWorktreeRoot(dir: string): string | null;
function detectWorktreeIndexMismatch(startPath: string, indexRoot: string): {worktreeRoot, indexRoot} | null;
function worktreeMismatchWarning(m): string; function worktreeMismatchNotice(m): string;

// mcp/daemon.ts
class Daemon {
  constructor(projectRoot: string, opts?: {idleTimeoutMs?, maxIdleMs?});
  start(): Promise<{socketPath: string; lock: DaemonLockInfo}>;
  stop(reason?: string): Promise<void>;  getClientCount(): number;  getSocketPath(): string;
  backstopShouldExit(isAlive): boolean;  reapDeadClients(isAlive): number;
}
interface DaemonHello { codegraph: string; pid: number; socketPath: string; protocol: 1 }
interface DaemonClientHello { codegraph_client: 1; pid: number; hostPid: number | null }
function tryAcquireDaemonLock(projectRoot): {kind:'acquired', pidPath, info} | {kind:'taken', existing: DaemonLockInfo|null, pidPath};
function clearStaleDaemonLock(pidPath: string, expectedDeadPid?: number): boolean;
function isProcessAlive(pid: number): boolean;   // kill(pid,0); EPERM ⇒ alive
function bindFirstUsableSocket(candidates, listen, opts?): Promise<{server, socketPath}>;
function finalizeDaemonExit(platform, exit): Timeout | null;
function parseClientHelloLine(line): {pid, hostPid} | null;
function peerIsDead(peers, isAlive): boolean;

// mcp/daemon-paths.ts
function getDaemonSocketCandidates(projectRoot): string[];  function getDaemonSocketPath(root): string;
function getDaemonPidPath(root): string;
interface DaemonLockInfo { pid: number; version: string; socketPath: string; startedAt: number }
function encodeLockInfo(i): string;  function decodeLockInfo(raw): DaemonLockInfo | null;

// mcp/daemon-registry.ts
interface DaemonRecord { root; pid; version; socketPath; startedAt }
function registerDaemon(rec); function deregisterDaemon(root);
function listDaemons(opts?: {prune?: boolean}): DaemonRecord[];  // newest first
function stopDaemonAt(root): Promise<StopResult>;  // outcome: 'term'|'kill'|'not-running'|'no-daemon'
function stopAllDaemons(): Promise<StopResult[]>;

// mcp/proxy.ts
function runProxy(socketPath, expectedVersion?): Promise<{outcome:'proxied'|'fallback-needed', reason?}>;
function connectWithHello(socketPath, expectedVersion?): Promise<net.Socket | 'version-mismatch' | null>;
function runLocalHandshakeProxy(deps: {getDaemonSocket(), makeEngine(), root}): Promise<void>;

// mcp/ppid-watchdog.ts / early-ppid.ts / startup-handshake.ts / liveness-watchdog.ts
function supervisionLostReason(state: {originalPpid, currentPpid, hostPpid, isAlive, platform?}): string | null;
const DEFAULT_PPID_POLL_MS = 5000;  const EARLY_PPID: number;
function parsePpidPollMs(raw): number;  function parseHostPpid(raw): number | null;
function armStartupHandshakeTimeout(onAbandoned, stream?, timeoutMs?): () => void;
function installMainThreadWatchdog(options?: {progressPaths?: string[]}): {stop()} | null;
function installCommandSupervision(label: string, watchdog?: WatchdogOptions): {stop()};
function installFatalHandlers(deps?): void;  function describeFatal(value: unknown): string;
```

## Key algorithms & data flow

### CLI bootstrap (bin/codegraph.ts)
1. `import '../mcp/early-ppid'` FIRST — captures `process.ppid` before anything else (#1185).
2. Node gate: major ≥ 25 → print `buildNode25BlockBanner`, exit 1 unless `CODEGRAPH_ALLOW_UNSAFE_NODE`; major < `MIN_NODE_MAJOR` (20) → `buildNodeTooOldBanner`, same override.
3. `relaunchWithWasmRuntimeFlagsIfNeeded(__filename)`: if `--liftoff-only` not in execArgv and `CODEGRAPH_WASM_RELAUNCHED` unset, re-exec self once with env `CODEGRAPH_WASM_RELAUNCHED=1` and `CODEGRAPH_HOST_PPID=<process.ppid>` (threads the real MCP-host pid past the shim). `WASM_RUNTIME_FLAGS = ['--liftoff-only']`.
4. `installFatalHandlers()` — uncaught/unhandled → bounded line to fd 2 (`[CodeGraph] Uncaught exception: <Name: msg>`), `process.exit(1)`; NEVER touches `error.stack`.
5. `process.argv.length === 2` → run interactive installer; else commander `main()`.
6. Pre-parse intercept: argv[2] `-v` or `-version` → print version, return (commander only handles `-V`/`--version`; separate `version` subcommand exists too).
7. Telemetry `preAction` hook: skip if `CODEGRAPH_DAEMON_INTERNAL`; skip name `telemetry`; record `cli_command:<name>`; flush for `{init, uninit, index, sync, upgrade}`.
8. `resolveProjectPath(arg?)`: `path.resolve(arg || cwd)`; if `isInitialized` (dir has `.codegraph/` **and** `.codegraph/codegraph.db`) use it, else walk parents to filesystem root; not found → return original (commands then fail with "not initialized").

### Subcommands (flags → behavior → exit codes)
- **`init [path]`** `-i/--index`(deprecated no-op), `-f/--force`, `-v/--verbose`. Refuses `unsafeIndexRootReason` roots (filesystem root / home / parent-of-home) without `--force` (exit code 1 via `process.exitCode`). Already-initialized → warn + `offerWatchFallback`, exit 0. Else `CodeGraph.init(path,{index:false})`, supervised full index (shimmer progress or verbose 5%-step lines), `printIndexResult`, index telemetry, `nodesCreated===0` → `offerIndexIgnoredRepos` (interactive gitignored-child-repo opt-in writing `includeIgnored` to codegraph.json, #1156). Errors → exit 1.
- **`uninit [path]`** `-f/--force`. Confirm `(y/N)` unless forced; `cg.uninitialize()` (deletes `.codegraph/`), `removeGitSyncHook`, telemetry `uninstall` lifecycle + flush. Not initialized → warn, exit 0.
- **`index [path]`** `-f`, `-q/--quiet`, `-v/--verbose`. Full rebuild via `CodeGraph.recreate` (delete DB+WAL, never row-DELETE — #874/#1067) wrapped in `installCommandSupervision('index', {progressPaths:[db, db+'-wal']})`. `!result.success` → exit 1.
- **`sync [path]`** `-q/--quiet` (for git hooks). `cg.sync()`; prints `Synced N changed files` + `Added/Modified/Removed … N nodes in T`. Not initialized → exit 1 (silent under `-q`).
- **`status [path]`** `-j/--json`. Detects worktree mismatch from the *start* path vs resolved root. Text output: `CodeGraph Status`, project, index-state warnings (`indexing`/`partial`/`failed`), pendingRefs warning (#1187), Files/Nodes/Edges/DB Size (MB, 2dp), `Backend: node:sqlite — built-in (full WAL)`, Journal (`wal` green else warning), Nodes by Kind desc, Files by Language desc, Pending Changes or `Index is up to date`, re-index hint if built by older version. JSON shape: see Wire section.
- **`query <search>`** `-p`, `-l/--limit` (default '10'), `-k/--kind`, `-j`. `searchNodes`; results re-sorted so generated files sink (`isGeneratedFile` 0/1 subtract). Text: `kind.padEnd(12) + name`, dim `path:startLine`, dim signature. No raw score printed (#1045); JSON keeps `score`.
- **`explore <query...>`** `-p`, `--max-files`. Runs MCP `codegraph_explore` via `ToolHandler` and prints `result.content[0].text`; `result.isError` → exit 1. Un-indexed → agent-facing refusal text (see Wire), exit 1.
- **`node [name]`** `-p`, `-f/--file`, `--offset`, `--limit`, `--symbols-only`. Neither name nor file → usage error, exit 1. Name containing `/` or `\` is a file (`\`→`/`); else symbol with `includeCode:true`. Runs MCP `codegraph_node`.
- **`files`** `-p`, `--filter <dir>` (prefix or `./`+prefix), `--pattern <glob>` (glob→regex: `**`→`.*`, `*`→`[^/]*`, `?`→`[^/]`; NOT anchored), `--format tree|flat|grouped` (default `tree`), `--max-depth`, `--no-metadata`, `-j`. Tree sorts dirs first, then localeCompare.
- **`callers <symbol>` / `callees <symbol>`** `-p`, `-l` (default '20'), `-j`. `searchNodes(symbol,{limit:50})`; keep matches where `name === symbol || name.endsWith('.'+symbol) || name.endsWith('::'+symbol)` (filter only applies when >1 match); union `getCallers`/`getCallees` deduped by node id; if empty, fall back to top match; slice to limit. JSON `{symbol, callers|callees:[{name,kind,filePath,startLine}]}`.
- **`impact <symbol>`** `-d/--depth` default '2' clamped [1,10], `-j`. Same exact-match merge over `getImpactRadius`; JSON `{symbol, depth, nodeCount, edgeCount, affected:[…]}`; text grouped by file.
- **`affected [files...]`** `--stdin`, `-d/--depth` default '5', `-f/--filter <glob>`, `-j`, `-q`. Inputs normalized to project-relative POSIX (#825: absolute → relative, `path.normalize`, `\`→`/`, strip `./`). Default test patterns: `/\.spec\./, /\.test\./, /\/__tests__\//, /\/tests?\//, /\/e2e\//, /\/spec\//`. Custom glob→regex: escape `[+[\]{}()^$|\\]`, `.`→`\.`, `**`→`.+`, `*`→`[^/]*`. BFS over `getFileDependents` (test files are terminal, non-test enqueued until depth ≥ max). No inputs → exit 0. `-q` prints bare sorted paths.
- **`daemon`** (alias `daemons`). No daemons → info, exit 0. Non-TTY → plain `pid N  vX  up D  root` lines. TTY → clack picker: current project's daemon (realpath'd `findNearestCodeGraphRoot(cwd)`) floats first & pre-selected; loop pick→stop→re-list; `Stop all` shown only when >1; sentinels `__stop_all__`/`__cancel__`; uptime `45s`/`12m`/`3h 5m`.
- **`serve`** (hidden) `-p/--path`, `--mcp`, `--no-watch` (sets `CODEGRAPH_NO_WATCH=1`). `--mcp` + TTY stdin + not `CODEGRAPH_DAEMON_INTERNAL` → explanation, exit 0. Else `new MCPServer(path).start()`. Without `--mcp`: prints config snippet + tool list to **stderr**.
- **`unlock [path]`** removes `.codegraph/codegraph.lock`; missing → info, exit 0.
- **`prompt-hook`** (hidden) — see below.
- **`install`** `-t/--target`, `-l/--location global|local`, `-y/--yes`, `--no-permissions`, `--print-config <id>`, `--refresh`. Invalid location → exit 1. `--no-permissions` maps: explicit false → `autoAllow=false`; `--yes` → `true`; else `undefined` (prompt).
- **`uninstall`** `-t`, `-l`, `-y`, `--keep-cli`.
- **`telemetry [status|on|off]`** unknown action → exit 1; warns when `DO_NOT_TRACK`/`CODEGRAPH_TELEMETRY` env overrides the saved choice.
- **`upgrade [version]`** `--check`, `-f/--force`; version pin also from `CODEGRAPH_VERSION` env; exits with code returned by `runUpgrade`.
- **`version`** prints package version.

### prompt-hook (Claude `UserPromptSubmit` hook)
Contract: **never** breaks the prompt — every path exits 0; only effect is optional stdout. Kill-switch: `CODEGRAPH_NO_PROMPT_HOOK=1` or `CODEGRAPH_PROMPT_HOOK=0`; TTY stdin → return. Reads all stdin, `JSON.parse` → `{prompt, cwd}`. Tiered gate with telemetry counters `cli_command:prompt-hook-gate-<outcome>`:
1. `hasStructuralKeyword(prompt)` (multi-language keyword list) OR `extractCodeTokens` OR `extractProseCandidates`; none → `noop-shape`.
2. `planFrontload(cwd, prompt)` picks nearest indexed ancestor or monorepo sub-project (#964); nothing → `noop-no-index`.
3. HIGH (keyword, or code-token verified via `cg.getNodesByName(t).length>0`): run `codegraph_explore` with the raw prompt; truncate body at **MAX = 16000** chars appending `\n…(truncated; call codegraph_explore for the rest)`; wrap in `<codegraph_context note="Structural context from CodeGraph for this prompt — treat returned source as already read; …">…</codegraph_context>`; outcomes `high-keyword`/`high-token`, empty/error → `noop-explore-keyword`/`noop-explore-token`.
4. MEDIUM (prose→symbol-segment matches): heal empty vocab (`healSegmentVocabIfEmpty`, fail → `noop-vocab-empty`); no matches → `noop-unverified`; else emit symbol list + suggested query (top-3 names) → `medium-segment`.
5. Multiple sub-projects with no clear match → nudge block listing `projectPath:` lines → `nudge-projects`.

### Daemon lifecycle & routing
`MCPServer.start()` order: (1) `CODEGRAPH_DAEMON_INTERNAL` truthy → *be* the daemon; (2) `CODEGRAPH_NO_DAEMON` truthy (not `0`/`false`) → direct stdio mode; (3) no `.codegraph/` root reachable (realpath'd `findNearestCodeGraphRoot`) → direct; (4) else local-handshake proxy. Direct mode = single `MCPSession` over stdio + stdin teardown + startup-handshake backstop + PPID watchdog + liveness watchdog. CLI commands other than `serve --mcp` never route through the daemon — they open the DB directly.

**Daemon-elect**: loop ≤ `TAKEOVER_MAX_RETRIES=5` (delay 100ms): `tryAcquireDaemonLock` → acquired ⇒ `new Daemon(root).start()` + liveness watchdog (no PPID watchdog — detached on purpose); taken+holder-alive ⇒ stderr note, exit 0; taken+dead ⇒ `clearStaleDaemonLock(pidPath, expectedDeadPid)` (compare-and-delete: bail if pid differs or is alive), retry. Exhausted ⇒ exit 0.

**Lock acquisition**: write full JSON record to `<pidPath>.<pid>.tmp` (mode 0600) then `linkSync(tmp, pidPath)` — atomic + exclusive, no empty-file window; `EEXIST` ⇒ taken; other errno (no-hardlink FS: ENOTSUP/EPERM/EISDIR, #997) ⇒ fallback `openSync(pidPath,'wx',0o600)` + write via fd.

**Socket bind**: candidates from `getDaemonSocketCandidates`: win32 → `\\.\pipe\codegraph-<sha256(realpath)[:16]>` only; POSIX → `[.codegraph/daemon.sock, tmpdir/codegraph-<hash>.sock]`, tmp-only if in-project path length > `POSIX_SOCKET_PATH_LIMIT=100`. Before each POSIX bind, unlink stale socket; after bind chmod 0600. `bindFirstUsableSocket` relocates past **any** errno except `EADDRINUSE` (which propagates; caller releases lock, all launchers fall back to direct). If the bound path ≠ candidate 0, rewrite pidfile atomically (temp `${pidPath}.${pid}.relocate` + rename). Then `registerDaemon` and log `[CodeGraph daemon] Listening on <path> (pid N, vX). Idle timeout Nms.`

**Per-connection**: daemon writes hello line `{"codegraph":ver,"pid":N,"socketPath":p,"protocol":1}\n` immediately; then reads optional client-hello for ≤ `CLIENT_HELLO_TIMEOUT_MS=3000` (line cap `MAX_HELLO_LINE_BYTES=4096`; non-hello or timeout ⇒ pids null, bytes unshifted back after `socket.pause()` — the pause-before-detach is load-bearing, #662); creates `MCPSession`; transport close ⇒ refcount decrement.

**Reaping**: idle timer arms at 0 clients (`CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`, default **300_000** ms; `0` = never); inactivity backstop (`CODEGRAPH_DAEMON_MAX_IDLE_MS`, default **1_800_000**, tick `min(maxIdle, 60_000)`) reaps only when no inbound bytes for the window AND no client is provably alive (#692); client sweep every `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS` (default **30_000**) drops sessions whose proxy pid or host pid is dead (`peerIsDead`; unknown pid never dead). Shutdown: SIGTERM/SIGINT → close sessions, server, engine, remove own pidfile (only if pid matches), deregister, unlink socket, then `finalizeDaemonExit` (POSIX `exit(0)`; win32 sets exitCode 0, drains loop, unref'd force-exit backstop after **2_000** ms).

**Launcher/proxy**: probe each socket candidate with `connectWithHello` (hello read timeout 3s, exact version equality; mismatch is definitive → serve in-process); none → `spawnDetachedDaemon(root)`: spawn `process.execPath [execArgv..., argv[1], 'serve','--mcp','--path',root]` with `detached:true`, `windowsHide`, stdio → append fd on `.codegraph/daemon.log` (fallback `'ignore'`), env +`CODEGRAPH_DAEMON_INTERNAL=1`, −`CODEGRAPH_HOST_PPID`, `unref()`; then poll ≤ `DAEMON_CONNECT_MAX_RETRIES=240` × `DAEMON_CONNECT_RETRY_DELAY_MS=25` (~6s). The local-handshake proxy answers `initialize` (static `PROTOCOL_VERSION`/`SERVER_INFO`/instructions), `tools/list` (`getStaticTools()`), `resources/list`→`{resources:[]}`, `resources/templates/list`→`{resourceTemplates:[]}`, `prompts/list`→`{prompts:[]}` locally; forwards everything else line-by-line; forwards the initialize to prime the daemon but suppresses its reply; buffers lines while connecting; on daemon death mid-session flips to in-process engine and re-serves in-flight requests (tracked by JSON-RPC id); unserveable → `{"error":{"code":-32603,"message":"CodeGraph daemon unavailable"}}`. After hello-verify, proxy sends `{"codegraph_client":1,"pid":<own>,"hostPid":<CODEGRAPH_HOST_PPID ?? EARLY_PPID>}\n`.

**PPID watchdog (#277/#692/#1185)**: poll every `CODEGRAPH_PPID_POLL_MS` (default 5000; `0` disables). Shutdown reasons in precedence order: (a) `currentPpid !== originalPpid` (POSIX reparent) → `"ppid A -> B"`; (b) win32 only: `originalPpid > 1 && !isAlive(originalPpid)` → `"parent pid N exited"`; (c) `hostPpid !== null && !isAlive(hostPpid)` → `"host pid N exited"`. Baseline is always `EARLY_PPID`. `isAlive` = `kill(pid,0)`, EPERM ⇒ alive. Liveness fallback is deliberately win32-only (POSIX double-fork false positives).

**Liveness watchdog (#850/#1231)**: disabled by `CODEGRAPH_NO_WATCHDOG` (`1/true/yes/on`); timeout `CODEGRAPH_WATCHDOG_TIMEOUT_MS` default **60_000**; heartbeat interval `min(2000, max(50, round(timeout/5)))`; separate child process (`node -e <inline source>`, cwd=tmpdir, argv: parentPid, timeoutMs, capMs, progressPaths…) — parent writes `\n` per tick to child stdin; silence ≥ timeout ⇒ child SIGKILLs parent, unless `progressPaths` (DB + `-wal`) size/mtime fingerprint advanced and continuous silence < `PROGRESS_CAP_MULTIPLIER=10` × timeout (10 min cap). stdin EOF/error ⇒ child exits (no orphan).

**Startup-handshake backstop (#1185)**: one-shot timer, default `DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS = 900_000` (env `CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS`, ≤0 disables); disarmed by first stdin byte; must be armed AFTER the real stdin consumer; never armed in the detached daemon.

### FileWatcher pipeline
Strategy: darwin/win32 → single `fs.watch(root,{recursive:true})` (O(1) fds); linux → one watch per non-ignored directory (walk at start; new dirs added on events with `markExisting=true` so files written before the watch install become pending), cap `CODEGRAPH_MAX_DIR_WATCHES` default **50_000** (warn once). Errors: EMFILE/ENFILE (or message match when no code) ⇒ `degrade(EXHAUSTION_REASON)` — permanent until next `start()`, fires `onDegraded` once, stops watcher; ENOSPC (Linux inotify budget) ⇒ warn once + stop adding watches, NON-fatal. `start()` refuses when `watchDisabledReason` non-null: precedence `CODEGRAPH_NO_WATCH=1` → off; `CODEGRAPH_FORCE_WATCH=1` → on; WSL (`WSL_DISTRO_NAME`/`WSL_INTEROP` env or `/proc/version` contains `microsoft`/`wsl`, cached) + root matching `/^\/mnt\/[a-z](\/|$)/i` → off (#199).

Event path: rel path normalized to POSIX; drop if empty/`.`/`..`-prefixed; drop CodeGraph data dirs (`.codegraph`, `CODEGRAPH_DIR` override, `.codegraph-*` siblings) and `.git`; drop via `buildScopeIgnore` (indexer's ignore = defaults + .gitignore, #276/#407/#514); drop non-source extensions. Survivors → `pendingFiles[rel] = {firstSeenMs, lastSeenMs}` and debounced sync (`debounceMs` default **2000**). `flush()`: skip if syncing; record `syncStartedMs`; run `syncFn`; success ⇒ clear counters, delete pending entries with `lastSeenMs <= syncStartedMs` (mid-sync edits stay pending — prefer false-stale over false-fresh); `LockUnavailableError` ⇒ `lockRetryCount++` (quiet), degrade after > `MAX_LOCK_RETRIES=5`; other errors ⇒ `syncFailureRetryCount++`, `onSyncError`, degrade after > `MAX_SYNC_FAILURE_RETRIES=5` (#1127); finally, if pending remain: retry delay `min(debounceMs * 2^(max(retryStreak)−1), MAX_RETRY_BACKOFF_MS=30_000)` else normal debounce. `getPendingFiles()` computes `indexing = syncing && syncStartedMs >= lastSeenMs`.

### Git hooks
`gitHooksDir` = `git rev-parse --git-path hooks` (cwd=root, 5000ms timeout — all git calls in this subsystem are 5s-bounded, #1139), resolved relative to root; honors `core.hooksPath` and worktrees. Install per hook (`post-commit`,`post-merge`,`post-checkout`): existing file → strip old marker block, trim trailing ws, append `\n\n<block>\n` (or fresh `#!/bin/sh\n<block>\n` if effectively empty); new file → `#!/bin/sh\n<block>\n`; chmod 0755 (best-effort). Remove: only files containing `MARKER_BEGIN`; strip block; delete file if only shebang/blank remains, else rewrite + chmod. Detection: any hook file containing `MARKER_BEGIN`.

### Worktree mismatch
`detectWorktreeIndexMismatch(startPath, indexRoot)` → null when: startPath not in git; `gitWorktreeRoot(startPath) === realpath(indexRoot)`; indexRoot not itself a worktree root; or the two trees have **different** `gitCommonDir`s (nested repo/submodule already covered by parent index, #1031/#1033). Otherwise `{worktreeRoot, indexRoot}`; `status` prints the multi-line warning, read tools prefix the one-line `⚠ …` notice.

## Wire/contract surfaces

- **Daemon socket paths**: `.codegraph/daemon.sock`; tmp fallback `os.tmpdir()/codegraph-<sha256(realpath(root)).hex[:16]>.sock`; win32 pipe `\\.\pipe\codegraph-<hash>`; limit constant 100.
- **`daemon.pid`**: `JSON.stringify({pid, version, socketPath, startedAt}, null, 2) + '\n'`, mode 0600. Decoder also accepts a bare decimal pid (legacy → `{pid, version:'unknown', socketPath:'', startedAt:0}`).
- **Daemon hello** (first line on every accept): `{"codegraph":"<semver>","pid":N,"socketPath":"…","protocol":1}` + `\n`. Version must match the proxy's **exactly** or the proxy falls back. `protocol` is literally `1`.
- **Client hello** (optional first proxy line): `{"codegraph_client":1,"pid":N,"hostPid":N|null}` + `\n`. Anything else is treated as JSON-RPC data.
- **Registry records**: `~/.codegraph/daemons/<sha256(resolve(root)).hex[:16]>.json` = `DaemonRecord` pretty-JSON + `\n`, mode 0600. Discovery only; live pid is truth; readers prune dead records.
- **After hello, the pipe is raw newline-delimited JSON-RPC** — the daemon side is a full MCP session per connection; the proxy does no parsing in classic mode and line-parsing in local-handshake mode.
- **Env vars (complete set for this subsystem)**: `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` (300000), `CODEGRAPH_DAEMON_MAX_IDLE_MS` (1800000), `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS` (30000), `CODEGRAPH_NO_DAEMON`, `CODEGRAPH_DAEMON_INTERNAL`, `CODEGRAPH_PPID_POLL_MS` (5000, 0=off), `CODEGRAPH_HOST_PPID`, `CODEGRAPH_WASM_RELAUNCHED`, `CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS` (900000, ≤0 off), `CODEGRAPH_NO_WATCHDOG`, `CODEGRAPH_WATCHDOG_TIMEOUT_MS` (60000), `CODEGRAPH_NO_WATCH`, `CODEGRAPH_FORCE_WATCH`, `CODEGRAPH_MAX_DIR_WATCHES` (50000), `CODEGRAPH_NO_PROMPT_HOOK`/`CODEGRAPH_PROMPT_HOOK`, `CODEGRAPH_ALLOW_UNSAFE_NODE`, `CODEGRAPH_MCP_LOG_ATTACH`, `CODEGRAPH_MCP_DEBUG`, `CODEGRAPH_VERSION`, `CODEGRAPH_DIR`, `CODEGRAPH_QUERY_POOL_SIZE`, `DO_NOT_TRACK`/`CODEGRAPH_TELEMETRY`.
- **`status --json`** (initialized): `{initialized:true, version, projectPath, indexPath, lastIndexed:ISO|null, fileCount, nodeCount, edgeCount, dbSizeBytes, backend, journalMode, nodesByKind, languages:[…], pendingChanges:{added,modified,removed}, worktreeMismatch:{worktreeRoot,indexRoot}|null, index:{builtWithVersion, builtWithExtractionVersion, currentExtractionVersion, reindexRecommended, state:'complete'|'partial'|'indexing'|'failed'|null, pendingRefs}}`. Un-initialized: `{initialized:false, version, projectPath, indexPath, lastIndexed:null}`.
- **Git hook markers** (byte-exact): `# >>> codegraph sync hook >>>` / `# <<< codegraph sync hook <<<`; snippet body: comment lines + `if command -v codegraph >/dev/null 2>&1; then` / `  ( codegraph sync >/dev/null 2>&1 & ) >/dev/null 2>&1` / `fi`.
- **Agent-facing un-indexed refusal** (`explore`/`node` CLI): `CodeGraph isn't available here — no .codegraph/ index exists in <path>. If you are an AI agent: continue with your usual tools; indexing is the user's decision, do not run it yourself. (The project owner can enable CodeGraph with 'codegraph init'.)`
- **prompt-hook stdout**: `<codegraph_context note="…">\n<body>\n</codegraph_context>\n`; 16000-char cap; gate counter names above are the telemetry contract.
- **Degrade strings** (surfaced to users via `onDegraded`): the `EXHAUSTION_REASON`, `INOTIFY_LIMIT_REASON`, lock-budget, and sync-failure-budget messages quoted in watcher.ts must be preserved verbatim-equivalent.
- **Exit-code semantics**: 0 = success/expected no-op (unlock with no lock, uninit-not-initialized, affected-no-input, daemon-none-running); 1 = genuine failure/not-initialized for query-class commands; `upgrade` returns its own code; prompt-hook always 0.

## Test coverage

Contract tests that must be ported (all under `__tests__/`, real FS + real sockets, no mocks):
- **`mcp-daemon.test.ts`** — end-to-end daemon semantics: two launchers share one daemon; concurrent launchers converge (lockfile race); daemon survives first client's death; `CODEGRAPH_NO_DAEMON=1` isolates; stale dead-pid lock cleared and taken over; version mismatch → direct fallback; live-but-quiet client NOT reaped by backstop; idle timeout fires after last disconnect; proxy survives daemon death mid-session (#662).
- **`daemon-client-liveness.test.ts`** — `parseClientHelloLine` accept/reject matrix; `peerIsDead` (unknown pid never dead; host-dead reaps even with live proxy); `reapDeadClients`; `backstopShouldExit` full decision table.
- **`daemon-socket-fallback.test.ts`** — candidate ordering; relocate on ENOTSUP/unexpected errno, never on EADDRINUSE; last-candidate error propagates; no-hardlink lock fallback.
- **`daemon-bind-failure.test.ts`** — all-candidates-fail releases lock; `finalizeDaemonExit` per-platform.
- **`daemon-registry.test.ts`** — register/list/deregister, dead-pid pruning, `prune:false`, newest-first ordering.
- **`daemon-manager.test.ts`** — `formatUptime`, pick ordering/sentinels, picker loop.
- **`proxy-connect.test.ts`** — socket never left without an error listener (#974).
- **`ppid-watchdog.test.ts`** — POSIX divergence vs win32 liveness vs host-pid, pid 0/1 rejection, signal precedence, env parsers.
- **`startup-handshake.test.ts` / `stdin-teardown.test.ts`** — one-shot semantics, disable conventions, no data theft, destroy-on-terminal.
- **`liveness-watchdog.test.ts`** — kills sync wedge (incl. non-allocating under heap pressure), spares healthy loop and slow-store-with-disk-progress, hard cap, opt-out. **`index-orphan-watchdog.test.ts`** — index self-terminates when parent SIGKILL'd.
- **`fatal-handler.test.ts`** — never reads `.stack`; bounded line + exit 1. **`node-version-check.test.ts`** — banner content pins.
- **`watcher.test.ts`** — lifecycle idempotency; EMFILE degrade-once vs ENOSPC warn-once; lock-contention and persistent-failure degradation with bounded retries and reset-on-clean-sync; debounce coalescing; ignore filters; pending-file tracking semantics (#403/#449).
- **`watch-policy.test.ts`** — env precedence, WSL /mnt detection, `/mnt/wsl` not flagged.
- **`git-hooks.test.ts`** — install/idempotent/preserve-user/remove/shared-file/`core.hooksPath`/non-repo.
- **`sync.test.ts`** — CodeGraph.sync change detection (mtime + git paths), ignore-matcher agreement (#766), cross-file ref resolution (#1240) — mostly extraction-layer but drives the `sync` command contract.
- **`worktree-detection.test.ts`** — mismatch matrix incl. submodule/gitlink suppression (#1031/#1033) and warning text.
- **`cli-version.test.ts`** — `-v`/`-version`/`--version`/`-V`/`version` all print exactly the version; `serve` hidden from help; trailing `-v` stays `--verbose`. **`cli-affected-paths.test.ts`** — path normalization (#825). **`cli-node-command.test.ts`**, **`cli-query-command.test.ts`** — arg routing / output.
- **`frontload-hook.test.ts`** — `planFrontload` monorepo resolution and `hasStructuralKeyword` multilingual gate (prompt-hook’s core).

## Rust port notes

- **Crate placement**: `src/bin/*` + all subcommand wiring → `selene-cli` (clap); daemon/proxy/registry/watchdogs/handshake → `selene-mcp` (the daemon is inseparable from the MCP session layer); `watcher/watch-policy/git-hooks/worktree` → `selene-sync`; `unsafeIndexRootReason`/`isInitialized`/root-walking helpers → `selene-core` or a small `selene-cli` util since both CLI and MCP need them. The liveness/PPID watchdog pair is shared by CLI commands and the server — put it in `selene-mcp` or a tiny shared module both depend on.
- **Node-runtime baggage to drop**: the Node 25 / Node <20 gates, `--liftoff-only` re-exec, and `CODEGRAPH_WASM_RELAUNCHED` are Node/V8-specific — a static Rust binary deletes them, **but** `CODEGRAPH_HOST_PPID` threading must survive in some form if any wrapper/shim re-execs, and `EARLY_PPID` becomes "capture `getppid()` first thing in `main`". On POSIX Rust, prefer `prctl(PR_SET_PDEATHSIG)` (Linux) / `kqueue` `EVFILT_PROC` (macOS) over ppid polling where possible, but keep the polling `supervisionLostReason` logic as the portable contract (Windows: `OpenProcess` + wait handle replaces the liveness poll).
- **Liveness watchdog**: the "separate process, not thread" rationale (V8 safepoints) does not apply to Rust — a wedged tokio worker doesn't stall a dedicated OS thread. A plain watchdog thread + `std::process::abort`/SIGKILL-self suffices; keep the disk-progress deferral (`progressPaths` fingerprint = size+mtime, 10× cap) since a long SurrealDB/SQLite statement on slow storage is the same hazard (#1231).
- **Socket layer**: `daemon.sock` semantics map to `tokio::net::UnixListener` + `interprocess`/`tokio` named pipes on Windows. The `socket.unshift()` put-back trick (client-hello tail, hello tail in the proxy) has no direct Rust analog — model reads through an explicit buffered framing layer that owns leftover bytes instead. The pause-before-detach hazard (#662) disappears if one owner reads the stream from accept to session end.
- **Hard-link lock**: `fs::hard_link` is atomic+exclusive on POSIX and NTFS; keep the O_EXCL (`OpenOptions::new().write(true).create_new(true)`) fallback for ExFAT/network mounts. Keep the pid-verified compare-and-delete for stale locks.
- **Version match**: daemon/proxy compare full semver strings for exact equality — keep exact, do not "compatible-range" it.
- **File watching**: `notify` crate gives FSEvents/ReadDirectoryChangesW/inotify; you get recursive watching on Linux too, so the per-directory walk + `CODEGRAPH_MAX_DIR_WATCHES` cap + ENOSPC handling may collapse — but `notify`'s inotify backend still consumes one watch per directory, so the ENOSPC warn-once and the degrade taxonomy (fatal EMFILE/ENFILE vs non-fatal ENOSPC) still apply. Keep debounce=2000ms, retry budgets (5/5), backoff `debounce·2^(n−1)` cap 30s, and the pending-files false-stale-over-false-fresh rule — they're MCP-facing behavior.
- **TS quirks/bugs to not replicate**: (1) file-header usage comment still lists a `codegraph context <task>` subcommand that no longer exists; (2) `globToRegex` in `files --pattern` is unanchored (substring match) while `affected --filter` builds a different unanchored regex with different escaping — unify deliberately and document; (3) `impact`'s fallback branch overwrites `edgeCount` with the unmerged count; (4) `callers`/`callees` exact-match filter is skipped when there is exactly one fuzzy match (`matches.length > 1` guard) — intentional but surprising; (5) daemon-registry doc comment refers to `codegraph list` / `codegraph stop --all`, which don't exist as subcommands (the `daemon` picker is the real surface); (6) `resolveProjectPath`'s walk duplicates `findNearestCodeGraphRoot` with a stricter DB check — port one function with a `require_db` flag.
- **Error semantics invariant**: expected conditions print guidance and exit 0/1 as tabulated above; only genuine malfunctions get stack-trace-style output. The prompt-hook's "exit 0, silent, never break the prompt" contract and the agent-facing un-indexed message are behavioral contracts — port their exact text.
- **Async model**: the daemon multiplexes N sessions on one loop and offloads reads to a query pool (`CODEGRAPH_QUERY_POOL_SIZE=0` disables); in Rust this is naturally tokio tasks + a rayon/spawn_blocking pool around the `GraphStore` — decide per PRD §5.4 before freezing the trait.
