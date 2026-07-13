# Phase 6 — `selene-cli` + the daemon + `selene-sync`: the binary a human drives — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 5 made `selene` a thing an **agent** talks to. Phase 6 makes it a thing a
**human installs, runs, and forgets about** — all 22 subcommands with their exact flags,
output and **exit codes**; the shared **daemon** (socket protocol, lockfile arbitration,
refcount + idle timeout, PPID watchdog, stdio proxy); the **FileWatcher** (debounce, degrade
policy, WSL2 policy); **git hooks**; the **prompt-hook**; and the terminal UI.

```
selene (bin)  →  selene-cli  ──────────────────────────────┐
                    │  22 subcommands, exit codes, term UI │
                    ├─→ selene-sync   watcher · git hooks · watch policy · worktree
                    ├─→ selene-mcp    daemon · proxy · registry · watchdogs   (Phase 5's crate,
                    │                                                          extended here)
                    └─→ selene-graph / selene-context / selene-db  (via ONE seam: `GraphAccess`)
```

**THE GATE (Task 22):** daemon lifecycle integration tests — **spawn / reuse / idle-exit /
takeover** — driving the **real binary** over a **real socket**. Not a mocked listener, not an
in-process `Daemon::start()`. Two `selene serve --mcp` launchers, one daemon; kill the holder,
watch the survivor take the lock over; let it go idle, watch it exit.

**Tech stack (what is already pinned — prefer these):** `clap` 4.6 derive, `tokio` (the pinned
feature set **already includes `net`, `process`, `signal`, `fs`, `io-std`** — the socket layer
needs no new dep), `notify` 8, `sha2` (socket-path hashing), `serde`/`serde_json`, `thiserror`
2, `anyhow` (bin only), `indicatif` 0.18 + `crossterm` 0.29 (terminal UI), `toml`/`toml_edit`,
`ignore` (the watcher's ignore matcher — **the same `ScopeIgnore` the indexer uses**, not a
second one), `wait-timeout` (the 5 s-bounded `git` calls), `tempfile`/`insta` (dev).
**New deps are enumerated and justified in Task 1 — there are four, and no more.**

**Reference (in priority order):**
- `docs/reference/from-codegraph/maps/cli-daemon-sync.md` — **THE parity contract for this
  phase.** Every constant, timeout, path, marker string and exit code in this plan is copied
  from it **verbatim**. A task should never need to open the map to execute; it opens it when
  this plan is ambiguous, and the map wins when they disagree.
- `docs/plans/2026-07-12-selenecode-roadmap.md` §Phase 6 — scope and the gate.
- `docs/plans/2026-07-13-phase45-graph-context-mcp.md` — the house form, and the **inert-seam
  lesson** this plan is written against (below). Its Task 19 defines the `PendingFiles`
  provider + the staleness/degraded banners whose **only real implementation is Phase 6's
  watcher** — closing that seam is Task 20 here, and it is not optional.
- `crates/selene/src/main.rs` — the **live** binary (`index`, `serve --mcp`). Phase 6
  **extends** it; Task 2 must keep both of those commands byte-identical, because the Phase 5
  dogfood gate drives them.
- `crates/selene-db/src/surreal.rs` §`connect_disk_with_lock_retry` — read it before Task 1.
  It is why **Open Question 1** exists and why it is the most important sentence in this plan.
- `docs/reference/rust-ecosystem-2026-07.md` §4 — the supporting-crate pins.

---

## Global Constraints (bind every task — reviewers use this as the rubric)

- **`isError` is RESERVED — and so is a non-zero exit code.** The CLI is the human-facing
  mirror of the MCP invariant. Only a **genuine malfunction**, a **security refusal**
  (`Error::PathRefusal`), and the map's explicitly-tabulated failure rows exit non-zero.
  **Every expected condition is success-shaped:** `unlock` with no lock → **0**; `uninit` on an
  un-initialized project → **0**; `affected` with no inputs → **0**; `daemon` with none running
  → **0**; `prompt-hook` **always** → **0**. Exit codes are a wire contract (Task 2 pins them in
  one table and one test).
- **Never break the caller's prompt.** `prompt-hook` exits **0** on every path, including panic.
  It is wired into a user's Claude session; a non-zero exit there is a user-visible regression
  in a tool they did not ask about.
- **Errors are collected, never thrown.** A watcher that cannot install one directory watch
  degrades that watch, not the process. A sync that fails takes its retry budget (5), not the
  daemon. A subcommand collects and prints; it does not panic.
- **No `unwrap`/`expect` outside `#[cfg(test)]`.** Workspace lints already warn
  (`clippy::unwrap_used`, `expect_used`). The bin may use `anyhow`; the libraries use
  `thiserror` and return `Result`. **A panic in the daemon kills every attached agent session** —
  this constraint is load-bearing here in a way it was not in Phase 4.
- **`#![forbid(unsafe_code)]`-compatible.** The detached-daemon spawn does **not** need
  `pre_exec`: `std::os::unix::process::CommandExt::process_group(0)` is safe and stable, and
  `CREATE_NEW_PROCESS_GROUP` covers Windows. If a task believes it needs `unsafe`, it stops and
  asks (workspace lint: `unsafe_code = "warn"`).
- **`S: GraphStore`-generic wherever it touches the store.** `GraphStore` is not `dyn`-safe
  (RPITIT). Thread the type parameter; never `Box<dyn GraphStore>`. The **one** sanctioned
  erasure boundary in this phase is `GraphAccess` (Task 5), which is an *enum*, not a trait
  object.
- **Determinism.** Same repo + same command ⇒ byte-identical stdout. Sorted output everywhere
  (`files`, `affected`, `status`'s kind/language tables, the daemon list). No `HashMap`
  iteration order reaching output. No wall-clock in output except where the map prints one
  (uptime, `lastIndexed`), and those are pinned by a fake clock in tests.
- **Every constant is the map's, verbatim.** If a task writes "a reasonable timeout" instead of
  `300_000`, it has failed. The numbers were decided years ago in production; re-deriving them
  is how a subtle regression enters (a 1 s client-hello timeout looks fine and breaks a
  cold-start proxy on a loaded laptop).
- **Env vars carry the `SELENE_` prefix** (per Phase 5's ruling). The map's `CODEGRAPH_*` names
  map 1:1: `SELENE_DAEMON_IDLE_TIMEOUT_MS`, `SELENE_DAEMON_MAX_IDLE_MS`,
  `SELENE_DAEMON_CLIENT_SWEEP_MS`, `SELENE_NO_DAEMON`, `SELENE_DAEMON_INTERNAL`,
  `SELENE_PPID_POLL_MS`, `SELENE_HOST_PPID`, `SELENE_STARTUP_HANDSHAKE_TIMEOUT_MS`,
  `SELENE_NO_WATCHDOG`, `SELENE_WATCHDOG_TIMEOUT_MS`, `SELENE_NO_WATCH`, `SELENE_FORCE_WATCH`,
  `SELENE_MAX_DIR_WATCHES`, `SELENE_NO_PROMPT_HOOK`/`SELENE_PROMPT_HOOK`, `SELENE_DIR`,
  `SELENE_MCP_DEBUG`. **Dropped as Node baggage** (map §Rust port notes): `*_WASM_RELAUNCHED`,
  `*_ALLOW_UNSAFE_NODE` (no Node gate in a static binary). **Deferred to Phase 8:**
  `*_VERSION`, `DO_NOT_TRACK`/`*_TELEMETRY` (upgrade + telemetry).
- **Single source of tool guidance.** The MCP `server-instructions` remain the **one** place
  agent-facing guidance lives. CLI `--help` text describes flags to a **human**; it does not
  re-teach the agent how to use `explore`. The one agent-facing string the CLI owns is the
  **un-indexed refusal** (map §Wire — ported verbatim, Task 9), because `explore`/`node` can be
  invoked *by* an agent from a shell.
- **One production call site, named, per capability.** See below.
- **Tasks are completable by a fresh subagent in one session** (~1–3 files), are **TDD** (port
  the contract test first, watch it fail, then implement), and end in **one conventional
  commit**. `cargo fmt && cargo clippy --all-targets && cargo test` green before every commit.

### The lesson this plan is written against

Four seams in this project shipped with green unit tests and **zero production callers** — a
library nobody invoked, passing its own tests. Not one was caught by a unit test, because

> **a seam that returns "nothing found" is indistinguishable from a seam that works and found
> nothing.**

Binding consequences, not advice:

1. **Task 2 lands the full 22-subcommand clap surface and its dispatch FIRST**, before a single
   subcommand body exists. Every later task fills a dispatch arm that is **already reachable
   from `main()`**. A subcommand implemented in `selene-cli` but not wired into the dispatch is
   exactly the bug class — so **every task that adds a subcommand carries this acceptance
   criterion verbatim:**
   > - [ ] **Production call site:** `selene <cmd> …` invoked as a **subprocess** in an
   >   integration test produces the asserted stdout/stderr **and exit code**. Not
   >   `run_cmd()` called in-process — the binary.
2. **The gate drives the real binary over a real socket** (Task 22). A daemon test that calls
   `Daemon::start()` in-process proves nothing about the thing the user runs.
3. **A "no results" assertion is not a test.** Every test whose passing state is empty output
   must be paired with a positive control on the same fixture that proves the path can produce
   a non-empty result at all.
4. **Phase 5's `NoWatcher` is a known, documented inert seam** (its plan says so). **Task 20
   closes it.** If Phase 6 ships without wiring the real watcher into the staleness banners,
   this phase has *added* a fifth one instead of removing the fourth.

---

## The exit-code contract (map §Exit-code semantics — the whole table, in one place)

Pinned by **one** table in `selene-cli/src/exit.rs` and **one** test that shells out to the
real binary for every row. This is the contract; a subcommand does not get to invent a code.

| Code | Meaning | Rows (map §Subcommands) |
|---|---|---|
| **0** | success, **or an expected no-op** | `unlock` with no lock file; `uninit` on an un-initialized project; `affected` with no inputs; `daemon` with none running; `init` on an already-initialized project (warn + `offer_watch_fallback`); `serve --mcp` refusing a TTY stdin (prints the explanation); **`prompt-hook`, always, on every path** |
| **1** | genuine failure, **or "not initialized" for a query-class command** | `init` on an `unsafe_index_root_reason` root without `--force`; `init` error; `index` when `!result.success`; `sync` on an un-initialized project (silent under `-q`); `status`/`query`/`callers`/`callees`/`impact`/`files` on an un-initialized project; `explore`/`node` when the tool result `isError` (incl. un-indexed → the agent-facing refusal text); `node` with neither name nor file (usage); `telemetry <unknown-action>`; `install` with an invalid `--location` |
| *(its own)* | `upgrade` returns the code `run_upgrade` returns | Phase 8 (see Open Question 2) |

**The one asymmetry, and it is deliberate:** an un-indexed project is *success-shaped* over
**MCP** (guidance, `isError: false`) and *exit 1* over the **CLI** (`explore`/`node` print the
agent-facing refusal text **and exit 1**). That is the map's contract (§Subcommands `explore`),
and it is right: a shell caller checks `$?`; an agent mid-session must not be told "error".
Port both. Do not "unify" them.

---

## File structure

```
crates/selene-cli/
  Cargo.toml            clap, tokio, anyhow/thiserror, indicatif, crossterm, dialoguer, serde_json,
                        toml, sha2, dirs, selene-{core,db,extract,resolve,graph,context,mcp,sync}
  src/lib.rs            [T2 creates / T21 ledger pass] `pub async fn run(cli: Cli) -> ExitCode`
  src/cli.rs            [T2] THE clap tree: all 22 subcommands, every flag, hidden flags
  src/exit.rs           [T2] the exit-code table above + `Outcome` → `ExitCode`
  src/fatal.rs          [T3] panic hook → ONE bounded stderr line, exit 1, never a backtrace
  src/paths.rs          [T3] resolve_project_path(require_db), find_nearest_selene_root,
                        is_initialized, unsafe_index_root_reason
  src/term.rs           [T3] TTY detection, colors, `indicatif` progress, confirm prompts
  src/access.rs         [T5] ⚠ `GraphAccess` — THE daemon-vs-direct seam (Open Question 1)
  src/cmd/mod.rs        [T2 creates] one `pub async fn` per subcommand; T2 stubs, T4–T13 fill
  src/cmd/lifecycle.rs  [T4]  init · uninit · unlock
  src/cmd/index.rs      [T6]  index · sync  (+ command supervision)
  src/cmd/status.rs     [T7]  status (text + --json)
  src/cmd/query.rs      [T8]  query · callers · callees · impact · files  (ONE formatter)
  src/cmd/agent.rs      [T9]  explore · node  (MCP-shaped; the un-indexed refusal text)
  src/cmd/affected.rs   [T10] affected (BFS, #825 path normalization, glob→regex)
  src/cmd/daemon.rs     [T17] the `daemon` picker (pure pick logic + clack-equivalent)
  src/cmd/serve.rs      [T18] serve (mode decision) — the ONLY command that routes to the daemon
  src/cmd/hook.rs       [T19] prompt-hook
  src/cmd/misc.rs       [T21] version · install · uninstall · telemetry · upgrade (delegations)

crates/selene-sync/
  Cargo.toml            notify 8, ignore, tokio, wait-timeout, selene-{core,extract}, thiserror
  src/lib.rs            [T11 creates / T14 ledger pass] re-exports
  src/policy.rs         [T11] watch_disabled_reason, detect_wsl (cached)
  src/worktree.rs       [T11] git_worktree_root, detect_worktree_index_mismatch, warning strings
  src/hooks.rs          [T12] install/remove/is_installed; the byte-exact marker block
  src/watcher.rs        [T13] FileWatcher: debounce 2000, pending files, degrade, retry budgets
  src/git.rs            [T11] the 5000 ms-bounded `git` runner every call in this crate uses

crates/selene-mcp/                       (Phase 5's crate — the daemon lands INSIDE it, per the map)
  src/daemon/paths.rs   [T14] socket candidates, POSIX_SOCKET_PATH_LIMIT=100, daemon.pid codec
  src/daemon/lock.rs    [T14] hard-link lock + O_EXCL fallback, stale compare-and-delete
  src/daemon/registry.rs[T15] ~/.selene/daemons/<hash>.json — register/list/deregister/prune/stop
  src/daemon/server.rs  [T16] Daemon: bind, hello, client-hello, refcount, idle, sweep, shutdown
  src/daemon/proxy.rs   [T18] connect_with_hello, the stdio proxy, spawn_detached_daemon
  src/watchdog/ppid.rs  [T16] supervision_lost_reason + env parsers + EARLY_PPID
  src/watchdog/liveness.rs [T16] the wedge killer (watchdog THREAD, not a child process)
  src/pending.rs        [T20] ⚠ the REAL `PendingFiles` impl over `selene-sync` — closes Phase 5's
                        documented `NoWatcher` inert seam

crates/selene/src/main.rs [T2] shrinks to a ~15-line shim: `installFatalHandlers` → parse →
                        `selene_cli::run(cli).await` → `ExitCode`. ⚠ `index` and `serve --mcp`
                        must keep byte-identical behavior — the Phase 5 gate drives them.

tests:
  selene-cli/tests/exit_codes.rs        [T2]  every row of the table, via the real binary
  selene-cli/tests/{lifecycle,index_sync,status,query,agent,affected,hook}_test.rs  [T4–T19]
  selene-sync/tests/{policy,worktree,hooks,watcher}_test.rs                        [T11–T13]
  selene-mcp/tests/daemon_{paths,lock,registry,liveness}_test.rs                   [T14–T16]
  selene-mcp/tests/daemon_lifecycle_gate.rs   [T22] ⚠ THE GATE — real binary, real socket
  docs/benchmarks/2026-07-phase6-daemon.md    [T22] gate results
```

---

## ⚠ Task sequencing — the shared seams

Tasks touching the same file are **strictly sequential** — never dispatch two of them to
parallel subagents or worktrees.

| Shared file | Tasks | Rule |
|---|---|---|
| `selene-cli/src/cli.rs` + `cmd/mod.rs` | **2** (creates: all 22 arms as stubs), 4–21 (each fills its own arm) | 2 lays down **every** arm with a `// TODO(Task N)` body that exits 1 with `"not yet implemented"`. Later tasks **replace one body**; none adds a variant. If a task needs a new flag, it edits `cli.rs` — that is the one line of collision, and it is why 4–21 are sequential *on this file only*. |
| `crates/selene/src/main.rs` | **2** (rewrites to a shim), 22 (drives it — read-only) | After 2, nobody edits it. Phase 5's `index`/`serve --mcp` behavior is **regression-tested** by 2. |
| `selene-cli/src/access.rs` | **5** (creates), 7–10 (consume) | ⚠ **Open Question 1 lands here and NOWHERE else.** Whatever the maintainer rules, exactly one file changes. |
| `selene-mcp/src/lib.rs` | **14** (adds `mod daemon`), 15, 16, 18, 20; **21** (ledger pass) | Append-only, one `mod`/`pub use` per task. |
| `selene-mcp/src/daemon/server.rs` | **16** (creates), **18** (the proxy connects to it; read-only), **22** (drives it) | 16 → 18 → 22, strictly. |
| `selene-sync/src/lib.rs` | **11** (creates), 12, 13; **14** (ledger) | Append-only. |
| `selene-cli/src/cmd/serve.rs` | **18** (creates: the mode decision + proxy), **20** (adds the watcher start) | 18 → 20. |
| `selene-mcp/src/pending.rs` + Phase 5's `banners.rs` | **20** only | 20 is the **only** task allowed to touch Phase 5's banner layer — and it must not rewrite the banner *strings*, only feed them a real provider. |

**Parallelizable once Task 2 lands:** `selene-sync` (T11 → T12/T13 can run beside the CLI
tasks), and T14/T15 (daemon paths/lock/registry are fresh files). Everything in
`selene-cli/src/cmd/` is sequential on `cli.rs`.

---

## Deliberately deferred (each with its phase and its reason)

- **`install` / `uninstall` bodies → Phase 7** (`selene-installer`: 8 targets, the ~97 contract
  tests). Phase 6 lands the **flag surface, the `--location` validation, the exit codes, and
  the delegation call site** — so Phase 7 fills a function that is already called by a live
  dispatch arm, not a library nobody invokes. See **Open Question 2**.
- **`telemetry` / `upgrade` bodies → Phase 8.** Same shape: flags + exit codes + call site now,
  body later. The prompt-hook's telemetry **counters** (`cli_command:prompt-hook-gate-<outcome>`)
  are a Phase-8 contract — Task 19 emits them through a `Telemetry` trait whose Phase-6 impl is
  a no-op, **and** (per the inert-seam rule) ships with a fake that asserts the counter names.
- **The Node-runtime gates** (`MIN_NODE_MAJOR`, the Node-25 banner, `--liftoff-only` re-exec,
  `WASM_RELAUNCHED`) — **deleted, not ported.** A static binary has no runtime to gate.
  `EARLY_PPID` survives as "capture `getppid()` first thing in `main`" and `SELENE_HOST_PPID`
  survives for any future shim (map §Rust port notes).
- **The liveness watchdog as a separate *process*** — **collapsed to a thread.** The map is
  explicit: the "separate process, not thread" rationale is V8 safepoints; a wedged tokio worker
  does not stall a dedicated OS thread. Keep the **disk-progress deferral** (`progress_paths`
  size+mtime fingerprint, `PROGRESS_CAP_MULTIPLIER = 10`) — a long SurrealDB statement on slow
  storage is the same hazard (#1231).
- **`format_subgraph_tree`** — Phase 4 deferred it here as "port only if the CLI needs it."
  **It does not.** No subcommand in the map renders a subgraph tree. Leave it unported.
- **Windows named pipes** — the code paths are **written and `cfg`-gated** (Task 14/16 carry the
  `\\.\pipe\selene-<hash>` candidate and `finalize_daemon_exit`'s win32 arm), but **CI gates
  POSIX only** (roadmap §Risks: "integration-test the POSIX paths in CI"). A Windows box
  validates them post-v1. Do not silently drop the win32 arms — a dropped arm is a rewrite; a
  `cfg`-gated untested arm is a port.
- **`notify-debouncer-full`** — **deliberately NOT adopted**, though the ecosystem doc suggests
  it. The map's debounce is not a generic debounce: it is a stateful flush loop with
  `pending_files`, `sync_started_ms`, the *false-stale-over-false-fresh* rule, two retry budgets
  and an exponential backoff. A generic debouncer would hide exactly the semantics the tests
  pin. Raw `notify` + our own flush loop (Task 13).

---

## Tasks

<!-- 22 tasks. Each is one commit. Task 22 is THE GATE. -->

### Task 1: Spike — the IPC layer, process liveness, the hard-link lock, and `notify`'s real behavior

**Files:** Create: `crates/selene-mcp/tests/spike_ipc.rs`, `crates/selene-sync/tests/spike_notify.rs`.
Modify: root `Cargo.toml` (`[workspace.dependencies]`), `crates/selene-mcp/Cargo.toml`,
`crates/selene-sync/Cargo.toml`.

**Interfaces:** none — throwaway knowledge, kept as two smoke tests. **Every finding goes into a
comment block at the top of the spike file, and into this plan** (edit the affected task; do not
silently diverge).

The daemon is the concurrency-critical part of the product and it rests on four things nobody
here has checked. Front-load them.

- [ ] **The socket layer — and confirm it needs NO new dep.** `tokio`'s pinned feature set
  already has `net`. Prove: `tokio::net::{UnixListener, UnixStream}` binds `.selene/daemon.sock`,
  accepts, and round-trips a newline-delimited line. Then prove the Windows arm compiles
  (`tokio::net::windows::named_pipe::{ServerOptions, ClientOptions}` behind `#[cfg(windows)]`) —
  `cargo check --target x86_64-pc-windows-msvc` if the target is installable, else record it as
  unverified + `cfg`-gated. **If the two backends cannot be unified behind one
  `AsyncRead + AsyncWrite` cheaply, record the shape to use instead** (`enum Transport {
  Unix(UnixStream), Pipe(NamedPipeClient) }` with hand-written `AsyncRead`/`AsyncWrite`
  delegation is the expected answer — write it here so Task 16 copies it).
  ⚠ **Do NOT reach for `interprocess` reflexively.** It is a fine crate, but a new dep whose only
  job is a `cfg` we can write in 40 lines is a dep we maintain forever. Adopt it **only** if this
  spike proves tokio's named-pipe API cannot express accept-loop + hello.
- [ ] **The `socket.unshift()` problem has no Rust analog — design its replacement here** (map
  §Rust port notes). TS pauses the socket, reads an optional client-hello line, and *puts the
  unconsumed bytes back*. In Rust: **one owner reads the stream from accept to session end**
  through a `BufReader` that keeps its leftover buffer. Prove it: read one `\n`-terminated line
  with a **3 000 ms** timeout and a **4 096-byte** cap, then hand the *same* `BufReader` (not the
  raw stream) to a second consumer and assert **zero bytes are lost** when the client sends
  `hello\n{"jsonrpc":…}\n` in **one** `write_all`. That single test is the whole of #662.
- [ ] **Process liveness: `is_process_alive(pid)`.** The contract is `kill(pid, 0)` with
  **`EPERM` ⇒ alive**. std cannot do it. Decide and record **one** of: `rustix` (`process`
  feature), `nix` 0.30, or raw `libc`. **Recommendation: `rustix`** — maintained, no `unsafe` at
  our call site. Windows arm: `windows-sys` `OpenProcess` + `GetExitCodeProcess`, `cfg`-gated.
  **Test the EPERM branch explicitly** — pid 1 is alive-but-not-signalable for a non-root user,
  and that is the exact case a naive `kill(pid,0).is_ok()` check gets **backwards** (it would
  declare the process dead and reap a live daemon). Also pin: **pid 0 and pid 1 are never valid
  supervision targets** (map §PPID watchdog).
- [ ] **The hard-link lock.** Prove `std::fs::hard_link(tmp, pid_path)` is atomic + exclusive:
  write the full JSON record to `<pid_path>.<pid>.tmp` mode `0600`, `hard_link` it into place,
  assert a **second** `hard_link` fails with `AlreadyExists`. Then prove the fallback:
  `OpenOptions::new().write(true).create_new(true)` (O_EXCL) for filesystems without hard links
  (#997: ENOTSUP/EPERM/EISDIR). Record **which `io::ErrorKind` each errno maps to** —
  `ErrorKind::AlreadyExists` is the **only** one that means "taken"; everything else means "try
  the fallback", and getting that backwards silently disables the lock on ExFAT/network mounts.
- [ ] **`notify` 8 — what it actually delivers.** The map's watcher is written against Node's
  `fs.watch`. Record, from a real run: does `RecommendedWatcher` + `RecursiveMode::Recursive` give
  **one** watch for the whole tree on macOS (FSEvents, O(1) fds)? What `EventKind`s arrive for
  create/modify/remove/rename? **Does an editor's atomic save (write-temp + rename) arrive as
  `Modify(Name(Both))` or as remove+create?** — that decides whether Task 13's filter needs a
  rename arm, and getting it wrong means *saved files never re-index*, which is the worst
  possible silent failure in this crate. And: the inotify backend is still one-watch-per-directory,
  so the ENOSPC warn-once and the `SELENE_MAX_DIR_WATCHES` (**50 000**) cap still apply — confirm
  the **fatal EMFILE/ENFILE vs non-fatal ENOSPC** taxonomy survives `notify`'s error type
  (`ErrorKind::MaxFilesWatch` vs `Io(e)`); a collapsed taxonomy turns a recoverable warning into a
  permanent degrade.
- [ ] **⚠ THE BIG ONE — can two processes open the same `.selene/graph.db`?** Read
  `crates/selene-db/src/surreal.rs` §`connect_disk_with_lock_retry`: the on-disk engine takes an
  exclusive **LOCK** file. Now *prove it*: open a `SurrealStore` on a temp dir in process A, and
  from a **second process**, open the same dir. Record exactly what happens (error kind, message,
  whether the retry loop eventually succeeds or gives up, and how long it waits). **This single
  fact decides the daemon's entire reason for existing in Rust** — Open Question 1 cannot be
  adjudicated without it. Write the observed behavior into the spike comment **verbatim**; the
  maintainer will read it.
- [ ] **The new deps — four, and no more.** Add to `[workspace.dependencies]` with a one-line
  justification each, house style:
  - **`rustix`** (or `nix`) — `kill(pid, 0)`. std cannot. The daemon's whole liveness model is
    this one syscall.
  - **`dirs`** — `~/.selene/daemons/` (the registry) and the global agent-config locations Phase 7
    needs. `std::env::var("HOME")` is wrong on Windows.
  - **`dialoguer`** — the `(y/N)` confirms (`uninit`) and the `daemon` picker (the clack
    equivalent). Same family as the pinned `indicatif`/`console`. ⚠ **If the spike finds the
    already-pinned `crossterm` makes the picker cheap, DROP this dep and say so** — one fewer dep
    is the better outcome.
  - **`windows-sys`** — `#[cfg(windows)]` only: `OpenProcess` for the win32 liveness fallback.
  ⚠ `notify-debouncer-full` is **NOT** adopted (see Deferred). `interprocess` is adopted **only**
  if the socket bullet proves tokio insufficient.
- [ ] Commit: `chore(cli): spike the daemon IPC layer, process liveness, hard-link lock, notify 8`

### Task 2: `selene-cli` — the clap tree for **all 22 subcommands**, the exit-code table, the dispatch

**Files:** Create: `crates/selene-cli/src/{cli.rs, exit.rs, cmd/mod.rs}`; rewrite
`crates/selene-cli/src/lib.rs` + `crates/selene-cli/Cargo.toml`; create
`crates/selene-cli/tests/exit_codes.rs`; rewrite `crates/selene/src/main.rs` (→ a shim).

**This task is the anti-inert-seam move, and it comes first for that reason.** Every subcommand
arm exists and is **reachable from `main()`** before a single body is written. A later task
*replaces a body*; it never has to remember to "wire it up", because the wire is already there
and its test already fails.

**Interfaces:**
```rust
// cli.rs — the ONE clap tree. 22 subcommands, exactly the map's flags.
#[derive(Parser)]
#[command(name = "selene", version, about = "Local-first code intelligence, in Rust.")]
pub struct Cli { #[command(subcommand)] pub command: Command }

#[derive(Subcommand)]
pub enum Command {
    Init   { path: Option<PathBuf>, #[arg(short, long)] index: bool,   // deprecated NO-OP
             #[arg(short, long)] force: bool, #[arg(short, long)] verbose: bool },
    Uninit { path: Option<PathBuf>, #[arg(short, long)] force: bool },
    Index  { path: Option<PathBuf>, #[arg(short, long)] force: bool,
             #[arg(short, long)] quiet: bool, #[arg(short, long)] verbose: bool },
    Sync   { path: Option<PathBuf>, #[arg(short, long)] quiet: bool },
    Status { path: Option<PathBuf>, #[arg(short, long)] json: bool },
    Query  { search: String, #[arg(short, long)] path: Option<PathBuf>,
             #[arg(short, long, default_value_t = 10)] limit: usize,
             #[arg(short, long)] kind: Option<String>, #[arg(short, long)] json: bool },
    Explore{ query: Vec<String>, #[arg(short, long)] path: Option<PathBuf>,
             #[arg(long)] max_files: Option<usize> },
    Node   { name: Option<String>, #[arg(short, long)] path: Option<PathBuf>,
             #[arg(short, long)] file: Option<String>, #[arg(long)] offset: Option<usize>,
             #[arg(long)] limit: Option<usize>, #[arg(long)] symbols_only: bool },
    Files  { #[arg(short, long)] path: Option<PathBuf>, #[arg(long)] filter: Option<String>,
             #[arg(long)] pattern: Option<String>,
             #[arg(long, value_enum, default_value_t = FilesFormat::Tree)] format: FilesFormat,
             #[arg(long)] max_depth: Option<usize>, #[arg(long)] no_metadata: bool,
             #[arg(short, long)] json: bool },
    Callers{ symbol: String, /* -p, -l default 20, -j */ },
    Callees{ symbol: String, /* -p, -l default 20, -j */ },
    Impact { symbol: String, #[arg(short, long, default_value_t = 2)] depth: u32, /* -p, -j */ },
    Affected{ files: Vec<String>, #[arg(long)] stdin: bool,
             #[arg(short, long, default_value_t = 5)] depth: u32,
             #[arg(short, long)] filter: Option<String>, /* -j, -q */ },
    #[command(alias = "daemons")] Daemon,
    #[command(hide = true)] Serve { #[arg(short, long)] path: Option<PathBuf>,
             #[arg(long)] mcp: bool, #[arg(long)] no_watch: bool },
    Unlock { path: Option<PathBuf> },
    #[command(hide = true)] PromptHook,
    Install  { /* -t, -l global|local, -y, --no-permissions, --print-config <id>, --refresh */ },
    Uninstall{ /* -t, -l, -y, --keep-cli */ },
    Telemetry{ action: Option<String> },     // status|on|off; unknown → exit 1
    Upgrade  { version: Option<String>, #[arg(long)] check: bool, #[arg(short, long)] force: bool },
    Version,
}

// exit.rs — THE contract. One table (the section above), one test file.
pub enum Outcome { Ok, ExpectedNoOp, Failure, Code(u8) }
impl From<Outcome> for std::process::ExitCode { /* Ok|ExpectedNoOp → 0, Failure → 1, Code(c) → c */ }

// lib.rs
pub async fn run(cli: Cli) -> std::process::ExitCode;    // the ONE dispatch. 22 arms.
```

- [ ] **All 22 arms are present and dispatched.** Each `cmd::<name>()` exists with a body that
  prints `selene <name>: not yet implemented (Task N)` to **stderr** and returns
  `Outcome::Failure`. A later task replaces exactly one body. **A test asserts the variant count
  is 22** — a dropped subcommand is then a red test, not a silent hole.
- [ ] **`serve` and `prompt-hook` are `hide = true`** (map §Subcommands). Port
  `cli-version.test.ts`'s assertion that `serve` is **absent from `--help`**.
- [ ] **The `-v` pre-parse intercept** (map §CLI bootstrap step 6): `argv[1] == "-v"` or
  `"-version"` prints the version, exit **0** (clap itself only handles `-V`/`--version`; the
  `version` subcommand also exists). ⚠ The trap the TS test pins: a **trailing** `-v` (e.g.
  `selene index -v`) must stay `--verbose` — the intercept fires **only** at `argv[1]`.
- [ ] **The exit-code table** lives in `exit.rs` as `Outcome`'s doc comment, and
  `tests/exit_codes.rs` shells out to the **real binary** for every row a stub can already produce
  (`--help` → 0, `--version` → 0, `-v` → 0, unknown subcommand → clap's **2**, every stub → 1).
  **Each later task appends its rows to this one file**, so the contract stays in one place.
- [ ] **⚠ REGRESSION GUARD — `index` and `serve --mcp` keep byte-identical behavior.** They are
  live today and **the Phase 5 dogfood gate drives them**. `main.rs` becomes a shim (capture
  `EARLY_PPID` → `install_fatal_handlers()` → `Cli::parse()` → `selene_cli::run(cli)` →
  `ExitCode`), and `cmd::index`/`cmd::serve` **move** the existing bodies out of `main.rs`
  unchanged. Acceptance: `cargo test -p selene-mcp` (incl. the dogfood gate) is green **before and
  after**, and `selene index <fixture>` prints the same lines it printed before this commit.
- [ ] **Production call site:** the real binary, as a subprocess — `selene --help` lists
  **20 visible** subcommands (22 − 2 hidden); every `selene <name>` reaches its arm.
- [ ] Commit: `feat(cli): the 22-subcommand clap surface, the exit-code contract, and the dispatch`

### Task 3: `selene-cli` — project resolution, the unsafe-root refusal, the fatal handler, the terminal UI

**Files:** Create: `crates/selene-cli/src/{paths.rs, fatal.rs, term.rs}`,
`crates/selene-cli/tests/paths_test.rs`. Modify: `src/lib.rs` (three `mod` lines).

**Interfaces:**
```rust
// paths.rs — ONE walk-up function with a flag. (Map §Rust port notes, quirk #6: the TS has TWO —
// `resolveProjectPath` and `findNearestCodeGraphRoot` — differing only in a DB check. Port one.)
pub fn find_nearest_selene_root(start: &Path, require_db: bool) -> Option<PathBuf>;
pub fn resolve_project_path(arg: Option<&Path>) -> PathBuf;  // resolve(arg||cwd); if initialized use
                                                             // it; else walk to FS root; if not
                                                             // found → return the ORIGINAL path
pub fn is_initialized(dir: &Path) -> bool;   // `.selene/` EXISTS **and** the DB dir exists
pub fn unsafe_index_root_reason(p: &Path) -> Option<String>;  // FS root / $HOME / parent-of-$HOME
```

- [ ] **`resolve_project_path` returns the ORIGINAL path when the walk finds nothing** — it does
  not error. The *command* then fails with "not initialized" (exit 1). That indirection is the
  map's, and it is what makes every "not initialized" message name the path the **user typed**
  rather than `/`.
- [ ] **`is_initialized` requires BOTH** `.selene/` **and** the database directory (import
  `selene_db::DATABASE_DIRNAME` — `"graph.db"`; do not retype the string). A bare `.selene/` with
  no DB is **not** initialized: that is a half-deleted index, and treating it as initialized is
  how every query-class command starts returning *empty* instead of *guidance*.
- [ ] **`unsafe_index_root_reason`** refuses the filesystem root, `$HOME` itself, and any **parent
  of** `$HOME`. `init` refuses these without `--force` → exit **1**. Test all three plus the
  negative (a normal project dir → `None`).
- [ ] **The fatal handler** (map §`bin/fatal-handler.ts`): a `std::panic::set_hook` printing **ONE
  bounded line** to stderr — `[Selene] Uncaught exception: <payload>` — then exit **1**. It
  **never** prints a backtrace (the TS never touches `.stack`, #850: a 200-line stack dump in an
  agent's tool output poisons the context window). Truncate the payload at a fixed width.
  ⚠ **`prompt-hook` installs a DIFFERENT hook that exits 0** (Task 19) — "never break the prompt"
  outranks "report the fault".
- [ ] **`term.rs`**: `std::io::IsTerminal` for TTY checks (no dep); `indicatif` for the index
  progress bar with the **verbose 5 %-step lines** as the non-TTY fallback (map §`init`); the
  `(y/N)` confirm. **Non-TTY is the default in CI and in an agent's shell — every prompt has a
  non-interactive answer and every progress bar has a non-TTY form.** A prompt that blocks with no
  TTY is a hang, and a hang inside a git hook is a wedged commit.
- [ ] TDD (`paths_test.rs`, real temp trees): a project nested inside a project (nearest wins);
  `.selene/` with no DB (**not** initialized); a start path deep inside a project (walks up); a
  path outside any project (returns the original, unchanged).
- [ ] Commit: `feat(cli): project resolution, unsafe-root refusal, fatal handler, terminal UI`

### Task 4: `selene-cli` — `init` · `uninit` · `unlock` (the lifecycle three)

**Files:** Create: `crates/selene-cli/src/cmd/lifecycle.rs`,
`crates/selene-cli/tests/lifecycle_test.rs`. Modify: `src/cmd/mod.rs` (three arms), `exit_codes.rs`.

- [ ] **`init [path]`** `-i/--index` (**deprecated no-op** — accept, do nothing, do not warn),
  `-f/--force`, `-v/--verbose`. Exact order (map §Subcommands): refuse `unsafe_index_root_reason`
  without `--force` → exit **1**; **already initialized** → warn + `offer_watch_fallback()` → exit
  **0**; else create `.selene/`, run the **supervised full index** (Task 6's function — call it,
  do not duplicate it), `print_index_result`, and if `nodes_created == 0` →
  `offer_index_ignored_repos()` (the interactive gitignored-child-repo opt-in, #1156, which writes
  `include_ignored` into the project config). Any error → exit **1**.
- [ ] **`uninit [path]`** `-f/--force`: confirm `(y/N)` unless forced; delete `.selene/`; call
  `remove_git_sync_hook()` (Task 12 — call it, do not reimplement). **Not initialized → warn, exit
  0** — an expected no-op, not a failure.
- [ ] **`unlock [path]`**: remove the project's stuck-writer lock. **Missing → info, exit 0.**
  ⚠ **See Open Question 3.** The TS removes `.codegraph/codegraph.lock`, an *application*-level
  lock. Selene's equivalent depends on OQ1's ruling (it may be `daemon.pid`, SurrealDB's own
  `LOCK`, or a new app lock). **Do not invent one.** Implement against whatever Task 5's
  `GraphAccess` names as "the lock a stuck writer leaves behind", and say which, in the commit
  message.
- [ ] **Production call site** (subprocess, real temp project): `selene init <tmp>` creates
  `.selene/graph.db`, exits **0**; a **second** `selene init` exits **0** with the
  already-initialized warning; `selene init $HOME` exits **1**; `selene uninit <tmp> -f` removes
  `.selene/`, exits 0; `selene uninit <fresh-tmp>` exits **0**; `selene unlock <tmp>` with no lock
  exits **0**.
- [ ] Commit: `feat(cli): init, uninit, unlock — the lifecycle commands`

### Task 5: `selene-cli` — `GraphAccess`: **the one seam the daemon-vs-direct decision lives behind**

**Files:** Create: `crates/selene-cli/src/access.rs`, `crates/selene-cli/tests/access_test.rs`.
Modify: `src/lib.rs` (one `mod` line).

**⚠ This task exists because of Open Question 1, and its entire purpose is to make that question's
answer a ONE-FILE change.** Read OQ1 before starting. If the maintainer has not ruled yet,
implement the **`Local`** arm, leave `Remote` as a named variant with a failing `todo!`-free stub,
and **say so in the commit message** so the follow-up is visible rather than forgotten.

**The problem, stated once.** The TS daemon is a *warm-cache* optimization: SQLite in WAL mode lets
**many** processes open one DB, which is why the map can say *"CLI commands other than `serve --mcp`
never route through the daemon — they open the DB directly."* **SurrealDB embedded does not work
that way.** `crates/selene-db/src/surreal.rs` takes an exclusive `LOCK` file and
`connect_disk_with_lock_retry` retries only *briefly* before surfacing the error. So in Rust, while
a daemon holds `.selene/graph.db`, a concurrent `selene status` **cannot open it at all**. The
daemon's role inverts: it stops being a cache and becomes the **DB-access arbiter**.

**Interfaces:**
```rust
/// The ONE way any CLI command reaches the graph. Every query-class subcommand goes through it.
pub enum GraphAccess {
    /// No live daemon: open `.selene/graph.db` in-process. The common case, and the fast path.
    Local(QueryManager<SurrealStore>),
    /// A live daemon holds the DB: speak MCP to it over the socket and render its reply.
    Remote(DaemonClient),
}
impl GraphAccess {
    /// Probe the daemon (Task 14's socket candidates + Task 18's `connect_with_hello`); on
    /// no-daemon, open locally. "No daemon" is NEVER an error — it is the normal path.
    pub async fn open(root: &Path) -> Result<Self, Outcome>;
    pub async fn call_tool(&self, tool: &str, args: Value) -> Result<String, Outcome>;
}
```

- [ ] **Every failure here is guidance + an exit code — never a panic, never a raw store error.**
  No `.selene/` → the "not initialized" message + exit **1** (the map's query-class row). A DB held
  by another process **and** no daemon reachable → a *specific* sentence naming the holding pid
  (read it from `daemon.pid`), not a SurrealDB lock exception. ⚠ **The raw error leaking to the
  user is the single most likely bad UX in this phase**: a user who runs `selene status` while
  their editor's MCP daemon is up must get a sentence, not a database exception.
- [ ] **`Local` must stay fast.** No socket-probe cost when no daemon has ever run: `stat`
  `daemon.pid` **first**; only connect if it exists **and** its pid is alive. Missing pidfile ⇒
  straight to `Local`, zero wasted syscalls.
- [ ] **Positive control (the inert-seam rule).** The test that proves `Remote` works must actually
  **stand a daemon up** and get a **real answer** through it — not merely assert that
  `GraphAccess::open` returned the `Remote` variant. A `Remote` arm that constructs and never
  round-trips is this project's signature bug.
- [ ] TDD: no daemon → `Local`, and a query returns real rows; a **live** daemon (spawned by the
  test) → `Remote`, and the same query returns the **same rows**; a **stale** `daemon.pid` (dead
  pid) → `Local`, and the stale pidfile is cleared (Task 14's `clear_stale_daemon_lock`).
- [ ] Commit: `feat(cli): GraphAccess — the single seam between direct DB access and the daemon`

### Task 6: `selene-cli` — `index` · `sync` + command supervision

**Files:** Create: `crates/selene-cli/src/cmd/index.rs`,
`crates/selene-cli/tests/index_sync_test.rs`. Modify: `src/cmd/mod.rs` (2 arms), `exit_codes.rs`.

**Interfaces:**
```rust
pub async fn index(path: Option<PathBuf>, force: bool, quiet: bool, verbose: bool) -> Outcome;
pub async fn sync (path: Option<PathBuf>, quiet: bool) -> Outcome;
/// Shared by `init` (Task 4) and `index`. The ONE full-index entry point.
pub async fn run_full_index(root: &Path, ui: &IndexUi) -> Result<IndexResult, CliError>;
```

- [ ] **`index [path]`** `-f`, `-q/--quiet`, `-v/--verbose`: a **full rebuild** — and the map is
  emphatic about *how*: **delete the DB directory and recreate it, never row-DELETE** (#874/#1067).
  In Rust that is `fs::remove_dir_all(.selene/graph.db)` then `SurrealStore::open` + `apply_schema`
  — **not** `GraphStore::clear()`. (`clear()` exists and is the trap: a row-delete leaves the LSM
  fat and the FTS index stale, and it is why the TS bug numbers exist.) `!result.success` → exit
  **1**.
- [ ] **`index` = extract **and** resolve.** The existing `main.rs` body already does both. Keep
  it: an index without resolution is symbols with no flow — the dead-product shape. Print the same
  summary lines.
- [ ] **`sync [path]`** `-q/--quiet` (for git hooks). Change detection: compare each scanned file's
  content hash / mtime against its stored `FileRecord` (`content_hash`, `modified_at` — both
  already on the record), re-index the changed set via `Indexer::index_files(&[...])`, delete
  records for files that vanished (`GraphStore::delete_file` cascades), then **resolve the
  affected refs**. Prints `Synced N changed files` + `Added/Modified/Removed … N nodes in T`. **Not
  initialized → exit 1** (silent under `-q` — a git hook must not spew).
- [ ] **⚠ `sync` is what the git hook and the watcher both call** — it is the hottest path in the
  product and the one most likely to run **concurrently with a daemon holding the DB**. It must
  therefore route through `GraphAccess` (Task 5) like everything else, and on a lock conflict it
  must surface `LockUnavailable` — the watcher's retry path (Task 13) is built on exactly that
  error being *distinguishable* from a real failure. **Two error kinds, not one.**
- [ ] **Command supervision** (map §`bin/command-supervision.ts`, #999): wrap `index` (and `init`'s
  index) in the liveness watchdog (Task 17's `install_command_supervision("index",
  progress_paths: [db_dir])`) so a wedged index self-terminates rather than hanging a user's
  terminal forever — **and so an `index` orphaned by a SIGKILL'd parent dies with it**
  (`index-orphan-watchdog.test.ts`). ⚠ This is a **forward reference to Task 17**: implement
  `index`/`sync` first with the supervision call site present but calling a **no-op** supervisor,
  and have Task 17 fill it. Name the call site here so Task 17 has one to land into.
- [ ] **Production call site** (subprocess): `selene index <tmp>` on a 3-file fixture → exit 0,
  `.selene/graph.db` exists, node count > 0 **and edge count > 0** (the positive control — an index
  that produced zero cross-file edges did not resolve); touch one file, `selene sync <tmp>` → exit
  0 and prints `Synced 1 changed files`; `selene sync <un-indexed>` → exit **1**, and with `-q`,
  **exit 1 with empty stdout**.
- [ ] Commit: `feat(cli): index and sync — full rebuild, incremental change detection, supervision`

### Task 7: `selene-cli` — `status` (text + `--json`)

**Files:** Create: `crates/selene-cli/src/cmd/status.rs`,
`crates/selene-cli/tests/status_test.rs`. Modify: `src/cmd/mod.rs` (1 arm), `exit_codes.rs`.

- [ ] **The `--json` wire shape is a contract** (map §Wire, `status --json`) — port it field for
  field:
  ```json
  {"initialized":true,"version":"…","projectPath":"…","indexPath":"…","lastIndexed":"ISO|null",
   "fileCount":0,"nodeCount":0,"edgeCount":0,"dbSizeBytes":0,"backend":"…","journalMode":"…",
   "nodesByKind":{},"languages":[],"pendingChanges":{"added":0,"modified":0,"removed":0},
   "worktreeMismatch":{"worktreeRoot":"…","indexRoot":"…"}|null,
   "index":{"builtWithVersion":"…","builtWithExtractionVersion":0,"currentExtractionVersion":0,
            "reindexRecommended":false,"state":"complete|partial|indexing|failed|null",
            "pendingRefs":0}}
  ```
  Un-initialized: `{"initialized":false,"version","projectPath","indexPath","lastIndexed":null}` —
  and it still exits **1** (query-class). **camelCase keys** (`#[serde(rename_all = "camelCase")]`)
  — the TS shape is the contract, not Rust's snake_case habit.
- [ ] **`backend` and `journalMode` are SQLite-isms and must be re-stated, not faked.** The map
  prints `Backend: node:sqlite — built-in (full WAL)` and a `Journal (wal green else warning)`
  line. **Selene has neither.** Print the *truth*: `Backend: surrealdb 3.2 (embedded, surrealkv)`
  and drop the journal line, keeping the JSON keys present with the honest values
  (`"backend":"surrealkv","journalMode":null`). ⚠ **Do not print a WAL claim that is false** — and
  do not silently delete the keys either, since a consumer may read them. **See Open Question 4.**
- [ ] **Text output**, in the map's order: `Selene Status`, project, index-state warnings
  (`indexing`/`partial`/`failed`), the **pendingRefs** warning (#1187), Files/Nodes/Edges/DB Size
  (**MB, 2 dp**), Nodes by Kind **desc**, Files by Language **desc**, Pending Changes **or**
  `Index is up to date`, and the re-index hint when `builtWithExtractionVersion` <
  `EXTRACTION_VERSION` (`selene-core` owns the constant — a mismatch is **"re-index recommended",
  never a hard error**, per the roadmap's contract list).
- [ ] **Worktree mismatch is detected from the *start* path vs the *resolved* root** (Task 11's
  `detect_worktree_index_mismatch`) and printed as the multi-line warning. Forward reference:
  land `status` with the call site present; Task 11 fills the function.
- [ ] **Determinism:** `nodesByKind`/`languages` sort **by count desc, then name asc** — a tie
  broken by `HashMap` order makes the output flap between runs and turns a snapshot test into a
  coin flip.
- [ ] **Production call site** (subprocess): `selene status <indexed-tmp>` → exit 0, and
  `selene status <indexed-tmp> --json | jq .nodeCount` > 0; `selene status <un-indexed>` → exit
  **1** with `"initialized": false`.
- [ ] Commit: `feat(cli): status — text and --json`

### Task 8: `selene-cli` — the five query subcommands share **one** formatter: `query` · `callers` · `callees` · `impact` · `files`

**Files:** Create: `crates/selene-cli/src/cmd/query.rs`,
`crates/selene-cli/tests/query_test.rs`. Modify: `src/cmd/mod.rs` (5 arms), `exit_codes.rs`.

They are one task because they are one output problem: **a list of nodes, in text or JSON.** Write
the formatter once (`fn render_nodes(&[NodeRow], json: bool) -> String`) and let all five call it.

- [ ] **`query <search>`** `-p`, `-l/--limit` (default **10**), `-k/--kind`, `-j`: `search_nodes`,
  then **re-sort so generated files sink** (subtract `is_generated_file` as 0/1 —
  `selene-extract`'s `generated.rs` already classifies; call it). Text row:
  `kind.pad_end(12) + name`, then dim `path:start_line`, then the dim signature. **No raw score is
  printed** (#1045) — but JSON **keeps** `score`. Both halves of that are the contract.
- [ ] **`callers <symbol>` / `callees <symbol>`** `-p`, `-l` (default **20**), `-j`: search with
  `limit 50`; keep matches where `name == symbol || name.ends_with(".{symbol}") ||
  name.ends_with("::{symbol}")` — **and the filter applies only when there is more than one match**
  (map §Rust port notes, quirk #4: "intentional but surprising"). **Port the quirk and pin it with
  a test that cites this line**, or the next reader "fixes" it and silently changes which symbol a
  single fuzzy hit resolves to. Union `get_callers`/`get_callees` **deduped by node id**; if empty,
  fall back to the top match; slice to `limit`. JSON:
  `{symbol, callers|callees: [{name, kind, filePath, startLine}]}`.
- [ ] **`impact <symbol>`** `-d/--depth` default **2**, clamped **[1, 10]**, `-j`: the same
  exact-match merge over `get_impact_radius`. JSON `{symbol, depth, nodeCount, edgeCount,
  affected: […]}`; text grouped by file. ⚠ **The TS has a bug here** (map quirk #3: the fallback
  branch overwrites `edgeCount` with the *unmerged* count). **Fix it, and write a comment saying
  the TS did not** — this one is a genuine bug, not a load-bearing quirk, and it produces a number
  that is simply wrong. (Contrast with the `callers` quirk above, which changes *which symbol* is
  resolved and is therefore behavior to preserve.)
- [ ] **`files`** `-p`, `--filter <dir>`, `--pattern <glob>`, `--format tree|flat|grouped` (default
  **tree**), `--max-depth`, `--no-metadata`, `-j`. Tree sorts **dirs first, then by name**. The
  `--filter` uses the #426 path normalization (`/`, `.`, `./`, and a backslash path all behave) —
  `selene-graph`'s `QueryManager` already owns that normalizer; call it.
- [ ] **⚠ The two glob dialects — DECIDE AND DOCUMENT** (map quirk #2, which explicitly says
  "unify deliberately and document"): `files --pattern` compiles `**`→`.*`, `*`→`[^/]*`, `?`→`[^/]`
  and is **not anchored** (a substring match); `affected --filter` (Task 10) escapes a *different*
  character class and compiles `**`→`.+`. **They disagree today.** This task owns
  `glob_to_regex()`; Task 10 **must reuse it**. See **Open Question 5** for the unification the
  maintainer must pick — until then, implement `files --pattern`'s dialect and leave the divergence
  documented in one comment naming both call sites.
- [ ] **Misses are success-shaped in TEXT but exit 1 on a query-class miss?** — **No.** Read the
  table: exit **1** is for *not initialized*, not for *no results*. `No results found for "{q}"`
  prints and exits **0**. Pin this: a `grep`-style caller expects 0-with-no-rows, and an agent
  expects guidance. Only "not initialized" is 1.
- [ ] **Production call site** (subprocess, indexed fixture): each of the five prints rows and
  exits 0; each with `-j` emits **parseable JSON** with the asserted keys; `impact -d 0` clamps to
  1 and `-d 11` clamps to 10; a symbol with **three** definitions returns all three from `callers`.
- [ ] Commit: `feat(cli): query, callers, callees, impact, files — one node formatter, two shapes`

### Task 9: `selene-cli` — `explore` · `node` (the agent-facing pair)

**Files:** Create: `crates/selene-cli/src/cmd/agent.rs`,
`crates/selene-cli/tests/agent_test.rs`. Modify: `src/cmd/mod.rs` (2 arms), `exit_codes.rs`.

These two are **the MCP tools, run from a shell**. They must not grow a second rendering path:
they call the **same** `selene-context` entry points the MCP handlers call, and print
`result.content[0].text`. A second renderer here is a second truth, and it will drift.

- [ ] **`explore <query...>`** `-p`, `--max-files`: run the `selene_explore` tool through
  `GraphAccess` and print the text. `result.is_error` → exit **1**.
- [ ] **`node [name]`** `-p`, `-f/--file`, `--offset`, `--limit`, `--symbols-only`: **neither name
  nor file → usage error, exit 1.** A name containing `/` or `\` **is a file** (normalize `\` →
  `/`); otherwise it is a symbol, fetched with `include_code: true`.
- [ ] **The un-indexed refusal text is a byte-exact contract** (map §Wire) — it is written **to an
  AI agent**, because an agent may be the one running this in a shell. Port it verbatim, modulo the
  Phase-5 rename table:
  > `Selene isn't available here — no .selene/ index exists in <path>. If you are an AI agent:
  > continue with your usual tools; indexing is the user's decision, do not run it yourself. (The
  > project owner can enable Selene with 'selene index'.)`

  and **exit 1** (the map's `explore` row). ⚠ Note the deliberate asymmetry with MCP, which returns
  this **success-shaped**. Both are correct; see the exit-code contract section.
- [ ] **Production call site** (subprocess): `selene explore "how does X work"` on an indexed
  fixture → exit 0, stdout contains the `**Flow` section; on an **un-indexed** dir → exit **1** and
  stdout is the refusal text **byte-for-byte** (assert the bytes, not a substring — this string is
  read by an agent and every word in it was tuned); `selene node` with no args → exit **1**;
  `selene node src/foo.rs` → the file view; `selene node MyFunc` → the symbol view.
- [ ] Commit: `feat(cli): explore and node — the agent-facing pair, with the verbatim refusal`

### Task 10: `selene-cli` — `affected` (BFS over file dependents)

**Files:** Create: `crates/selene-cli/src/cmd/affected.rs`,
`crates/selene-cli/tests/affected_test.rs`. Modify: `src/cmd/mod.rs` (1 arm), `exit_codes.rs`.

- [ ] **Input normalization is a *fixed bug*, ported** (#825): every input → project-relative
  POSIX. Absolute → relative-to-root; `path::normalize`; `\` → `/`; strip a leading `./`. Port
  `cli-affected-paths.test.ts` **whole** — it is the test that exists because this was wrong once.
- [ ] **`--stdin`** reads the file list from stdin (the `git diff --name-only | selene affected
  --stdin` pipeline). **No inputs at all → exit 0** (an expected no-op — a CI hook with an empty
  diff must not fail the build).
- [ ] **The default test patterns** (regex, verbatim): `/\.spec\./`, `/\.test\./`, `/\/__tests__\//`,
  `/\/tests?\//`, `/\/e2e\//`, `/\/spec\//`.
- [ ] **The BFS** over `get_file_dependents`, `-d/--depth` default **5**: **test files are
  terminal** (a test that imports X is a leaf — you do not chase what the test's own imports pull
  in); non-test files are enqueued until `depth >= max`. That asymmetry is the whole algorithm; a
  BFS that treats tests as normal nodes returns the entire repo.
- [ ] **`-f/--filter <glob>`** — ⚠ **reuse Task 8's `glob_to_regex()`.** The TS built a *second*
  dialect here (escape `[+[\]{}()^$|\\]`, `.`→`\.`, `**`→`.+`, `*`→`[^/]*`). See **Open Question
  5**. Do not copy-paste a second compiler into this file; call Task 8's and note the divergence in
  one comment if the maintainer has not yet ruled.
- [ ] **`-q`** prints **bare sorted paths** (that is the pipeline form: `selene affected -q | xargs
  npm test`), `-j` the JSON shape.
- [ ] **Production call site** (subprocess): `echo src/a.rs | selene affected --stdin -q` on a
  fixture where `b.rs` imports `a.rs` prints `src/b.rs`; with **no** inputs → exit **0** and empty
  output; an absolute input path resolves the same as its relative form (the #825 assertion).
- [ ] Commit: `feat(cli): affected — dependent BFS with test-terminal semantics`

### Task 11: `selene-sync` — the crate, the bounded `git` runner, the watch policy, worktree detection

**Files:** Create: `crates/selene-sync/src/{policy.rs, worktree.rs, git.rs}`; rewrite
`crates/selene-sync/src/lib.rs` + `Cargo.toml`; create
`crates/selene-sync/tests/{policy_test.rs, worktree_test.rs}`.

Three pure-ish functions, no async, no watcher yet — the cheap half of the crate, and everything
else in it depends on the `git` runner.

**Interfaces:**
```rust
// git.rs — EVERY git call in this crate is 5000 ms-bounded (#1139). One runner, no exceptions.
pub(crate) const GIT_TIMEOUT_MS: u64 = 5_000;
pub(crate) fn git(root: &Path, args: &[&str]) -> Option<String>;   // None on timeout/failure
pub fn is_git_repo(root: &Path) -> bool;
// policy.rs
pub fn watch_disabled_reason(root: &Path, probe: Option<&Probe>) -> Option<String>;
pub fn detect_wsl() -> bool;                                        // CACHED
// worktree.rs
pub fn git_worktree_root(dir: &Path) -> Option<PathBuf>;
pub fn detect_worktree_index_mismatch(start: &Path, index_root: &Path) -> Option<Mismatch>;
pub fn worktree_mismatch_warning(m: &Mismatch) -> String;   // `status`'s multi-line form
pub fn worktree_mismatch_notice(m: &Mismatch) -> String;    // the read tools' one-line `⚠ …`
```

- [ ] **The 5 s bound is not optional.** `std::process::Command` has no timeout; use the pinned
  `wait-timeout` crate (already in the workspace for `selene-extract`'s git fast path). A `git`
  call that hangs (a network-backed `core.hooksPath`, a wedged index.lock) would otherwise hang the
  **watcher**, which would hang the **daemon**, which would hang **every attached agent**. That
  causal chain is why #1139 exists.
- [ ] **`watch_disabled_reason` — precedence order is behavior** (map §FileWatcher): (1)
  `SELENE_NO_WATCH=1` → off; (2) `SELENE_FORCE_WATCH=1` → **on** (it overrides the WSL rule — that
  is the escape hatch); (3) **WSL + a `/mnt/…` root** → off (#199). WSL detection: the
  `WSL_DISTRO_NAME` / `WSL_INTEROP` env vars, **or** `/proc/version` containing `microsoft`/`wsl`,
  **cached** (do not stat `/proc/version` per event). The root regex is
  `^/mnt/[a-z](/|$)` — case-insensitive. ⚠ Port `watch-policy.test.ts`'s pin that **`/mnt/wsl` is
  NOT flagged** (it is the Linux-side mount, where watching works fine); the naive prefix check
  gets it wrong.
- [ ] **`detect_worktree_index_mismatch` returns `None` in four cases** (map §Worktree mismatch —
  all four, or the warning fires on every submodule): start path not in git; the worktree root
  **equals** `realpath(index_root)`; the index root is not itself a worktree root; **or** the two
  trees have **different `gitCommonDir`s** (a nested repo/submodule is already covered by the
  parent index — #1031/#1033). Otherwise `Some(Mismatch{worktree_root, index_root})`.
- [ ] TDD, on **real git repos** built in temp dirs (`git init` + `git worktree add` — no mocks;
  this function's entire job is reading real git state): the mismatch matrix incl. the
  submodule/gitlink suppression, and the two warning strings pinned byte-for-byte.
- [ ] Commit: `feat(sync): the bounded git runner, the watch policy, worktree-mismatch detection`

### Task 12: `selene-sync` — git hooks (byte-exact markers, idempotent install/remove)

**Files:** Create: `crates/selene-sync/src/hooks.rs`, `crates/selene-sync/tests/hooks_test.rs`.
Modify: `src/lib.rs` (one `mod` line).

**Interfaces:**
```rust
pub enum GitHookName { PostCommit, PostMerge, PostCheckout }
pub const DEFAULT_SYNC_HOOKS: [GitHookName; 3] = [PostCommit, PostMerge, PostCheckout];
pub struct GitHookResult { pub installed: Vec<String>, pub hooks_dir: PathBuf,
                           pub skipped: Vec<String> }
pub fn install_git_sync_hook(root: &Path, hooks: Option<&[GitHookName]>) -> GitHookResult;
pub fn remove_git_sync_hook (root: &Path, hooks: Option<&[GitHookName]>) -> GitHookResult;
pub fn is_sync_hook_installed(root: &Path, hooks: Option<&[GitHookName]>) -> bool;
```

- [ ] **The marker block is byte-exact** (map §Wire) — these bytes are the *detection* mechanism, so
  a single changed space orphans every previously-installed hook:
  ```sh
  # >>> selene sync hook >>>
  if command -v selene >/dev/null 2>&1; then
    ( selene sync >/dev/null 2>&1 & ) >/dev/null 2>&1
  fi
  # <<< selene sync hook <<<
  ```
  Note the **backgrounded, fully-silenced** invocation: a commit must never wait on an index, and a
  hook that writes to stdout corrupts porcelain output. (The comment lines from the TS body are
  ported too — only the `codegraph`→`selene` rename applies.)
- [ ] **The hooks dir comes from `git rev-parse --git-path hooks`** (cwd = root, via Task 11's
  bounded runner) — **not** from `.git/hooks`. That is what honors `core.hooksPath` and worktrees,
  and it is the difference between "works" and "silently installs into a directory git never
  reads".
- [ ] **Install semantics**: an existing file → strip any **old** marker block first, trim trailing
  whitespace, append `\n\n<block>\n` (or write a fresh `#!/bin/sh\n<block>\n` if the file is
  effectively empty); a new file → `#!/bin/sh\n<block>\n`; `chmod 0755` **best-effort** (a failure
  to chmod is not a failure to install). **Idempotent**: installing twice yields **one** block.
- [ ] **Remove semantics**: touch **only** files containing `MARKER_BEGIN`; strip the block; delete
  the file if only a shebang/blank remains, else rewrite + chmod. **A user's own hook lines survive
  both operations** — that is the whole point of the marker delimiters, and it is the assertion
  most worth writing.
- [ ] TDD (`git-hooks.test.ts`, ported whole): install → idempotent re-install → **preserve a
  user's existing hook body** → remove → shared-file (our block + theirs in one file) →
  `core.hooksPath` honored → **non-repo → skipped, not an error**.
- [ ] Commit: `feat(sync): git sync hooks — byte-exact markers, idempotent install and removal`

### Task 13: `selene-sync` — `FileWatcher` (debounce, pending files, the degrade taxonomy)

**Files:** Create: `crates/selene-sync/src/watcher.rs`,
`crates/selene-sync/tests/watcher_test.rs`. Modify: `src/lib.rs` (one `mod` line).

**The most stateful object in the phase.** Every constant below is the map's; none is negotiable.

**Interfaces:**
```rust
pub const DEBOUNCE_MS: u64 = 2_000;
pub const MAX_LOCK_RETRIES: u32 = 5;
pub const MAX_SYNC_FAILURE_RETRIES: u32 = 5;
pub const MAX_RETRY_BACKOFF_MS: u64 = 30_000;
pub const MAX_DIR_WATCHES: usize = 50_000;          // env SELENE_MAX_DIR_WATCHES

pub struct PendingFile { pub path: String, pub first_seen_ms: i64, pub last_seen_ms: i64,
                         pub indexing: bool }
pub struct WatchOptions { pub debounce_ms: u64, pub on_sync_complete: Option<…>,
                          pub on_sync_error: Option<…>, pub on_degraded: Option<…>,
                          pub inert_for_tests: bool }
/// `sync_fn` returning `Err(SyncError::LockUnavailable)` takes the QUIET retry path — it is a
/// DIFFERENT budget from a real failure, and conflating them is how a busy index degrades a
/// healthy watcher. (Task 6 must return two distinct error kinds for this to work.)
pub struct FileWatcher { … }
impl FileWatcher {
    pub fn new(root: PathBuf, sync_fn: SyncFn, opts: WatchOptions) -> Self;
    pub fn start(&self) -> bool;      // false when `watch_disabled_reason` is Some
    pub fn stop(&self);
    pub fn is_active(&self) -> bool;
    pub fn is_degraded(&self) -> bool;
    pub fn degraded_reason(&self) -> Option<String>;
    pub fn pending_files(&self) -> Vec<PendingFile>;    // ⚠ Task 20 feeds the MCP banners from this
    pub async fn wait_until_ready(&self, timeout_ms: u64) -> bool;   // default 10_000
}
```

- [ ] **The event filter, in order** (map §FileWatcher event path): normalize the rel path to POSIX;
  **drop** if empty / `.` / `..`-prefixed; **drop** Selene's own data dirs (`.selene`, the
  `SELENE_DIR` override, `.selene-*` siblings) and `.git`; **drop** via the **indexer's own
  `ScopeIgnore`** (defaults + `.gitignore` — #276/#407/#514: `selene-extract` already owns this
  matcher; **call it, do not build a second one** — two ignore matchers that disagree is how a file
  gets indexed but never re-synced, or vice versa); **drop** non-source extensions. Survivors →
  `pending_files[rel] = {first_seen_ms, last_seen_ms}` + arm the debounce.
- [ ] **`flush()` — the false-stale-over-false-fresh rule, which is the subtle one.** Skip if a sync
  is already running. Record `sync_started_ms`; run `sync_fn`. **On success**, clear the retry
  counters and delete **only** the pending entries whose `last_seen_ms <= sync_started_ms` — a file
  edited *during* the sync **stays pending**. The bias is deliberate: a false-stale file gets
  re-synced (cheap); a false-fresh file is **silently wrong in the graph forever** (expensive).
- [ ] **Two retry budgets, and they are separate:** `LockUnavailable` → `lock_retry_count += 1`,
  **quietly** (no `on_sync_error` — a lock conflict is normal when the user runs `selene index` by
  hand), degrade after **> 5**. Any other error → `sync_failure_retry_count += 1`, fire
  `on_sync_error`, degrade after **> 5** (#1127). **Backoff**, when pending files remain:
  `min(debounce_ms * 2^(max(retry_streak) − 1), 30_000)`; otherwise the normal debounce.
- [ ] **The degrade taxonomy — fatal vs non-fatal, and they are NOT the same** (this is the bullet a
  rushed port collapses): **EMFILE/ENFILE** (fd exhaustion) ⇒ `degrade(EXHAUSTION_REASON)` —
  **permanent** until the next `start()`, fires `on_degraded` **once**, and **stops the watcher**.
  **ENOSPC** (the Linux inotify watch budget) ⇒ **warn once, stop adding new watches, keep
  running** — NON-fatal. Collapsing ENOSPC into the fatal arm silently disables auto-sync on any
  large Linux repo. Per Task 1's finding, map `notify`'s error kinds onto this taxonomy explicitly.
- [ ] **`pending_files()` computes `indexing = syncing && sync_started_ms >= last_seen_ms`** — the
  flag the MCP staleness banner renders as `(indexing in progress)` vs `(pending sync)` (Task 20).
- [ ] **Strategy**: `notify`'s `RecommendedWatcher` + `RecursiveMode::Recursive` (per Task 1: one
  watch on macOS/Windows; inotify still costs one per directory, hence the `MAX_DIR_WATCHES` cap +
  the warn-once).
- [ ] TDD (`watcher.test.ts`, ported): lifecycle **idempotency** (double `start()`/`stop()`);
  **EMFILE degrade-once vs ENOSPC warn-once**; lock-contention degradation after exactly 5;
  persistent-failure degradation after exactly 5; **counters reset on a clean sync**; debounce
  **coalescing** (3 edits in 100 ms ⇒ **one** sync call); the ignore filters (a `.selene/` write and
  a `.gitignore`d file produce **zero** syncs); pending-file semantics incl. the mid-sync-edit rule
  (#403/#449). Use the injectable clock + `inert_for_tests` — **no `sleep(2000)` in the test
  suite.**
- [ ] Commit: `feat(sync): FileWatcher — debounce, pending files, retry budgets, degrade taxonomy`

### Task 14: `selene-mcp` — daemon paths + **lockfile arbitration** (the hard-link lock)

**Files:** Create: `crates/selene-mcp/src/daemon/{mod.rs, paths.rs, lock.rs}`,
`crates/selene-mcp/tests/daemon_paths_test.rs`, `crates/selene-mcp/tests/daemon_lock_test.rs`.
Modify: `src/lib.rs` (one `mod daemon;` line).

**The first of the four concurrency tasks. Nothing here is async and nothing here binds a socket —
it is pure path math and one atomic filesystem primitive, which is exactly why it is separable and
exactly why it must be correct: every race in Tasks 16/18 is arbitrated by this lock.**

**Interfaces:**
```rust
// paths.rs
pub const POSIX_SOCKET_PATH_LIMIT: usize = 100;      // map §Wire — the sun_path ceiling, minus slack
/// POSIX: [`<root>/.selene/daemon.sock`, `<tmpdir>/selene-<hash>.sock`] — and the in-project
/// candidate is DROPPED (tmp-only) when its length exceeds POSIX_SOCKET_PATH_LIMIT (100).
/// win32:  [`\\.\pipe\selene-<hash>`] only.
/// `<hash>` = sha256(realpath(root)).hex[..16]  — sha2 is already pinned.
pub fn daemon_socket_candidates(root: &Path) -> Vec<PathBuf>;
pub fn daemon_socket_path(root: &Path) -> PathBuf;    // candidate 0
pub fn daemon_pid_path(root: &Path) -> PathBuf;       // <root>/.selene/daemon.pid

#[derive(Serialize, Deserialize)]
pub struct DaemonLockInfo { pub pid: u32, pub version: String, pub socket_path: String,
                            pub started_at: i64 }
pub fn encode_lock_info(i: &DaemonLockInfo) -> String;          // pretty JSON + '\n', mode 0600
pub fn decode_lock_info(raw: &str) -> Option<DaemonLockInfo>;   // ALSO accepts a bare decimal pid

// lock.rs
pub enum LockAttempt { Acquired { pid_path: PathBuf, info: DaemonLockInfo },
                       Taken    { pid_path: PathBuf, existing: Option<DaemonLockInfo> } }
pub fn try_acquire_daemon_lock(root: &Path) -> io::Result<LockAttempt>;
pub fn clear_stale_daemon_lock(pid_path: &Path, expected_dead_pid: Option<u32>) -> bool;
pub fn is_process_alive(pid: u32) -> bool;      // kill(pid,0); **EPERM ⇒ ALIVE** (Task 1's dep)
```

- [ ] **The lock is a hard link, and the reason is a race** (map §Lock acquisition). Write the
  **full JSON record** to `<pid_path>.<pid>.tmp` (mode **0600**), then `fs::hard_link(tmp, pid_path)`
  — atomic **and** exclusive, with **no empty-file window**. (The naive `create_new` + `write`
  sequence has one: a second daemon that reads the pidfile between create and write sees an empty
  file, decodes `None`, concludes "stale", and deletes a **live** daemon's lock. That is the bug the
  hard-link dance exists to prevent — say so in a comment.) Always `remove_file(tmp)` afterwards,
  success or failure.
- [ ] **`AlreadyExists` is the ONLY errno that means "taken."** Every other error means *this
  filesystem cannot hard-link* (#997: ENOTSUP/EPERM/EISDIR on ExFAT/network mounts) ⇒ fall back to
  `OpenOptions::new().write(true).create_new(true).mode(0o600)` + write through the fd. ⚠ Getting
  this backwards **silently disables the lock** — every launcher then "acquires" it and you get N
  daemons. Test the fallback by injecting the error kind, and assert the fallback is **still
  exclusive**.
- [ ] **`clear_stale_daemon_lock` is a compare-and-delete, not a delete** — and this is where a
  **recycled PID** kills you. Re-read the pidfile; **bail** (return `false`, delete nothing) if the
  decoded pid **differs** from `expected_dead_pid`, or if that pid is **alive now**. Between the
  "holder is dead" observation and the `unlink`, another launcher may have taken the lock, and the
  OS may have handed its pid to something unrelated. Deleting blind there unlinks a **live**
  daemon's lock and you get two. **Test it: write a lock for pid X, pass `expected_dead_pid = Y` ⇒
  the file survives and the call returns `false`.**
- [ ] **`decode_lock_info` also accepts a bare decimal pid** (a legacy pidfile) → `{pid, version:
  "unknown", socket_path: "", started_at: 0}` (map §Wire). A malformed/empty file → `None`, never a
  panic — a corrupted pidfile is a routine event (a machine that lost power mid-write), not a
  malfunction.
- [ ] **The socket-path length rule**: compute the in-project candidate; **if its length exceeds
  `POSIX_SOCKET_PATH_LIMIT = 100`, drop it and return the tmp candidate only.** Test with a deeply
  nested root that crosses 100 chars — a `bind()` on an over-long `sun_path` fails with a
  bewildering errno, and this is the guard that keeps a deep monorepo working.
- [ ] **`is_process_alive`: EPERM ⇒ alive.** (Per Task 1's dep decision.) A pid owned by another
  user is **alive**, not dead. Reaping on EPERM kills a shared daemon.
- [ ] TDD (`daemon-socket-fallback.test.ts` + the lock half of `daemon-bind-failure.test.ts`,
  ported): candidate **ordering**; the >100 drop; the no-hardlink fallback; **two concurrent
  `try_acquire_daemon_lock` calls (real threads, real FS) ⇒ exactly ONE `Acquired`** — that
  assertion is the whole task; stale-lock compare-and-delete incl. the **pid-mismatch bail** and the
  **pid-recycled** case; the legacy bare-pid decode.
- [ ] Commit: `feat(mcp): daemon socket candidates + hard-link lockfile arbitration`

### Task 15: `selene-mcp` — the daemon registry (`~/.selene/daemons/`)

**Files:** Create: `crates/selene-mcp/src/daemon/registry.rs`,
`crates/selene-mcp/tests/daemon_registry_test.rs`. Modify: `src/daemon/mod.rs` (one line).

**Interfaces:**
```rust
#[derive(Serialize, Deserialize)]
pub struct DaemonRecord { pub root: String, pub pid: u32, pub version: String,
                          pub socket_path: String, pub started_at: i64 }
pub fn register_daemon(rec: &DaemonRecord) -> io::Result<()>;
pub fn deregister_daemon(root: &Path) -> io::Result<()>;
pub fn list_daemons(prune: bool) -> Vec<DaemonRecord>;          // **newest first**
pub enum StopOutcome { Term, Kill, NotRunning, NoDaemon }
pub async fn stop_daemon_at(root: &Path) -> StopOutcome;
pub async fn stop_all_daemons() -> Vec<StopOutcome>;
```

- [ ] **Path**: `~/.selene/daemons/<sha256(resolve(root)).hex[..16]>.json` (via `dirs`), pretty JSON
  + `\n`, mode **0600**. Same 16-hex-char hashing helper as Task 14 — **one function, called
  twice**, not two implementations that could ever disagree about what a root hashes to.
- [ ] **The registry is DISCOVERY ONLY — the live pid is the truth** (map §Wire). It exists so
  `selene daemon` can *find* daemons across projects; it is **never** consulted to decide whether a
  daemon is running (that is `daemon.pid` + `is_process_alive`). A stale record is normal (a
  SIGKILL'd daemon never deregisters). **Readers prune dead records** as they list.
- [ ] **`stop_daemon_at`**: SIGTERM → wait → SIGKILL if it will not go; outcomes are exactly
  `Term | Kill | NotRunning | NoDaemon` (`NotRunning` = a record exists but its pid is dead ⇒ prune
  it; `NoDaemon` = no record at all). Both are **success-shaped** — `selene daemon` with nothing
  running exits **0**.
- [ ] TDD (`daemon-registry.test.ts`): register/list/deregister; **dead-pid pruning**;
  `prune: false` keeps them; **newest-first ordering**; a corrupt JSON record is skipped, not fatal.
- [ ] Commit: `feat(mcp): the daemon registry — discovery records, pruning, stop`

### Task 16: `selene-mcp` — **the Daemon**: bind, hello, refcount, idle timeout, sweep, shutdown + the watchdogs

**Files:** Create: `crates/selene-mcp/src/daemon/server.rs`,
`crates/selene-mcp/src/watchdog/{mod.rs, ppid.rs, liveness.rs}`,
`crates/selene-mcp/tests/daemon_liveness_test.rs`. Modify: `src/lib.rs`, `src/daemon/mod.rs`.

**The heart of the phase.** Every number below is the map's, verbatim.

**Interfaces:**
```rust
// daemon/server.rs
pub const CLIENT_HELLO_TIMEOUT_MS: u64 = 3_000;
pub const MAX_HELLO_LINE_BYTES:   usize = 4_096;
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;     // env SELENE_DAEMON_IDLE_TIMEOUT_MS; 0 = never
pub const DEFAULT_MAX_IDLE_MS:     u64 = 1_800_000;   // env SELENE_DAEMON_MAX_IDLE_MS
pub const CLIENT_SWEEP_MS:         u64 = 30_000;      // env SELENE_DAEMON_CLIENT_SWEEP_MS
pub const WIN32_FORCE_EXIT_MS:     u64 = 2_000;       // finalize_daemon_exit's win32 backstop

#[derive(Serialize)] pub struct DaemonHello { pub selene: String, pub pid: u32,
                                              pub socket_path: String, pub protocol: u8 /* == 1 */ }
#[derive(Deserialize)] pub struct DaemonClientHello { pub selene_client: u8 /* == 1 */,
                                              pub pid: u32, pub host_pid: Option<u32> }
pub struct Daemon { … }
impl Daemon {
    pub fn new(root: PathBuf, idle_timeout_ms: Option<u64>, max_idle_ms: Option<u64>) -> Self;
    pub async fn start(&self) -> Result<(PathBuf, DaemonLockInfo), DaemonError>;
    pub async fn stop(&self, reason: &str);
    pub fn client_count(&self) -> usize;
    pub fn backstop_should_exit(&self, is_alive: &dyn Fn(u32) -> bool) -> bool;
    pub fn reap_dead_clients(&self, is_alive: &dyn Fn(u32) -> bool) -> usize;
}
pub fn parse_client_hello_line(line: &str) -> Option<(u32, Option<u32>)>;
pub fn peer_is_dead(peers: (Option<u32>, Option<u32>), is_alive: &dyn Fn(u32) -> bool) -> bool;
pub fn bind_first_usable_socket(candidates: &[PathBuf]) -> Result<(Listener, PathBuf), io::Error>;
pub fn finalize_daemon_exit(platform: Platform) -> Option<JoinHandle<()>>;

// watchdog/ppid.rs
pub const DEFAULT_PPID_POLL_MS: u64 = 5_000;          // env SELENE_PPID_POLL_MS; 0 disables
pub static EARLY_PPID: LazyLock<u32>;                 // getppid() captured at first touch in main
pub fn supervision_lost_reason(s: &SupervisionState) -> Option<String>;
// watchdog/liveness.rs
pub const DEFAULT_WATCHDOG_TIMEOUT_MS: u64 = 60_000;  // env SELENE_WATCHDOG_TIMEOUT_MS
pub const PROGRESS_CAP_MULTIPLIER: u32 = 10;          // 10 × timeout = the hard cap
pub fn install_main_thread_watchdog(progress_paths: &[PathBuf]) -> Option<WatchdogHandle>;
pub fn install_command_supervision(label: &str, opts: WatchdogOptions) -> SupervisionHandle;
```

- [ ] **Bind, and the ONE errno that must not relocate** (map §Socket bind): walk the candidates;
  before each POSIX bind **unlink a stale socket file**; after bind **`chmod 0600`**.
  `bind_first_usable_socket` **relocates past any errno EXCEPT `EADDRINUSE`** — `EADDRINUSE`
  **propagates**, the caller **releases the lock**, and every launcher falls back to direct mode.
  Rationale, and write it as a comment: `EADDRINUSE` on a freshly-unlinked path means **something
  live is already listening there** — relocating would put a second daemon on a second socket while
  clients keep finding the first, which is a split brain. Every other errno (a read-only `.selene/`,
  a weird FS) is "this path is unusable, try the next".
- [ ] **If the bound path ≠ candidate 0, rewrite the pidfile atomically** (temp
  `${pid_path}.${pid}.relocate` + `rename`) so `socket_path` in the lock record is the **truth**.
  A client that reads a pidfile pointing at a socket nobody is listening on hangs its whole
  handshake budget (240 × 25 ms) and then falls back — a 6-second stall on every cold start.
- [ ] **Then** `register_daemon` (Task 15) and log, verbatim shape:
  `[Selene daemon] Listening on <path> (pid N, vX). Idle timeout Nms.`
- [ ] **Per-connection, and the ordering is load-bearing** (map §Per-connection): write the hello
  line **immediately** on accept —
  `{"selene":"<semver>","pid":N,"socketPath":"…","protocol":1}\n` — *then* read the **optional**
  client-hello for **≤ 3 000 ms** (`CLIENT_HELLO_TIMEOUT_MS`), line-capped at **4 096 bytes**
  (`MAX_HELLO_LINE_BYTES`). A non-hello line or a timeout ⇒ pids are `None` **and the bytes are NOT
  consumed** — they belong to the JSON-RPC stream. ⚠ **This is #662.** The TS did it with
  `socket.pause()` + `unshift()`; **we do it by giving the session the same `BufReader`** (Task 1's
  finding) so the leftover bytes are simply *still in the buffer*. There is no put-back and no
  detach window. **Test: a client that sends `{"jsonrpc":…}\n` as its FIRST line — with no
  client-hello — must have that request served, not swallowed.**
- [ ] **Refcount**: one `MCPSession` per connection; increment on accept, **decrement on transport
  close** — and *only* there, exactly once (a decrement in both an error path and a close path
  double-counts and the idle timer arms while a client is still attached). At **0 clients**, arm the
  idle timer (`SELENE_DAEMON_IDLE_TIMEOUT_MS`, default **300 000** ms; **`0` = never**); **disarm on
  the next accept.**
- [ ] **The inactivity backstop** (`SELENE_DAEMON_MAX_IDLE_MS`, default **1 800 000**; tick every
  `min(max_idle, 60_000)`): reap **only** when *no inbound bytes for the whole window* **AND** *no
  client is provably alive* (#692). Both conditions. A client that is attached and quiet (an editor
  the user has not typed in for 40 minutes) is **alive** and must not be reaped — `backstop_should_exit`
  is a pure function precisely so its **full decision table** can be tested without a socket.
- [ ] **The client sweep** (every **30 000** ms): drop sessions whose **proxy pid** or **host pid**
  is dead (`peer_is_dead`). ⚠ **An unknown pid is NEVER dead** (a client that sent no hello has
  `None` pids — it cannot be swept, only closed). Getting that inverted disconnects every legacy
  client on the first sweep.
- [ ] **Shutdown, in this exact order** (SIGTERM/SIGINT — tokio's `signal` feature is already
  pinned): close sessions → close the listener → close the engine (drop the store, releasing the
  SurrealDB `LOCK`) → remove **our own** pidfile **only if the pid still matches** (compare, do not
  blind-unlink — a takeover may already own it) → `deregister_daemon` → unlink the socket →
  `finalize_daemon_exit` (POSIX: `exit(0)`; win32: set the exit code, drain, and force-exit after
  **2 000** ms).
- [ ] **The PPID watchdog** (poll every `SELENE_PPID_POLL_MS`, default **5 000**; **`0` disables**).
  Reasons, in **precedence order** (map §PPID watchdog): (a) `current_ppid != original_ppid` (POSIX
  reparent) → `"ppid A -> B"`; (b) **win32 only**: `original_ppid > 1 && !is_alive(original_ppid)` →
  `"parent pid N exited"`; (c) `host_ppid.is_some() && !is_alive(host_ppid)` → `"host pid N
  exited"`. The baseline is **always `EARLY_PPID`**. **The liveness fallback is deliberately
  win32-only** — on POSIX a double-forked daemon is legitimately reparented to init and a liveness
  check there produces **false positives that kill healthy daemons**. Pin the precedence with a
  table test; pin that **pid 0 and pid 1 are never valid targets**.
  ⚠ **The detached daemon runs with NO PPID watchdog** (it is detached *on purpose*); the **proxy**
  and **direct mode** run with one.
- [ ] **The liveness watchdog is a THREAD, not a child process** (map §Rust port notes — the
  "separate process" rationale is V8 safepoints and does not apply). A plain OS thread with a
  heartbeat: timeout **60 000** ms (`SELENE_WATCHDOG_TIMEOUT_MS`), heartbeat interval
  `min(2000, max(50, timeout/5))`, disabled by `SELENE_NO_WATCHDOG` (`1/true/yes/on`). On silence ≥
  timeout ⇒ SIGKILL self — **unless** the `progress_paths` fingerprint (size + mtime of the DB dir)
  has **advanced**, in which case defer, up to a hard cap of `PROGRESS_CAP_MULTIPLIER = 10` ×
  timeout (**10 minutes**). Keep the disk-progress deferral: a long SurrealDB statement on slow
  storage is exactly the #1231 hazard, and a watchdog that kills a *working* index is worse than no
  watchdog.
- [ ] TDD (`daemon-client-liveness.test.ts` + `ppid-watchdog.test.ts` + `liveness-watchdog.test.ts`,
  ported): `parse_client_hello_line`'s accept/reject matrix; `peer_is_dead` (**unknown pid never
  dead**; **host-dead reaps even with a live proxy**); `reap_dead_clients`; `backstop_should_exit`'s
  **full decision table**; `supervision_lost_reason`'s precedence + POSIX-vs-win32 divergence + the
  env parsers; the watchdog **kills a wedged loop**, **spares a healthy one**, **spares a slow store
  making disk progress**, honors the **hard cap**, and honors the **opt-out**.
- [ ] Commit: `feat(mcp): the daemon — bind, hello, refcount, idle reaping, watchdogs, shutdown`

### Task 17: `selene-cli` — the `daemon` subcommand (the picker)

**Files:** Create: `crates/selene-cli/src/cmd/daemon.rs`,
`crates/selene-cli/tests/daemon_cmd_test.rs`. Modify: `src/cmd/mod.rs` (one arm), `exit_codes.rs`.

The map's `daemon-manager.ts` is **pure pick logic with the prompt injected** — port that shape, so
the loop is testable with **zero terminal**.

**Interfaces:**
```rust
pub fn format_uptime(ms: i64) -> String;                       // `45s` / `12m` / `3h 5m`
pub enum Pick { Stop(DaemonRecord), StopAll, Cancel }          // sentinels `__stop_all__`/`__cancel__`
pub fn build_picks(records: &[DaemonRecord], cwd_root: Option<&Path>) -> Vec<PickItem>;
pub async fn daemon_cmd(prompt: &dyn Prompt) -> Outcome;       // `Prompt` injected ⇒ testable
```

- [ ] **No daemons → info, exit 0.** An expected no-op. (`selene daemon` on a fresh machine printing
  an error is the kind of thing that makes a user stop trusting the tool.)
- [ ] **Non-TTY → plain lines**, no picker, exit 0: `pid N  vX  up D  root`. This is what a CI job or
  an agent's shell gets, and it must never block waiting for a keypress.
- [ ] **TTY → the picker**: the **current project's** daemon (realpath'd
  `find_nearest_selene_root(cwd, require_db: false)`) **floats first and is pre-selected**; loop
  pick → stop → re-list; **`Stop all` is shown only when > 1** daemon exists; sentinels
  `__stop_all__` / `__cancel__`.
- [ ] **`format_uptime`**: `45s` / `12m` / `3h 5m` — exactly those three forms (test the boundaries:
  59 s, 60 s, 59 m, 60 m).
- [ ] TDD (`daemon-manager.test.ts`, ported): `format_uptime`; **pick ordering** (current project
  first) and the sentinels; the picker **loop** (stop one, re-list, the list shrank); `Stop all`
  hidden at exactly 1 daemon. All driven through a **fake `Prompt`** — no TTY in the test suite.
- [ ] **Production call site** (subprocess): `selene daemon` with none running → exit **0** with the
  info line; with a **real** daemon up (spawned by the test) → the non-TTY listing names its pid.
- [ ] Commit: `feat(cli): the daemon subcommand — listing and the interactive picker`

### Task 18: `selene-mcp` + `selene-cli` — **the proxy, the spawn race, and `serve`'s mode decision**

**Files:** Create: `crates/selene-mcp/src/daemon/proxy.rs`, `crates/selene-cli/src/cmd/serve.rs`,
`crates/selene-mcp/tests/daemon_proxy_test.rs`. Modify: `src/daemon/mod.rs`, `src/cmd/mod.rs`.

**The second-hardest task in the phase, and the one with the real races.** Read the whole task
before writing a line of it.

**Interfaces:**
```rust
pub const TAKEOVER_MAX_RETRIES:        u32 = 5;      // delay 100 ms between attempts
pub const DAEMON_CONNECT_MAX_RETRIES:  u32 = 240;    // × 25 ms ≈ 6 s
pub const DAEMON_CONNECT_RETRY_DELAY_MS: u64 = 25;

pub enum ProxyOutcome { Proxied, FallbackNeeded { reason: String } }
pub async fn connect_with_hello(socket: &Path, expected_version: &str)
    -> Result<Transport, HelloFailure>;               // HelloFailure::VersionMismatch | NoDaemon
pub async fn run_proxy(socket: &Path, expected_version: &str) -> ProxyOutcome;
pub async fn run_local_handshake_proxy(deps: ProxyDeps) -> Result<(), DaemonError>;
pub fn spawn_detached_daemon(root: &Path) -> io::Result<()>;
```

- [ ] **`serve`'s mode decision, in this exact order** (map §Daemon lifecycle & routing):
  1. `SELENE_DAEMON_INTERNAL` truthy → **be** the daemon (the daemon-elect loop below);
  2. `SELENE_NO_DAEMON` truthy (**not** `0`/`false`) → **direct** stdio mode;
  3. no `.selene/` root reachable (realpath'd walk-up) → **direct**;
  4. else → the **local-handshake proxy**.
  Direct mode = one `MCPSession` over stdio + stdin teardown + the startup-handshake backstop + the
  PPID watchdog + the liveness watchdog. ⚠ **`serve --mcp` on a TTY stdin, when not
  `SELENE_DAEMON_INTERNAL`, prints the explanation and exits 0** — a human who runs it by hand gets
  a sentence, not a hung terminal waiting for JSON-RPC.
- [ ] **`serve` without `--mcp`** prints the config snippet + the tool list **to stderr** (not
  stdout — stdout is the JSON-RPC channel and must stay clean even in this mode). See **Open
  Question 6** for what the snippet should say.
- [ ] **The daemon-elect loop, and the takeover race** (map §Daemon-elect): loop **≤
  `TAKEOVER_MAX_RETRIES = 5`**, **100 ms** between attempts: `try_acquire_daemon_lock` →
  - **Acquired** ⇒ `Daemon::start()` + the **liveness** watchdog (**and NO PPID watchdog** — it is
    detached on purpose);
  - **Taken + holder alive** ⇒ a note to stderr, **exit 0** (not an error — the other daemon is
    doing our job);
  - **Taken + holder dead** ⇒ `clear_stale_daemon_lock(pid_path, Some(dead_pid))` (Task 14's
    compare-and-delete) and **retry**;
  - **exhausted** ⇒ **exit 0**. Five failures to acquire means five *other* processes are fighting
    over it and at least one of them will win — flailing longer helps nobody.
  ⚠ **The race this closes:** two launchers both observe a dead holder, both try to clear it. The
  compare-and-delete means the loser's `clear` returns `false` (the pid it expected is gone,
  replaced by the winner's), it **retries**, sees a live holder, and exits 0. **Test it with real
  threads.**
- [ ] **`spawn_detached_daemon`**: spawn `current_exe() serve --mcp --path <root>` with
  **`process_group(0)`** (POSIX, safe + stable — **no `pre_exec`, no `unsafe`**) /
  `CREATE_NEW_PROCESS_GROUP` (win32), stdio → an **append fd on `.selene/daemon.log`** (fallback:
  `Stdio::null()` — a daemon that cannot open its log still starts), env **+**
  `SELENE_DAEMON_INTERNAL=1` and **−`SELENE_HOST_PPID`** (the child must not inherit the *host's*
  pid as its own supervision target), and **do not wait on it**. Then poll the socket **≤ 240 ×
  25 ms (≈ 6 s)**.
- [ ] **⚠ The two-clients-racing-to-spawn race.** Two editors open the same project at the same
  instant; both probe, both find no socket, **both spawn**. That is *fine and expected* — the
  lockfile arbitrates: one child acquires, the other exits 0 immediately. What must **not** happen
  is either **parent** giving up: both parents keep polling the socket, and both connect to the
  **one** survivor. **Test exactly this** (two concurrent `run_proxy` calls against a cold root ⇒
  **one** daemon pid, **two** connected clients, `client_count() == 2`).
- [ ] **`connect_with_hello`**: read the hello line with a **3 s** timeout; compare the version for
  **exact string equality** (map §Rust port notes: *"keep exact, do not compatible-range it"* — a
  daemon from a previous build has different tool schemas, and a "compatible" mismatch is a silent
  behavior fork). **Version mismatch is definitive** ⇒ do **not** retry, do **not** spawn: serve
  **in-process**. After the hello verifies, the proxy sends
  `{"selene_client":1,"pid":<own>,"hostPid":<SELENE_HOST_PPID ?? EARLY_PPID>}\n`.
- [ ] **The local-handshake proxy answers these locally, from a static table** (map §Launcher/proxy)
  — because the handshake must succeed **before** any index is opened (#964/#172): `initialize`
  (static protocol version / server info / **instructions**), `tools/list` (`get_static_tools()`),
  `resources/list` → `{"resources":[]}`, `resources/templates/list` → `{"resourceTemplates":[]}`,
  `prompts/list` → `{"prompts":[]}`. **Everything else is forwarded line-by-line.** The `initialize`
  **is also forwarded** to prime the daemon, **but its reply is suppressed** (the client already got
  ours — two `initialize` results on one id is a protocol violation that hangs the client). Lines
  that arrive **while still connecting are buffered**, not dropped.
- [ ] **⚠ A client dying mid-request, and a daemon dying mid-session.** Track in-flight requests **by
  JSON-RPC id**. If the daemon dies mid-session, the proxy **flips to an in-process engine and
  re-serves the in-flight requests** — the agent sees a slow response, not a dead tool. Anything
  genuinely unserveable gets `{"error":{"code":-32603,"message":"Selene daemon unavailable"}}`. On
  the **daemon** side, a client that dies mid-request must decrement the refcount **exactly once**
  (Task 16) and must not leave its session in the map — otherwise the idle timer never arms and the
  daemon lives forever. ⚠ **Port `proxy-connect.test.ts`'s #974: a socket is NEVER left without an
  error listener** — in Rust terms, **every spawned proxy task's `JoinHandle` is awaited or its error
  is explicitly logged and dropped**; a task that panics into the void takes its connection with it
  and nobody notices.
- [ ] **The startup-handshake backstop** (#1185): a one-shot timer,
  `SELENE_STARTUP_HANDSHAKE_TIMEOUT_MS`, default **900 000** ms (**≤ 0 disables**), **disarmed by the
  first stdin byte**, **armed AFTER the real stdin consumer is installed** (arming it first steals
  the byte that would disarm it), and **NEVER armed in the detached daemon** (which has no stdin).
  Plus **stdin teardown** (#799): stdin `end`/`close`/`error` ⇒ destroy + shut down **once**
  (idempotent — a double shutdown races the session close).
- [ ] TDD (`proxy-connect.test.ts` + the proxy half of `mcp-daemon.test.ts`): version mismatch ⇒
  **direct fallback, no spawn**; the buffered-lines-while-connecting case; the suppressed
  `initialize` reply; **proxy survives daemon death mid-session** (#662) and re-serves the in-flight
  request; the two-racing-launchers case above.
- [ ] Commit: `feat(mcp): the stdio proxy, the daemon-elect race, and serve's mode decision`

### Task 19: `selene-cli` — `prompt-hook` (never break the prompt)

**Files:** Create: `crates/selene-cli/src/cmd/hook.rs`, `crates/selene-cli/tests/hook_test.rs`.
Modify: `src/cmd/mod.rs` (one arm), `exit_codes.rs`.

**The contract, above every other consideration: this NEVER breaks the user's prompt. Every path
exits 0 — including a panic** (it installs its own panic hook that exits **0**, overriding Task 3's;
that inversion is deliberate and must carry a comment saying so). Its only effect is optional
stdout.

- [ ] **Kill-switches, checked first**: `SELENE_NO_PROMPT_HOOK=1` or `SELENE_PROMPT_HOOK=0` ⇒
  return. **TTY stdin** ⇒ return (a human ran it by hand).
- [ ] **Input**: read **all** of stdin, `serde_json` → `{prompt, cwd}`. A parse failure ⇒ **exit 0,
  silent**. Not a warning, not a usage message — **silence**. Anything on stdout is injected into the
  user's prompt.
- [ ] **The tiered gate** (map §prompt-hook), in order:
  1. `has_structural_keyword(prompt)` (the multi-language keyword list) **OR** `extract_code_tokens`
     **OR** `extract_prose_candidates`; none ⇒ `noop-shape`.
  2. `plan_frontload(cwd, prompt)` → the nearest indexed ancestor, or a monorepo sub-project (#964);
     nothing ⇒ `noop-no-index`.
  3. **HIGH** (a structural keyword, or a code token **verified** via
     `get_nodes_by_name(t).len() > 0`): run `selene_explore` with the **raw prompt**; truncate the
     body at **MAX = 16 000** chars, appending
     `\n…(truncated; call selene_explore for the rest)`; wrap in
     `<selene_context note="Structural context from Selene for this prompt — treat returned source as
     already read; …">…</selene_context>`. Outcomes: `high-keyword` / `high-token`; empty or errored
     ⇒ `noop-explore-keyword` / `noop-explore-token`.
  4. **MEDIUM** (prose → symbol-segment matches): heal an empty vocab
     (`heal_segment_vocab_if_empty`; failure ⇒ `noop-vocab-empty`); no matches ⇒ `noop-unverified`;
     else emit the symbol list + a suggested query (the top-3 names) ⇒ `medium-segment`.
     ⚠ **See Open Question 7** — the segment vocabulary is a `selene-context` structure that Phases
     4/5 may not have built. If it does not exist, **implement tiers 1–3 + 5 and make tier 4 a
     no-op that returns `noop-unverified`**, with a comment naming the gap. Do **not** invent a
     vocabulary here.
  5. Multiple sub-projects with no clear match ⇒ the nudge block listing `projectPath:` lines ⇒
     `nudge-projects`.
- [ ] **The telemetry counters are a contract** (`cli_command:prompt-hook-gate-<outcome>`) even
  though telemetry itself is Phase 8. Emit them through a `Telemetry` trait whose Phase-6 impl is a
  **no-op** — and, per the inert-seam rule, **ship a fake impl in the test that asserts the exact
  counter names for each of the outcomes above**. Otherwise Phase 8 wires a real sink into a
  producer nobody ever ran.
- [ ] TDD (`frontload-hook.test.ts`, ported): the multilingual `has_structural_keyword` gate;
  `plan_frontload`'s monorepo resolution; the 16 000-char truncation (**16 001 chars in ⇒ the
  truncation note appears**; 16 000 ⇒ it does not); **every** gate outcome emits its counter name;
  and the headline assertion — **malformed stdin, no stdin, an un-indexed cwd, and a forced panic
  ALL exit 0 with empty stdout.**
- [ ] **Production call site** (subprocess): `echo '{"prompt":"how does X work","cwd":"<indexed>"}' |
  selene prompt-hook` → exit **0**, stdout wrapped in `<selene_context …>`; the same against an
  **un-indexed** cwd → exit **0**, **empty stdout**; `echo 'not json' | selene prompt-hook` → exit
  **0**, empty stdout.
- [ ] Commit: `feat(cli): prompt-hook — the tiered gate that never breaks a prompt`

### Task 20: **Close Phase 5's `NoWatcher` inert seam** — wire the real watcher into the MCP staleness banners

**Files:** Create: `crates/selene-mcp/src/pending.rs`,
`crates/selene-mcp/tests/staleness_wiring_test.rs`. Modify:
`crates/selene-cli/src/cmd/serve.rs` (start the watcher), `crates/selene-mcp/src/server.rs` (inject
the provider), `crates/selene-mcp/src/lib.rs` (one `pub use`).

**This task is the reason Phase 6 is allowed to say it removed an inert seam instead of adding
one.** Phase 5 shipped `PendingFiles` + `NoWatcher` — a provider that returns empty, feeding banner
strings **no user has ever seen**. Its own plan called it "textbook inert-seam shape" and licensed
it **only** because Phase 6 would replace it. This is Phase 6.

**Interfaces:**
```rust
/// The REAL provider. `NoWatcher` stays for `--no-watch` and for tests — it is now a CHOICE,
/// not the only implementation.
pub struct WatcherPending(Arc<FileWatcher>);
impl PendingFiles for WatcherPending {
    fn pending(&self) -> Vec<PendingFile>;          // ← FileWatcher::pending_files()  (Task 13)
    fn degraded_reason(&self) -> Option<String>;    // ← FileWatcher::degraded_reason() (Task 13)
}
```

- [ ] **⚠ DO NOT TOUCH THE BANNER STRINGS.** Phase 5's `banners.rs` owns them and they are
  byte-exact contracts (the staleness banner, the `(indexing in progress|pending sync)` suffixes,
  the max-5 footer + `…and N more`, the degraded banner). This task **feeds them a real provider**
  and changes **nothing** about what they render. If a banner looks wrong, that is a finding to
  report — **not a string to edit here.**
- [ ] **The wiring, and it is the entire point:** `serve` (Task 18) constructs a `FileWatcher`
  (unless `--no-watch` / `SELENE_NO_WATCH`), `start()`s it with `sync_fn = cmd::sync` (Task 6), and
  injects `WatcherPending(watcher)` into `SeleneMcp`. **In the daemon, the watcher lives in the
  daemon** — it is the process that holds the DB (per OQ1) and therefore the only one that can write
  the re-index. **A watcher in the proxy would be a second writer racing the daemon.** Say this in a
  comment; it is the single easiest mistake to make in this task.
- [ ] **`start()` returning `false` is normal, not an error** — `watch_disabled_reason` said no
  (WSL2 `/mnt`, `SELENE_NO_WATCH`). The server then uses `NoWatcher` and **the degraded banner
  carries the reason**. That path is exactly what the degraded banner was written for, and it is now
  reachable for the first time.
- [ ] **The proof this seam is closed** — and it must be **end-to-end**, because an in-process
  assertion is what let the seam ship in the first place: an integration test **spawns the real
  binary** in `serve --mcp` mode over an indexed fixture, **edits a file on disk**, waits for the
  watcher to mark it pending, then calls `selene_explore` **over the real MCP transport** and asserts
  the response body **contains the staleness banner naming that file**. ⚠ **A test that constructs
  `WatcherPending` and calls `.pending()` proves nothing** — that is precisely the shape of the four
  seams this project has already paid for. **Drive the binary.**
- [ ] **Positive + negative control**: with **no** edits, the banner is **absent** (a banner on every
  response is noise that trains the agent to ignore it); with an edit, it is **present**. Both
  assertions, same test.
- [ ] Commit: `feat(mcp): wire the real FileWatcher into the staleness banners — closes the NoWatcher seam`

### Task 21: `selene-cli` — `version` · `install` · `uninstall` · `telemetry` · `upgrade` + the ledger pass

**Files:** Create: `crates/selene-cli/src/cmd/misc.rs`. Modify: `src/cmd/mod.rs` (5 arms),
`crates/selene-cli/src/lib.rs` (ledger), `crates/selene-sync/src/lib.rs` (ledger),
`crates/selene-mcp/src/lib.rs` (ledger), `exit_codes.rs`.

The last five arms, and the crate ledgers. **Four of these five are delegations to a later phase —
and the delegation *call site* is the deliverable**, so Phase 7/8 fill a function that a live
dispatch arm already calls.

- [ ] **`version`** — prints the package version. Fully implemented here. With `-v` / `-version` /
  `--version` / `-V` / the `version` subcommand **all printing exactly the same string**
  (`cli-version.test.ts` — port it whole; it is five call paths to one constant, and they have drifted
  before).
- [ ] **`install`** `-t/--target`, `-l/--location global|local`, `-y/--yes`, `--no-permissions`,
  `--print-config <id>`, `--refresh`: **validate the flags here** — an invalid `--location` → exit
  **1** — and map `--no-permissions` exactly: explicit `false` ⇒ `auto_allow = false`; `--yes` ⇒
  `true`; else ⇒ `None` (prompt). Then **call `selene_installer::install(opts)`**, which Phase 7
  implements. Until it does, that function returns a `NotImplemented` outcome that prints one line
  and exits 1. **See Open Question 2.**
- [ ] **`uninstall`** `-t`, `-l`, `-y`, `--keep-cli` — same shape, same delegation.
- [ ] **`telemetry [status|on|off]`** — **an unknown action → exit 1** (that row is in the table).
  The body delegates to Phase 8. The env-override warning (`DO_NOT_TRACK` / `SELENE_TELEMETRY`
  overriding the saved choice) is Phase 8's.
- [ ] **`upgrade [version]`** `--check`, `-f/--force` — **exits with the code `run_upgrade` returns**
  (its own code; not 0/1). Phase 8's body.
- [ ] **The ledger pass on all three crate `lib.rs` files** — role + PRD section, the
  **public-interface ledger** (map §Public interface → the Rust item, **or** "deferred → Phase N,
  because …"), the invariants, and the **exit-code contract** restated where a maintainer will
  actually read it. Every deferral above lands here **by name**: the installer bodies (Phase 7);
  telemetry + upgrade (Phase 8); the Windows arms (post-v1, `cfg`-gated, untested in CI); the
  prompt-hook's MEDIUM tier if OQ7 left it a no-op.
- [ ] Commit: `feat(cli): version, installer/telemetry/upgrade delegations, and the crate ledgers`

### Task 22: **PHASE 6 GATE** — daemon lifecycle: **spawn / reuse / idle-exit / takeover**, over a real socket

**Files:** Create: `crates/selene-mcp/tests/daemon_lifecycle_gate.rs`,
`docs/benchmarks/2026-07-phase6-daemon.md`. Requires **every** prior task.

**This gate drives the REAL BINARY over a REAL SOCKET. Not `Daemon::start()` in-process; not a
mocked transport; not a fake `is_alive`.** A daemon whose unit tests are green but which cannot
actually be *spawned, reused, and taken over* is the exact failure class this project has shipped
four times — and it is a class that **only** an end-to-end test can catch, because every one of its
failure modes lives in the gap between the library and the process.

Every case below: `std::process::Command` → the built `selene` binary → speak **real MCP over the
child's stdio** → assert on **real bytes**, **real pids**, and the **real filesystem**.

- [ ] **SPAWN.** From a cold indexed root (no `daemon.pid`, no socket), run one
  `selene serve --mcp --path <root>`. Assert: `initialize` **succeeds**; `.selene/daemon.sock`
  **exists** and is mode **0600**; `.selene/daemon.pid` decodes to a **live** pid; a registry record
  exists at `~/.selene/daemons/<hash>.json`; and a `selene_explore` call **returns real content**
  (the positive control — a daemon that handshakes but answers nothing is a dead product).
- [ ] **REUSE — two launchers, ONE daemon.** Start a **second** `serve --mcp` against the same root.
  Assert: **no second daemon process** (the daemon pid is unchanged), the second client is
  **served by the same daemon**, and `client_count() == 2`. Then start them **concurrently** (both
  launched within the same millisecond, cold root, both spawning) and assert **exactly one** daemon
  pid exists and **both** clients get answers. That is the two-clients-racing-to-spawn race from
  Task 18, and it is the one a lockfile bug shows up in.
- [ ] **A CLIENT DIES MID-REQUEST.** Kill client 1 with **SIGKILL** while a `selene_explore` call is
  in flight. Assert: the daemon **survives**; client 2's session is **unaffected** and still answers;
  the refcount drops to exactly **1** (not 2, not 0 — a double-decrement arms the idle timer under a
  live client, and a missed decrement means the daemon never exits).
- [ ] **IDLE-EXIT.** With `SELENE_DAEMON_IDLE_TIMEOUT_MS` set **small** (e.g. `1500`; the production
  default is **300 000** and a gate must not wait five minutes — assert the *default* is 300 000 in a
  unit test, and drive the *mechanism* here with the env override), disconnect the **last** client.
  Assert: the daemon exits within the window; **the socket file is gone**; **the pidfile is gone**;
  **the registry record is gone**. Then the negative control that makes the assertion mean something:
  with a client still attached and **quiet** (no traffic for well past the window), the daemon is
  **still alive** — a reaper that cannot tell "idle" from "quiet" disconnects a user's editor
  mid-session (#692).
- [ ] **TAKEOVER — the stale lock, including the recycled pid.** SIGKILL the daemon (so it **cannot**
  clean up: the pidfile and socket file are **left behind**). Start a new `serve --mcp`. Assert: it
  detects the dead holder, **clears the stale lock** (compare-and-delete), **takes over**, binds, and
  serves — within the **5 × 100 ms** takeover budget. Then the hard half: write a `daemon.pid` whose
  pid is **alive but is not a daemon** (a sleeping process the test owns — a *recycled* pid). Assert
  the launcher does **NOT** delete that lock (compare-and-delete bails), does **not** kill that
  process, and falls through to **direct mode** rather than spawning a second daemon on a socket
  someone else may own.
- [ ] **`SELENE_NO_DAEMON=1` isolates.** With it set, `serve --mcp` runs **direct**: no socket file,
  no pidfile, no registry record — and it still answers `selene_explore`. (The escape hatch must
  actually escape; it is what a user reaches for when the daemon misbehaves.)
- [ ] **VERSION MISMATCH → direct fallback.** Hand-write a `daemon.pid` + stand a fake listener that
  emits a hello with a **different version**. Assert the launcher does **not** spawn, does **not**
  proxy, and serves **in-process** — and that it does so **immediately** (a version mismatch is
  definitive; burning the 6-second connect budget on it is a bug).
- [ ] **PROXY SURVIVES DAEMON DEATH MID-SESSION (#662).** With a client attached through the proxy,
  SIGKILL the daemon **while a request is in flight**. Assert the client gets a **real answer** (the
  proxy flipped to the in-process engine and re-served the in-flight request by id) — **not** a
  `-32603`, and **not** a hang.
- [ ] **⚠ CI runs the POSIX paths.** The win32 arms are `cfg`-gated and validated post-v1 (roadmap
  §Risks). **Say so in the benchmark doc** — a gate that silently skips half its platforms while
  reporting green is worse than one that reports 8/10.
- [ ] **⚠ NO SLEEPS AS SYNCHRONIZATION.** Poll for the condition with a deadline
  (`wait_for(|| socket.exists(), 6_000)`), never `sleep(500)` and hope. A timing-flaky gate gets
  `#[ignore]`d within a month and then this whole phase is unverified — which is *exactly* how a
  daemon regression ships.
- [ ] **Record the results** in `docs/benchmarks/2026-07-phase6-daemon.md`: per case — pass/fail,
  wall-clock, the **measured** cold-start time (spawn → first `initialize` response) and the **warm**
  reuse time (connect → first response). Cold-start-vs-warm is the number that says whether the
  daemon is *earning its complexity*: if warm ≈ cold, the daemon is pure risk with no payoff and the
  maintainer should know that before v1. **Record it even if it is unflattering** — that is what the
  benchmark doc is for.
- [ ] Commit: `test(mcp): PHASE 6 GATE — daemon lifecycle over the real binary and a real socket`

---

## Definition of done

- [ ] Tasks 1–22 committed; `cargo fmt && cargo clippy --all-targets && cargo test` green.
- [ ] **All 22 subcommands** are reachable from the real binary's dispatch, with the map's flags and
      the **exit-code table** pinned by `tests/exit_codes.rs` — driven as a **subprocess**, not
      in-process.
- [ ] **THE GATE** (Task 22): spawn / reuse / idle-exit / takeover **green over a real socket**, plus
      the client-death, version-mismatch, `NO_DAEMON`, and #662 cases; results in
      `docs/benchmarks/2026-07-phase6-daemon.md` **including the cold-vs-warm number**.
- [ ] **Phase 5's `NoWatcher` inert seam is CLOSED** (Task 20), proven by an **end-to-end** test that
      edits a file on disk and sees the staleness banner come back over MCP.
- [ ] The three crate `lib.rs` ledgers name every deferred item **with its phase and its reason**.
- [ ] `docs/plans/2026-07-12-selenecode-roadmap.md`'s Phase 6 row updated to reflect reality.
- [ ] **Every Open Question below is adjudicated** — none was silently invented by a task.

---

## Open questions (for the maintainer — I did NOT invent answers to these)

**OQ1 — ⚠ THE ARCHITECTURAL ONE. SurrealDB embedded takes an EXCLUSIVE lock, so the daemon's whole
reason for existing changes. What routes through it?**
The map says: *"CLI commands other than `serve --mcp` never route through the daemon — they open the
DB directly."* **That is a SQLite fact, not a portable one.** SQLite in WAL mode admits many
processes. `crates/selene-db/src/surreal.rs` (`connect_disk_with_lock_retry`, and its own comment)
takes an exclusive **`LOCK` file** and retries only briefly. So **while a daemon is running,
`selene status` / `query` / `explore` / `sync` cannot open the DB at all.** In TS the daemon was a
*warm-cache optimization*; in Rust it becomes the **DB-access arbiter** — a different thing, with
different scope. The options:
  - **(a) Daemon-as-arbiter (my recommendation).** Every graph-touching command goes through
    `GraphAccess` (Task 5): probe the socket; if a daemon is live, **speak MCP to it**; else open the
    DB directly. `index`/`sync` route through it too (the daemon does the write). Exactly one process
    ever holds the DB. **Cost:** the daemon must expose the write path, and `GraphAccess::Remote`
    must render tool output for the CLI. **Benefit:** `selene status` works while an editor is
    attached, which is table stakes.
  - **(b) No daemon in v1.** Direct stdio mode only; two MCP clients on one project = the second one
    fails to open the DB. Cheap, and **wrong** for anyone running two editors — but it is honest, and
    Phase 6 shrinks by ~5 tasks.
  - **(c) Daemon holds the DB; CLI commands fail loudly while it is up** ("a daemon is running; stop
    it or use the MCP tools"). Least code, worst UX.
**This plan is written for (a)** and is structured so the answer changes **one file** (`access.rs`).
Task 1's spike **measures** the actual second-process behavior first — please read that measurement
before ruling.

**OQ2 — `install` / `uninstall` / `telemetry` / `upgrade`: flags-now-body-later, or out of scope?**
The roadmap gives Phase 6 "all 22 subcommands" but gives the **installer to Phase 7** and
**telemetry + upgrade to Phase 8**. My plan (Task 21) lands the **flag surface, the validation, the
exit codes, and the delegation call site**, with bodies that print one line and exit 1 until Phase
7/8 fill them — so Phase 7 fills a function a **live dispatch arm already calls** (the anti-inert-seam
shape). **Confirm**, or tell me to drop the four arms from Phase 6's clap tree entirely.

**OQ3 — What does `unlock` remove?** The TS removes `.codegraph/codegraph.lock`, an
**application-level** lock. Selene has no such file today. Candidates: (i) the daemon's
`daemon.pid`; (ii) SurrealDB's own `LOCK` file inside `graph.db/` (⚠ removing that under a live
holder is **dangerous** — it defeats the DB's own guard); (iii) a **new** app-level write-lock that
`index`/`sync` take (which is what makes `LockUnavailableError` — Task 13's quiet retry path —
meaningful in the first place). **I recommend (iii)**, and it is coupled to OQ1. Task 4 will not
invent one.

**OQ4 — `status`'s `backend` / `journalMode` fields are SQLite-isms.** The map pins
`Backend: node:sqlite — built-in (full WAL)` and a `Journal (wal)` line. **Selene has neither.** I
plan to print the truth (`Backend: surrealdb 3.2 (embedded, surrealkv)`) and keep the JSON keys with
honest values (`"journalMode": null`) so no consumer breaks on a missing key. **Confirm** — or say
whether the keys should be **dropped** from the JSON entirely (a wire-shape change either way, and I
would rather you choose it than discover it).

**OQ5 — Two glob dialects, and the map explicitly says to unify them "deliberately".**
`files --pattern` builds an **unanchored** regex (`**`→`.*`, `*`→`[^/]*`, `?`→`[^/]`);
`affected --filter` builds a **different** unanchored regex with different escaping (`**`→`.+`). They
disagree on `**` today. **Which wins?** My recommendation: **one `glob_to_regex()`** with `files`'s
dialect (`**`→`.*`), unanchored (preserving the substring-match behavior both rely on), used by
both — and a line in the ledger saying the TS had two. Task 8 owns the function; Task 10 calls it.

**OQ6 — What does `serve` (without `--mcp`) print?** The TS prints an MCP **config snippet + tool
list to stderr**. For Selene that snippet is presumably
`claude mcp add selene -- selene serve --mcp --path <root>` (or the `.mcp.json` block). **Confirm the
exact text** — it is user-facing copy and I will not invent product copy.

**OQ7 — Does the prompt-hook's MEDIUM tier have a vocabulary to stand on?** Tier 4 needs
`extract_prose_candidates` + a **symbol-segment vocabulary** + `heal_segment_vocab_if_empty`. Those
are `selene-context`/`selene-graph` structures that **Phases 4/5 may not have built** (I did not find
them). If they do not exist, Phase 6 has two honest choices: **(a)** ship tiers 1–3 + 5 and make tier
4 a no-op returning `noop-unverified` (my recommendation — the HIGH tier carries the value; MEDIUM is
a long tail), or **(b)** add the vocabulary as an extra task. **Do not** let a task invent a
vocabulary inline.

**OQ8 — Windows: port-and-`cfg`-gate, or defer wholesale?** I plan to **write** the win32 arms (named
pipes, `OpenProcess` liveness, `finalize_daemon_exit`'s drain, `CREATE_NEW_PROCESS_GROUP`) but
**gate CI to POSIX** (per the roadmap's §Risks). That means shipping code no test has ever run — which
is a smell, but the alternative (deleting the arms) means a **rewrite** later rather than a
**validation** later. **Confirm** the tradeoff, or tell me to `unimplemented!()` the win32 arms with a
clear message and own that v1 is POSIX-only.

**OQ9 — `~/.selene/daemons/` vs XDG.** The map uses `~/.codegraph/daemons/`. On Linux, the
XDG-correct location is `$XDG_STATE_HOME/selene/daemons/`. I plan to **keep `~/.selene/`** (parity,
one path on every OS, and it is where the rest of Selene's global state will want to live). Say if
you want XDG on Linux instead — it is a one-line change in Task 15 and a permanent one afterwards.

**OQ10 — `EXTRACTION_VERSION` drift and `sync`.** `status` warns "re-index recommended" when the
index was built by an older extraction version (never a hard error — the roadmap's contract). **Should
`sync` refuse to run incrementally against a drifted index** (and tell the user to `selene index`), or
sync anyway and let `status` carry the warning? The map is silent. My recommendation: **sync anyway,
warn once** — refusing turns a stale index into a *broken* tool, and the invariant is guidance over
refusal. **Confirm.**

---

## ✅ RULINGS — the maintainer's adjudication (2026-07-13). Binding; supersede the recommendations above where they differ.

**OQ1 — (a) Daemon-as-arbiter. RULED — but the spike RATIFIES it, not this ruling.**
This is the best catch in the plan. *"CLI commands never route through the daemon — they open the DB
directly"* is a **SQLite fact, and we are not on SQLite.** SQLite-WAL admits many processes;
SurrealDB embedded takes an exclusive `LOCK`. Ported literally, the map's sentence produces a
product where `selene status` **cannot run while an editor is attached** — which is not a degraded
feature, it is a broken one. So: **every graph-touching command goes through `GraphAccess`** — probe
the socket, speak MCP to a live daemon, else open the DB directly. Exactly one process ever holds
the DB.

⚠ **But Task 1's spike is the authority on the premise, not me.** The spike MUST measure what a
second process actually gets when the DB is held — exclusive-lock error, blocking wait, or (if
SurrealDB admits concurrent readers) nothing at all. **If the spike shows concurrent reads work, this
ruling is VOID and we follow the map** (direct access, no arbiter, Phase 6 shrinks by ~5 tasks).
Report the measurement before building on it. Do not implement (a) on the strength of the hypothesis
— that would be assuming the very fact the spike exists to establish.

**OQ2 — Confirmed: flags-now-body-later.** Land the flag surface, validation, exit codes, and the
**delegation call site**. That is deliberately the anti-inert-seam shape: Phase 7 then fills a
function that a **live dispatch arm already calls**, instead of adding a library nobody invokes.
Do not drop the arms.

**OQ3 — (iii): a new app-level write lock.** ⚠ **Never delete SurrealDB's own `LOCK`** under a live
holder — that defeats the DB's guard and corrupts the store; the TS's `unlock` removed an
*application* lock, and that distinction is the whole answer. `unlock` removes **our** lock (and a
stale `daemon.pid`), never the engine's. Coupled to OQ1: the app-level lock is what makes Task 13's
`LockUnavailableError` retry path mean anything.

**OQ4 — Print the truth; keep the JSON keys with honest values.**
`Backend: surrealdb 3.2 (embedded, surrealkv)`, `"journalMode": null`. A key that disappears breaks
a consumer silently; a key that says `null` tells it the truth. Never print `node:sqlite` — we would
be lying about our own storage engine in a diagnostic command whose entire job is to be believed.

**OQ5 — One `glob_to_regex()`, `files`'s dialect (`**`→`.*`), unanchored.** Task 8 owns it, Task 10
calls it. Record in the ledger that the TS carried two dialects that disagreed on `**` — that is a
bug we are declining to port, not a feature we are dropping.

**OQ6 — `serve` (no `--mcp`) prints this, to stderr, verbatim:**
```
selene: the MCP server is not started without --mcp.

Add SeleneCode to your agent:
  claude mcp add selene -- <ABSOLUTE_PATH_TO_SELENE> serve --mcp --path <ROOT>

or in .mcp.json:
  { "mcpServers": { "selene": { "command": "<ABSOLUTE_PATH_TO_SELENE>",
                                "args": ["serve", "--mcp", "--path", "<ROOT>"] } } }

Tools: selene_explore (start here) · selene_node · selene_search ·
       selene_callers · selene_callees · selene_impact · selene_files
```
`<ABSOLUTE_PATH_TO_SELENE>` is `current_exe()` resolved at runtime — **not** the bare name (same
ruling as Phase 7's Q8: a static binary is not guaranteed to be on `PATH`, and a config naming an
unrunnable command fails *silently* as an MCP server that never starts). `<ROOT>` is the resolved
project root.

**OQ7 — (a): ship tiers 1–3 + 5; tier 4 is a no-op returning `noop-unverified`.**
Correct instinct — **do not let a task invent a vocabulary inline.** An honestly-named no-op is
infinitely better than a tier that silently guesses. If the HIGH tier proves insufficient in
dogfood, tier 4 gets its own task with the vocabulary designed properly.

**OQ8 — v1 is POSIX-only. `unimplemented!()` the win32 arms with a clear message.** OVERRULING the
recommendation. "Ship code no test has ever run" is precisely this project's most expensive failure
mode — four inert seams shipped green and uninvoked, and the whole gate discipline exists because
*code that looks like it works is worse than code that says it doesn't*. Untested win32 arms are
that bug with a `cfg` on it. A loud `unimplemented!("SeleneCode v1 is POSIX-only; Windows support is
tracked for v2")` is honest, is a compile-time contract, and makes the eventual port a *validation*
against a real CI matrix rather than a debugging session against code nobody ever ran. Record it in
the roadmap's deferrals.

**OQ9 — Keep `~/.selene/`.** Parity, one path on every OS, and it is where Selene's global state
will live. Not XDG.

**OQ10 — Sync anyway, warn once.** Confirmed, and it follows directly from the `isError`-is-reserved
invariant: refusing turns a *stale* index into a *broken* tool, and the contract is
guidance-over-refusal. `status` carries the "re-index recommended" warning.
