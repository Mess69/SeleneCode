# Phase 7 — `selene-installer`: 8 targets, surgical config writes, reversible uninstall — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `selene install` writes SeleneCode's MCP-server registration into **8 different AI-tool
config files** — Claude Code, Cursor, Codex, opencode, Hermes, Gemini, Antigravity, Kiro — each
with its own format (JSON, JSONC-with-comments, TOML, YAML), and `selene uninstall` puts every
one of them back **byte-identical** to how it found them.

**This is a file-surgery problem, and the files are not ours.** `~/.claude.json` carries the
user's entire project history. `opencode.jsonc` carries their comments. `config.toml` carries
their other servers. Every one of these was written by a human who did not ask us to reformat it.
The whole phase is organized around one sentence:

> **We are editing config files the user did not write and cannot afford to lose.**

**THE GATE (Task 13):** the ~97 TS contract tests ported and green, driven against **real config
files on disk in a temp dir** — fixtures of realistic, messy, comment-laden user configs,
including one that already has three other MCP servers and a trailing comma — asserting, **byte
for byte**, the four contract properties:

| Property | What it means | How the gate proves it |
|---|---|---|
| **Idempotence** | `install` twice changes nothing the second time | second run: every `FileAction` is `unchanged` **and** the file's bytes are identical to after run 1 |
| **Neighbor preservation** | the user's OTHER MCP servers, comments, formatting, key order all survive | plant 3 sibling servers + comments + a trailing comma; after install/uninstall they are **byte-identical** outside our block |
| **Reversible uninstall** | uninstall returns the file to its pre-install state | snapshot bytes before install; assert `bytes_after_uninstall == bytes_before_install` |
| **Byte-equal re-run ⇒ `unchanged`** | the tool *reports* `unchanged`, not silently rewrites | the reported `WriteResult` is asserted, not just the bytes — a silent rewrite that lands on the same bytes still **fails** |

**Tech Stack:** `jsonc-parser` **0.33 with the `cst` feature** (the format/comment/key-order
preserving JSON **and** JSONC editor — see the Task 1 finding, it changes the shape of this
phase), `toml_edit` **0.25** (format-preserving TOML), `serde_json` (`Value` for
`jsonDeepEqual`-equivalent semantics **only** — never for writing), `thiserror` 2, `clap` 4.6
(bin), `anyhow` (bin); dev: `tempfile`, `insta` (optional). A hand-rolled line patcher for
Hermes YAML (no YAML crate — the TS is line-based on purpose, see Task 7).

**Reference (in priority order):**
- `docs/reference/from-codegraph/maps/installer.md` — **THE parity contract.** 206 lines: the 8
  targets, the registry, `--target auto|all|none|<id>`, the surgical-write strategy, the marker
  strips, and the ~97 contract tests. Every constant, path, marker string and target id this plan
  quotes is copied from it **verbatim**. A task should never need to open the map to execute; it
  opens it when this plan is ambiguous, and **the map wins** when they disagree.
- `docs/plans/2026-07-12-selenecode-roadmap.md` §"Phase 7" (line 124) — scope + the gate.
- `docs/plans/2026-07-13-phase45-graph-context-mcp.md` — the house form, and the source of the
  binary-wiring discipline this plan inherits (the binary is wired in **Task 3**, before a single
  target exists).
- `CLAUDE.md` §"Invariants — do not regress" — `isError` reserved; errors collected, never thrown;
  determinism; single source of tool guidance.
- `crates/selene-mcp/src/server.rs` — `Implementation.name = "selene"`, tools named
  `selene_explore` ⇒ the Claude permission string is `mcp__selene__*`. This is not a choice; it is
  read off the running server.
- TS parity source `../codegraph`: consult **ONLY** the specific file a task names
  (`src/installer/targets/<id>.ts`, `shared.ts`, `toml.ts`, `registry.ts`,
  `instructions-template.ts`, `__tests__/installer-targets.test.ts`) — never at large.

---

## Global Constraints (bind every task — reviewers use this as the rubric)

- **⚠ NEVER write a config file without first proving the round-trip is lossless on that exact
  file.** Before *any* mutation, the writer parses the file's current bytes, re-emits them
  unchanged, and compares: `parse(text).to_string() == text`. If that fails — an exotic construct
  the CST does not round-trip — the writer **refuses to write that file**, records a note, and
  reports `not-found`/`kept`. It never "does its best." This is a real function
  (`prove_lossless_roundtrip`), called on every path, and it is the single most load-bearing rule
  in the phase. **A destroyed `~/.claude.json` is not a bug report — it is a user who never comes
  back.**
- **Neighbor preservation is the product.** Anything outside our own block — other MCP servers,
  their comments, their key order, their indentation, their trailing commas, the file's line
  endings — comes out **byte-identical**. Tests assert bytes, not `serde_json::Value` equality;
  a `Value` comparison is exactly the assertion that would let a reformatting bug ship green.
- **`serde_json::Value` is for COMPARING, never for WRITING.** A `serde_json` round-trip destroys
  comments, key order, and formatting. It appears in this crate in exactly one role: the
  key-order-insensitive, array-order-sensitive deep-equal that decides `unchanged` vs `updated`
  (`Value`'s `PartialEq` already has exactly those semantics — map §Rust port notes). Any
  `to_string_pretty` call that is not the **greenfield-file seed** is a bug.
- **Idempotence is computed, not assumed.** Every writer computes the desired payload, compares it
  to what is on disk, and on equality **returns `unchanged` without opening the file for writing**.
  The file's mtime must not move. `created` means *the file did not exist*; adding a key to an
  existing file is `updated`. (One exception, ported verbatim: Codex treats empty content ⇒
  `created` — its `config.toml` is "ours".)
- **Errors are collected, never thrown** (CLAUDE.md invariant). An unparseable JSON file never
  aborts an install: it is backed up to `<path>.backup`, treated as `{}`, and a note is recorded.
  A failed `unlink`, a failed backup, a permission error on one target — none of them fail the
  other seven. **Exactly two things exit 1** (map §cli-daemon-sync §128): an unknown `--target` id
  (the crate's one `Err`), and an invalid `--location`. Nothing else, ever.
- **`isError` is reserved** (CLAUDE.md). The installer is a CLI surface, not an MCP tool, but the
  same discipline applies to its exit code: a target that is not installed, a config that does not
  exist, an uninstall on a clean slate — all are **success-shaped** (`not-found` / `not-configured`
  / `kept`), never failures.
- **Determinism.** Same inputs ⇒ byte-identical output files. No wall-clock, no `HashMap` iteration
  order in anything that reaches a file or a report. `ALL_TARGETS` order is **frozen** and
  user-visible (prompt order, `--target=all`, report order): `claude, cursor, codex, opencode,
  hermes, gemini, antigravity, kiro`. The **one** sanctioned nondeterminism is Antigravity's
  darwin `command -v selene` absolute-path resolution — ported as-is, documented, and called out
  in Open Question 5.
- **No `unwrap`/`expect` outside `#[cfg(test)]`** (workspace lint). A config file is exactly the
  place where "this can't be `None`" is wrong.
- **No globals. `Ctx` is injected.** The TS targets read `process.cwd()` / `HOME` / `APPDATA` /
  `XDG_CONFIG_HOME` / `HERMES_HOME` lazily at call time, and the whole test harness depends on it.
  In Rust, `std::env::set_current_dir` in a parallel test run is a data race, not a fixture. Every
  target takes a `&Ctx { home, cwd, env }` (Task 2). **A target that calls `std::env::var` or
  `current_dir()` directly is a defect** — clippy-visible in review, and it makes the gate
  unparallelizable.
- **Every task names its production call site.** Four seams have shipped in this project with green
  unit tests and **zero production callers**. An installer that works in its unit tests but is not
  reachable from `selene install` is that bug, wearing a different hat. Hence: **the binary is
  wired in Task 3, before a single target exists**, and the registry is the only way a target can
  be reached. A target that is not in `ALL_TARGETS` does not exist.
- **The gate drives the real binary and real files.** Not a mock filesystem, not an in-memory
  `Vec<u8>` FS trait. `tempfile::TempDir` + a `Ctx` pointed at it + the actual `selene install`
  code path. If it does not touch a real disk, it is not testing the thing that can destroy a
  user's file.
- **Tasks are completable by a fresh subagent in one session.** Each names its Files and Interfaces,
  is **TDD** (write the ported contract test first, watch it fail, then implement), and ends in
  **one conventional commit**. `cargo fmt && cargo clippy --all-targets && cargo test` green before
  every commit.

---

## The rename table (mechanical, test-asserted — apply this and nothing else)

The map's Rust port note: *"`getMcpServerConfig` constants must obviously say `selene serve
--mcp` (rename decision — the **shape** is the contract)."* The shape is the contract; the strings
below are the rename. **Nothing else changes** — not a path, not a note, not a heuristic.

| TS (CodeGraph) | Selene | Where |
|---|---|---|
| `codegraph` (the command) | `selene` | every MCP entry's `command` |
| `["serve","--mcp"]` | `["serve","--mcp"]` — **unchanged** | every MCP entry's `args` |
| `mcpServers.codegraph` | `mcpServers.selene` | claude, cursor, gemini, kiro, antigravity |
| `mcp.codegraph` | `mcp.selene` | opencode |
| `[mcp_servers.codegraph]` | `[mcp_servers.selene]` | codex TOML |
| `mcp_servers: codegraph:` | `mcp_servers: selene:` | hermes YAML |
| `- mcp-codegraph` | `- mcp-selene` | hermes `platform_toolsets.cli` |
| `mcp__codegraph__*` | `mcp__selene__*` | claude `permissions.allow` — **verified** against `Implementation.name = "selene"` in `crates/selene-mcp/src/server.rs` |
| `.codegraph/` | `.selene/` | the instructions block prose |
| `codegraph init` | `selene index` | the instructions block prose |
| `codegraph explore "<…>"` | `selene explore "<…>"` | the instructions block prose (the CLI subcommand is **Phase 6**'s — see Open Question 7) |
| `.cursor/rules/codegraph.mdc` | `.cursor/rules/selene.mdc` | cursor cleanup |
| `.kiro/steering/codegraph.md` | `.kiro/steering/selene.md` | kiro cleanup |
| `<!-- CODEGRAPH_START/END -->` | **⚠ OPEN QUESTION 1** — do not guess | the marker strip |
| `npm install -g @colbymchenry/codegraph` | **DROPPED** — a Rust static binary has no npm step | interactive flow (Task 12) |

**Target ids do NOT change**: `claude`, `cursor`, `codex`, `opencode`, `hermes`, `gemini`,
`antigravity`, `kiro` — they name *the other tool*, not ours.

---

## File structure

```
crates/selene-installer/
  Cargo.toml              [T1] jsonc-parser (feature "cst"), toml_edit, serde_json, serde,
                          thiserror; dev: tempfile
  src/lib.rs              [T2 rewrites / T12 ledger pass] crate docs, the ledger, re-exports
  src/error.rs            [T2] InstallError (thiserror) — the ONE throwing case: UnknownTarget
  src/ctx.rs              [T2] Ctx { home, cwd, env } — the testability seam. NO globals.
  src/types.rs            [T2] AgentTarget trait, Location, TargetId, DetectionResult,
                          WriteResult, FileAction, InstallOptions, FileEntry
  src/fsx.rs              [T2] atomic_write, read_json_file (+ .backup), json_deep_equal,
                          remove_file_quiet, tildify
  src/registry.rs         [T3] ALL_TARGETS (frozen order), get_target, list_target_ids,
                          detect_all, resolve_target_flag, uninstall_targets, refresh_targets
                          ⚠ SHARED SEAM — every target task appends exactly one row
  src/json_edit.rs        [T4] THE surgical JSON/JSONC writer (all 6 JSON targets + opencode)
                          upsert_key_path / remove_key_path / prove_lossless_roundtrip
  src/toml_write.rs       [T5] the Codex TOML writer over toml_edit
  src/markdown.rs         [T6] replace_or_append_marked_section, remove_marked_section,
                          upsert_instructions_entry
  src/instructions.rs     [T6] SELENE_SECTION_START/END + SELENE_INSTRUCTIONS_BLOCK (exact bytes)
  src/yaml_lines.rs       [T7] the Hermes line patcher (top_level_range, child_range,
                          list_child_block, join_lines)
  src/targets/mod.rs      [T3 creates] `pub mod` per target — one line added per target task
  src/targets/claude.rs   [T8]  .mcp.json / ~/.claude.json, settings.json permissions + hooks,
                          CLAUDE.md, legacy ./.claude.json migration
  src/targets/cursor.rs   [T9]  mcp.json with --path injection, .cursor/rules/selene.mdc cleanup
  src/targets/gemini.rs   [T9]  .gemini/settings.json + GEMINI.md
  src/targets/kiro.rs     [T9]  .kiro/settings/mcp.json + steering-file delete
  src/targets/codex.rs    [T10] ~/.codex/config.toml + ~/.codex/AGENTS.md (global-only)
  src/targets/hermes.rs   [T10] $HERMES_HOME/config.yaml (global-only)
  src/targets/opencode.rs [T11] opencode.jsonc|.json + AGENTS.md + %APPDATA% legacy sweep
  src/targets/antigravity.rs [T11] unified vs legacy mcp_config.json (global-only)
  src/flow.rs             [T12] run_install / run_uninstall (non-interactive core, T3) +
                          the interactive prompts (T12), print_config, describe_paths

crates/selene/src/main.rs  [T3] clap: `selene install` / `selene uninstall` — THE production
                           call site. Wired BEFORE any target exists.

crates/selene-installer/tests/
  common/mod.rs           [T2 creates] the harness: TempDir home + TempDir cwd + a Ctx over them;
                          snapshot_tree(dir) -> BTreeMap<PathBuf, Vec<u8>> for byte-diffing
  editors_test.rs         [T4/T5/T6/T7] the four writers' unit contracts
  targets_contract.rs     [T13] THE GATE — the ~97 ported contract tests
  fixtures/               [T13 owns; T8–T11 contribute] messy, comment-laden real-world configs
    messy-claude.json         3 sibling MCP servers, a `customField`, nested project history
    messy-opencode.jsonc      line + block comments, a TRAILING COMMA, 3 sibling servers
    messy-codex.toml          [other_table], [[array_of_tables]], a [zzz] after our block
    messy-hermes.yaml         PyYAML same-indent lists (#456), a sibling `other:` server
    corrupt.json              unparseable — must be backed up, never lost
docs/benchmarks/2026-07-phase7-installer.md   [T13] the gate's results table
```

---

## ⚠ Task sequencing — the shared seams

Files touched by more than one task. Tasks that touch the same file are **strictly sequential** —
never dispatch two of them to parallel subagents or worktrees.

| Shared file | Tasks that modify it | Rule |
|---|---|---|
| `src/lib.rs` | **2** (rewrites), 3–12 | Append-only, one `mod`/`pub use` line per task; **12** does the facade + ledger pass and must be the LAST task to touch it. |
| `src/registry.rs` | **3** (creates, with `ALL_TARGETS` **empty**), **8, 9, 10, 11** (each appends its rows) | STRICTLY SEQUENTIAL. `ALL_TARGETS`'s order is a frozen contract — a task appends **in the declared order**, it never re-sorts. Two target tasks in parallel = a lost row, and a lost row is a target that silently does not exist. |
| `src/targets/mod.rs` | **3** (creates, empty), 8–11 (one `pub mod` line each) | Same rule. One line, appended. |
| `src/json_edit.rs` | **4** (creates), **9/11** (read-only callers — never add a format branch) | 4 must land before 8. If a target "needs" a new option here, that is a signal the target is wrong, not the writer. |
| `src/flow.rs` | **3** (creates: `run_install`/`run_uninstall`, non-interactive, driven by flags), **12** (adds the prompts + `print_config` + `describe_paths`) | STRICTLY SEQUENTIAL 3 → 12. **3 lays down the full ordered flow** with the interactive steps as named stubs; 12 fills them and **never re-orders**. |
| `crates/selene/src/main.rs` | **3** (adds `install` + `uninstall`), **13** (gate drives it — read-only) | ⚠ Phase 5 is landing `index` + `serve --mcp` here **right now**. Rebase before touching. Phase 6 owns every other subcommand — do not add one. See Open Question 7 (does the CLI move to `selene-cli` first?). |
| `tests/fixtures/` | **13** owns the tree; **8–11** contribute a fixture per target | A target task adds its messy fixture; only 13 adds the contract suite over them. **ONE corpus.** |

**Parallelizable after their blocker lands:** T5 (`toml_write.rs`), T6 (`markdown.rs` +
`instructions.rs`) and T7 (`yaml_lines.rs`) are each a **fresh file** with no shared state — they
may run concurrently with each other once T2 is in. T9's three targets share a shape but are three
independent files; they collide only on the two one-line registry/mod appends, which the task does
once at its end.

---

## Deliberately deferred (each with its phase and its reason)

Naming these is what stops a task from half-porting one.

- **Telemetry** (`telemetry.install` lifecycle, the opt-in prompt, `kind:
  fresh|upgrade|reinstall`) → **Phase 8** (with upgrade + project-config, per the roadmap). Task 12
  ports the flow **without** steps 4½ and 6; it leaves the `sawCreated`/`sawUpdated` computation in
  place (it is one `fold` and it costs nothing) so Phase 8 has something to hand a reporter.
- **`offerWatchFallback`** (the git-hook watch fallback) → **Phase 6** (`selene-sync` owns the
  watcher and the git hooks). It is the one flow step that reaches *out* of the installer into a
  subsystem that does not exist yet. Do not port a stub that writes a git hook.
- **The npm global-install offer** (`npm install -g @colbymchenry/codegraph`, `execSync`, 120 000 ms
  timeout) → **DROPPED, permanently.** A single static Rust binary has no npm step. The distribution
  story (cross-compilation, npm shim, Homebrew) is the **post-v1 distribution phase** (roadmap
  §"Locked decisions" 4).
- **`config-writer.ts`** (the deprecated back-compat shim: `writeMcpConfig`, `writePermissions`,
  `hasMcpConfig`, `hasPermissions`) and **`clack.d.ts`** → **DROPPED** (map §Rust port notes says so
  explicitly). Their 3 shim tests (`installer.test.ts`) are ported as assertions **against the
  claude target directly** (Task 8), not as a shim.
- **The legacy-hook cleanup's *CodeGraph* strings** (`codegraph mark-dirty`, `codegraph
  sync-if-dirty`, `npx @colbymchenry/codegraph …`) → **Open Question 3.** The *mechanism* (two-pass
  strip, prune empty groups → events → `hooks`) is ported in Task 8 regardless; only the substrings
  it matches are in question.
- **`selene prompt-hook` / `selene mark-dirty` as real subcommands** → **Phase 6** (`selene-cli`).
  Task 8 writes the hook *entry* (`{type:'command', command:'selene prompt-hook'}`); the
  subcommand it names is Phase 6's. Ordering is fine (7 comes after 6) — but if Phase 7 is executed
  **before** Phase 6 lands, the prompt hook writes a command that does not exist yet. See Open
  Question 4.
- **The nix option-path / wave-2 target ecosystems** → **Phase 8**. Eight targets, frozen.

---

## Tasks

<!-- Each task is one commit. Task 13 is THE GATE. -->

### Task 1: Spike — prove the surgical writers are lossless *before* designing around them

**Files:** Create: `crates/selene-installer/tests/spike_editors.rs`. Modify:
`crates/selene-installer/Cargo.toml`, root `Cargo.toml` (`jsonc-parser` feature opt-in).

**Interfaces:** none — throwaway knowledge, kept as smoke tests. **Every finding goes into a
comment block at the top of the spike file and, where it changes a later task, into this plan.**

**The central technical risk of this phase, stated plainly:** a naive `serde_json` round-trip —
`from_str::<Value>` → mutate → `to_string_pretty` → write — **destroys comments, destroys key
order, destroys the user's indentation, and drops trailing commas.** It therefore fails *neighbor
preservation*, which is the gate. Every other design question in this phase is downstream of the
question "what writes the file?"

**⚠ A finding that changes the shape of the phase (verified 2026-07-13, against the vendored
source of `jsonc-parser` 0.33.0):** the map's Rust port note says *"no jsonc-parser equivalent
exists off-the-shelf with `modify`/`applyEdits` semantics; you'll need a span-tracking JSONC
editor."* **That is now false.** `jsonc-parser` 0.33 — **already pinned** in
`[workspace.dependencies]` — ships a full **CST** behind the `cst` feature:

```rust
// crates/.../jsonc-parser-0.33.0/src/cst/mod.rs — verified present
CstRootNode::parse(text, &ParseOptions { allow_comments, allow_trailing_commas, .. }) -> Result<CstRootNode, ParseError>
root.object_value_or_set() -> CstObject
object.object_value_or_set(name: &str) -> CstObject      // creates the wrapper if absent
object.object_value(name) / .array_value(name) -> Option<..>
object.append(prop_name, CstInputValue) -> CstObjectProp  // CstInputValue::{String,Bool,Array,Object,..}
object.insert(index, prop_name, value) / prop.set_value(v) / prop.remove() / object.remove()
root.single_indent_text() -> Option<String>               // INFERS the file's own indentation
root.newline_kind() -> CstNewlineKind                     // INFERS LF vs CRLF
node.uses_trailing_commas() -> bool                       // the trailing-comma fixture, handled
impl Display for CstRootNode                              // the round-trip back to text
```

This is `modify`/`applyEdits` semantics, in Rust, from a dep we already pin. **No hand-rolled span
editor.** And because JSON is a subset of JSONC, **the same writer serves all 8 targets' JSON
files** — which is why this plan has *one* JSON writer task (Task 4) and not one per target.

- [ ] **Adopt the feature, don't add a dep.** Root `Cargo.toml`: `jsonc-parser = { version =
  "0.33", features = ["cst"] }`. This is a **feature opt-in on an already-pinned dep**, not a new
  dependency — no justification owed. `toml_edit` 0.25 is likewise already pinned. **The only
  genuinely new dep in this phase is the interactive-prompt crate in Task 12** (`inquire`), and it
  is justified there.
- [ ] **Prove the JSON/JSONC round-trip is lossless — on the ugly cases, not the pretty ones.**
  Write a table-driven test over inputs that a user's real config actually looks like:
  a `//` line comment, a `/* */` block comment, a **trailing comma**, 4-space indent, tab indent,
  CRLF line endings, a top-level key order of `z, a, m`, a nested `mcpServers` with **3 sibling
  servers**, and a file with **no trailing newline**. For each: `CstRootNode::parse(text)?` →
  `.to_string()` → assert **byte-equal to the input**. Then: append `mcpServers.selene`, and assert
  every one of those properties **survives** in the output (comments still there, siblings in the
  same order, the trailing comma still trailing, CRLF still CRLF).
- [ ] **Prove the same for plain `.json`** (no comments) — because Claude/Cursor/Gemini/Kiro/
  Antigravity files are plain JSON, and the CST must be *at least* as good there. Record whether
  the CST's greenfield emission matches TS's `JSON.stringify(obj, null, 2) + '\n'` byte-for-byte;
  if not, record that **greenfield files are seeded with a literal string** (Task 4's approach) and
  the CST is used only for *edits* to existing files.
- [ ] **⚠ Find the round-trip's breaking point, and write it down.** Feed the CST something it
  *cannot* round-trip (a `NaN`, an unterminated string, a BOM, a duplicate key, a lone surrogate).
  Whatever fails is exactly what `prove_lossless_roundtrip` must catch **before** we write. A
  writer that has never been shown its own failure mode does not have one — it has an undetected
  data-loss bug. Record the list.
- [ ] **`toml_edit`: prove sibling + array-of-tables preservation.** Parse a `config.toml` with
  `[other_table]`, `[[array_of_tables]]` (×2), an inline comment on a value, and a `[zzz]` table
  **after** where ours goes. Upsert `[mcp_servers.selene]`, assert every sibling — **including the
  `[[array-of-tables]]`, which the TS hand-rolled parser has a special case for** — is byte-identical
  and the comments survive. Then remove it and assert byte-equality with the original.
  ⚠ Record where `toml_edit`'s *insertion position* lands vs. the TS's (TS inserts at EOF with
  `trimEnd() + '\n\n' + block + '\n'`). If `toml_edit` puts it elsewhere, that is fine — but it
  means the TS's exact byte output is **not** reproducible, and Task 5 must say so instead of
  pretending. Byte-parity with TS is **not** a requirement; byte-parity with *the user's own file*
  is.
- [ ] **`HOME`/`USERPROFILE` override order** (map §Rust port notes): confirm that reading `HOME`
  then `USERPROFILE` from an injected env map reproduces `os.homedir()`'s behavior, and that **no
  `dirs`/`home` crate is needed** — the test harness must be able to fake home, and a crate that
  reads the real OS home would make the whole gate unrunnable. Record: **no new dep.**
- [ ] Commit: `chore(installer): spike jsonc-parser CST + toml_edit — the surgical writers are lossless`

### Task 2: Core — `Ctx`, the `AgentTarget` trait, atomic writes, JSON read + backup

**Files:** Create: `crates/selene-installer/src/{ctx.rs, types.rs, error.rs, fsx.rs}`,
`crates/selene-installer/tests/common/mod.rs`; rewrite `crates/selene-installer/src/lib.rs`.

**Production call site:** none yet — this is the only task in the phase without one, and it is
allowed exactly one task's worth of that. **Task 3 wires the binary.** Nothing here is public API
until the registry can reach it.

**Interfaces (map §Public interface — `targets/types.ts`, `targets/shared.ts`):**
```rust
// ctx.rs — THE testability seam. Replaces process.cwd() + process.env, which the TS reads lazily
// and the entire test harness depends on. `set_current_dir` in a parallel test run is a race.
pub struct Ctx { home: PathBuf, cwd: PathBuf, env: BTreeMap<String, String> }
impl Ctx {
    pub fn from_process() -> Self;                 // the ONE place std::env is read. Bin only.
    pub fn new(home: PathBuf, cwd: PathBuf, env: BTreeMap<String, String>) -> Self;  // tests
    pub fn home(&self) -> &Path;                   // HOME, then USERPROFILE — os.homedir() order
    pub fn cwd(&self) -> &Path;
    pub fn env(&self, key: &str) -> Option<&str>;  // APPDATA, XDG_CONFIG_HOME, HERMES_HOME
    pub fn xdg_config_home(&self) -> PathBuf;      // ${XDG_CONFIG_HOME:-<home>/.config}
}

// types.rs
#[derive(Clone, Copy, PartialEq, Eq)] pub enum Location { Global, Local }
#[derive(Clone, Copy, PartialEq, Eq)] pub enum TargetId { Claude, Cursor, Codex, Opencode,
                                                          Hermes, Gemini, Antigravity, Kiro }
impl TargetId { pub fn as_str(&self) -> &'static str; }   // "claude" | "cursor" | … — WIRE CONTRACT
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileAction { Created, Updated, Unchanged, Removed, NotFound, Kept }
pub struct FileEntry { pub path: PathBuf, pub action: FileAction }
pub struct WriteResult { pub files: Vec<FileEntry>, pub notes: Vec<String> }
pub struct DetectionResult { pub installed: bool, pub already_configured: bool,
                             pub config_path: Option<PathBuf> }
pub struct InstallOptions { pub auto_allow: bool, pub prompt_hook: Option<bool> }
        // prompt_hook: Some(true)=write, Some(false)=strip, None=leave — port the tri-state
pub trait AgentTarget: Send + Sync {
    fn id(&self) -> TargetId;
    fn display_name(&self) -> &'static str;
    fn docs_url(&self) -> Option<&'static str>;
    fn supports_location(&self, loc: Location) -> bool;
    fn detect(&self, ctx: &Ctx, loc: Location) -> DetectionResult;
    fn install(&self, ctx: &Ctx, loc: Location, opts: &InstallOptions) -> WriteResult;
    fn uninstall(&self, ctx: &Ctx, loc: Location) -> WriteResult;   // safe on a clean slate
    fn print_config(&self, ctx: &Ctx, loc: Location) -> String;     // ⚠ MUST NOT touch the FS
    fn describe_paths(&self, ctx: &Ctx, loc: Location) -> Vec<PathBuf>;
}

// error.rs — the ONE throwing case in the whole crate.
#[derive(Debug, thiserror::Error)]
pub enum InstallError { #[error("Unknown --target id(s): {bad}. Known: claude, cursor, codex, \
    opencode, hermes, gemini, antigravity, kiro, plus 'auto' / 'all' / 'none'.")]
    UnknownTarget { bad: String } }

// fsx.rs — map §targets/shared.ts
pub fn read_json_file(p: &Path) -> serde_json::Value;   // {} on missing; UNPARSEABLE → back up to
                                                        // <p>.backup, warn, return {} — NEVER Err
pub fn atomic_write(p: &Path, content: &str) -> std::io::Result<()>;  // <p>.tmp.<pid> + rename;
                                                                      // mkdir -p parent; same dir
pub fn json_deep_equal(a: &Value, b: &Value) -> bool;   // = `a == b`. Value's PartialEq is already
                                                        // key-order-INsensitive, array-order-SENSITIVE
pub fn remove_file_quiet(p: &Path);                     // unlink failures SWALLOWED (map)
pub fn tildify(p: &Path, home: &Path) -> String;        // display only
```

- [ ] **`WriteResult.action` includes `Kept` and it looks dead — port it anyway.** The map's
  §Suspected dead/quirky TS: no target *returns* `kept` from install/uninstall except via
  `remove_marked_section`'s missing-file case, which flows into uninstall's `files`. The TS's
  install logger then renders `kept`/`not-found` as "Updated" (a **cosmetic bug**); the uninstall
  path filters on `removed` only, so it is harmless. **Port the variant, fix the logger** (Task 12
  renders `Kept`/`NotFound` honestly) and write the comment citing this line — otherwise the next
  reader deletes the variant and a `remove_marked_section` return value has nowhere to go.
- [ ] **`read_json_file` accepts ANY JSON and preserves unknown keys** (map §Rust port notes). It
  returns `serde_json::Value` — **never** a typed struct. A typed struct is precisely the change
  that silently drops the user's `customField` and every sibling MCP server, and every test would
  still pass. Comment it.
- [ ] **`atomic_write`**: temp file `<path>.tmp.<pid>`, then **rename in the same directory** (a
  cross-device rename is not atomic), `mkdir -p` the parent first, clean up the temp on failure.
- [ ] **`Ctx::from_process` is the ONE place `std::env` is read**, and it lives in this file. Every
  other module takes `&Ctx`. Add a `#[deny]`-shaped comment; a grep for `env::var` outside `ctx.rs`
  is the review check.
- [ ] **`display_name` + `docs_url` — VERBATIM from the TS** (pulled 2026-07-13 from
  `../codegraph/src/installer/targets/*.ts`; the map declares the fields but not the values, so
  these were read from the source rather than guessed — two of them are **not** what a reasonable
  person would guess, which is the whole point):

  | id | `display_name` | `docs_url` |
  |---|---|---|
  | `claude` | `Claude Code` | `https://docs.claude.com/en/docs/claude-code` |
  | `cursor` | `Cursor` | `https://docs.cursor.com/context/model-context-protocol` |
  | `codex` | `Codex CLI` | `https://github.com/openai/codex` |
  | `opencode` | `opencode` (lowercase — it is styled that way) | `https://opencode.ai/docs/config` |
  | `hermes` | **`Hermes Agent`** (not "Hermes") | `https://hermes-agent.nousresearch.com` |
  | `gemini` | `Gemini CLI` | `https://geminicli.com/docs/tools/mcp-server/` |
  | `antigravity` | **`Antigravity IDE`** (not "Antigravity") | `https://antigravity.google` |
  | `kiro` | `Kiro` | `https://kiro.dev/docs/cli/mcp/` |

  These name **the other tool**, not ours — the rename table does not touch them.
- [ ] **`tests/common/mod.rs` — the harness the gate is built on.** `TempDir` home + `TempDir` cwd,
  a `Ctx` over them with `APPDATA` and `XDG_CONFIG_HOME` set (→ `<home>/.config`) and `HERMES_HOME`
  **absent**, mirroring the TS harness exactly. Plus `snapshot_tree(dir) -> BTreeMap<PathBuf,
  Vec<u8>>` — a **byte-level** recursive snapshot. This is what makes "print_config writes nothing"
  and "uninstall is byte-reversible" *assertable* rather than aspirational.
- [ ] TDD: `read_json_file` on missing → `{}`; on **corrupt** → `{}` **and** `<p>.backup` exists
  **with the original bytes** (the `installer.test.ts` shim test, ported here); `atomic_write`
  creates parents; `json_deep_equal` is key-order-insensitive (`{a:1,b:2}` == `{b:2,a:1}`) and
  array-order-**sensitive** (`[1,2]` != `[2,1]`); `Ctx` reads `HOME` before `USERPROFILE`.
- [ ] Commit: `feat(installer): Ctx, AgentTarget trait, atomic writes, JSON read with backup`

### Task 3: The registry, the flag resolution, the sweeps — and **`selene install` in the real binary**

**Files:** Create: `crates/selene-installer/src/{registry.rs, flow.rs, targets/mod.rs}`,
`crates/selene-installer/tests/registry_test.rs`. Modify: `crates/selene/src/main.rs` (add
`install` + `uninstall`), `crates/selene/Cargo.toml` (`selene-installer` dep),
`crates/selene-installer/src/lib.rs`.

**⚠ Rebase first.** Phase 5 is landing `index` + `serve --mcp` into `main.rs` right now.

**Production call site: THIS TASK IS IT.** `ALL_TARGETS` starts **empty**, and `selene install
--target all` runs end-to-end and reports "no targets" — a live, exercised production path with
zero targets in it. Every subsequent target task appends one registry row and is, from that
instant, reachable from the binary. **This is deliberate and it is not negotiable:** four seams in
this project shipped with green unit tests and no caller. A target that is implemented but not in
`ALL_TARGETS` is that bug — and the *only* way to notice is for the registry to have been the
product's spine from before there was anything in it.

**Interfaces (map §`targets/registry.ts` + §`installer/index.ts`):**
```rust
// registry.rs
pub fn all_targets() -> &'static [&'static dyn AgentTarget];   // FROZEN ORDER:
                          // claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro
pub fn get_target(id: &str) -> Option<&'static dyn AgentTarget>;
pub fn list_target_ids() -> Vec<&'static str>;
pub fn detect_all(ctx: &Ctx, loc: Location) -> Vec<(&'static dyn AgentTarget, DetectionResult)>;
pub fn resolve_target_flag(value: &str, ctx: &Ctx, loc: Location)
    -> Result<Vec<&'static dyn AgentTarget>, InstallError>;     // the ONE Err in the crate

// flow.rs — the non-interactive core. Task 12 adds the prompts around it.
pub enum UninstallStatus { Removed, NotConfigured, Unsupported }
pub struct UninstallReport { pub id: TargetId, pub display_name: String,
                             pub status: UninstallStatus, pub removed_paths: Vec<PathBuf>,
                             pub notes: Vec<String> }
pub fn uninstall_targets(ctx: &Ctx, targets: &[&dyn AgentTarget], loc: Location)
    -> Vec<UninstallReport>;                                    // PURE. No prompts. No I/O beyond the targets.
pub enum RefreshStatus { Refreshed, Unchanged, NotConfigured, Unsupported }
pub struct RefreshReport { pub id: TargetId, pub display_name: String, pub location: Location,
                           pub status: RefreshStatus, pub changed_paths: Vec<PathBuf> }
pub fn refresh_targets(ctx: &Ctx, targets: &[&dyn AgentTarget], loc: Location) -> Vec<RefreshReport>;
pub struct InstallFlowOptions { pub target: Option<String>, pub location: Option<Location>,
                                pub auto_allow: Option<bool>, pub yes: bool, pub refresh: bool }
pub fn run_install(ctx: &Ctx, opts: &InstallFlowOptions) -> Result<Vec<WriteResult>, InstallError>;
pub fn run_uninstall(ctx: &Ctx, opts: &UninstallFlowOptions) -> Result<Vec<UninstallReport>, InstallError>;
```

- [ ] **`resolve_target_flag`, verbatim** (map §`--target` flag resolution): `"none"` → `[]`;
  `"all"` → the whole registry **in frozen order**; `"auto"` → every target with
  `detect(loc).installed == true`, **falling back to `[claude]` if none detected**; otherwise CSV
  split on `,`, each entry trimmed, empty entries dropped; **any** unknown id → `Err` with the
  message **exactly**:
  `Unknown --target id(s): <bad,...>. Known: claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro, plus 'auto' / 'all' / 'none'.`
  (Unknown ids are collected and reported **together**, comma-joined — one bad flag, one message.)
- [ ] **`uninstall_targets` is PURE** (map): unsupported location → status `Unsupported` + the note
  `` `no {location} config — this agent is {other}-only` `` (verbatim, `{other}` = the other
  location word); else `uninstall(loc)`; `removed_paths` = the entries whose action is `Removed`;
  any ⇒ `Removed`, none ⇒ `NotConfigured`. **No prompts, no printing** — it returns reports and the
  caller renders them. That purity is what makes the gate able to assert on it.
- [ ] **`refresh_targets`** (map): unsupported → `Unsupported`; `!detect(loc).already_configured` →
  `NotConfigured` (**it NEVER first-installs** — that is the whole point of refresh); else
  `install(loc, InstallOptions { auto_allow: false, prompt_hook: None })`; `changed_paths` = actions
  in `{Created, Updated, Removed}`; any ⇒ `Refreshed`, none ⇒ `Unchanged`. `install --refresh`
  sweeps **both** locations unless `--location` narrows.
- [ ] **`run_install` — lay the full ordered flow down now, with the interactive steps as named
  stubs** (`// step 3: location prompt — TODO(Task 12)`). Task 12 fills them and **never
  re-orders**. The order (map §Interactive install flow, minus the deferred steps):
  1. resolve targets — explicit `--target` wins; `--yes` ⇒ `auto`; else **[T12] multiselect**
  2. ~~npm install offer~~ — **DROPPED** (static binary)
  3. location — flag > `--yes`⇒`Global` > **if every selected target is global-only, force `Global`**
     > **[T12] select prompt**
  4. `auto_allow` — flag > `--yes`⇒`true` > **[T12] confirm, only if claude is selected**, else `false`
  4½. ~~telemetry opt-in~~ — **deferred → Phase 8**
  4¾. `prompt_hook` — **only if claude is selected**; `--yes`⇒`Some(true)`; else **[T12] confirm
     (default yes)**; if claude is not selected, `None`
  5. loop targets in registry order: unsupported location ⇒ warn + skip; else
     `install(loc, opts)`, one log line per file
  6. ~~telemetry lifecycle~~ — **deferred → Phase 8** (keep the `saw_created`/`saw_updated` fold)
  **Never index.** (`codegraph init` ≢ `codegraph install`; here, `selene index` ≢ `selene install`.)
- [ ] **Wire the binary** (`crates/selene/src/main.rs`) — the whole point of the task. **The flag
  surface is NOT invented: it is `maps/cli-daemon-sync.md` §128–129, verbatim:**
  ```
  selene install    -t/--target <auto|all|none|csv>  -l/--location <global|local>  -y/--yes
                    --no-permissions  --print-config <id>  --refresh
  selene uninstall  -t/--target  -l/--location  -y/--yes  --keep-cli     # --keep-cli: see OQ 9
  ```
  **`--no-permissions` is a tri-state, and the mapping is exact** (map §128): explicitly false ⇒
  `auto_allow = false`; `--yes` ⇒ `true`; otherwise `None` ⇒ **prompt** (Task 12). It maps onto
  `InstallOptions.auto_allow`, and it is the only reason that option is an `Option<bool>`.
  `Ctx::from_process()` is constructed **here** and nowhere else.
  **Exit codes**: an unknown `--target` id ⇒ **1** (the crate's one `Err`); an **invalid
  `--location`** ⇒ **1** (map §128 — clap's own enum rejection gives 2, so this must be a manual
  parse-and-check to hit 1; a test pins it); everything else ⇒ **0** with a printed report.
- [ ] TDD (`registry_test.rs`, map §Test coverage 9 + 10 + 11): `get_target` returns `Some` for all
  **8** ids and `None` for `"nope"` — ⚠ this test is written **now** and **fails until Task 11**;
  that failing assertion *is* the registry-completeness ledger, and it is the mechanism that makes
  a forgotten row impossible. Mark it `#[ignore = "un-ignore as targets land: Task 8/9/10/11"]`
  with the row list in the message, and **each target task un-ignores its own id**.
  `resolve_target_flag("none")` → `[]`; `("all")` → the frozen order; `("cursor,claude")` → **CSV
  order, not registry order** (port the TS: CSV preserves the user's order); `("bogus,claude")` →
  `Err` with the exact message; `("auto")` with nothing detected → `[claude]`.
- [ ] **A CLI smoke test that drives the real binary**: `selene install --target none --yes` exits
  0 and writes **nothing** (assert with `snapshot_tree` before/after); `selene install --target
  bogus` exits **1** with the exact message on stderr.
- [ ] Commit: `feat(installer): registry, --target resolution, sweeps — and `selene install` in the binary`

### Task 4: `json_edit.rs` — **THE** surgical JSON/JSONC writer (one writer, six targets)

**Files:** Create: `crates/selene-installer/src/json_edit.rs`,
`crates/selene-installer/tests/json_edit_test.rs`. Modify: `src/lib.rs` (one line).

**Production call site:** every JSON target — claude (T8), cursor/gemini/kiro (T9), codex's
AGENTS.md excepted, opencode/antigravity (T11). **Six targets, one writer.** That is the whole
reason this is a task and not six.

**This is the task the phase is about.** Get it right and neighbor preservation is free everywhere;
get it wrong and every target task inherits a data-loss bug.

**Interfaces:**
```rust
/// The one function that is allowed to decide a file is safe to touch.
/// Parses, re-emits, compares. Byte-inequality ⇒ Err ⇒ the caller REFUSES TO WRITE.
pub fn prove_lossless_roundtrip(text: &str) -> Result<CstRootNode, LossyFile>;
pub struct LossyFile { pub reason: String }   // → a note + FileAction::Kept. Never a panic.

/// Upsert `value` at `key_path` (e.g. ["mcpServers","selene"] or ["mcp","selene"]),
/// creating intermediate objects. Returns the action; on Unchanged it does NOT write.
pub fn upsert_key_path(p: &Path, key_path: &[&str], value: &Value, seed: Option<&str>)
    -> FileEntry;
/// Remove `key_path`. If `prune_empty_parent` and the parent object is then `{}`, remove it too
/// (opencode's `mcp` wrapper). If the whole file is then `{}`, `delete_if_empty` unlinks it.
pub fn remove_key_path(p: &Path, key_path: &[&str], prune_empty_parent: bool,
                       delete_if_empty: bool) -> FileEntry;
/// Append `s` to a string array at `key_path`, deduped (claude's permissions.allow).
pub fn append_to_string_array(p: &Path, key_path: &[&str], s: &str) -> FileEntry;
/// Remove every array entry with `prefix`; delete the emptied array key, then its emptied parent.
pub fn remove_from_string_array_by_prefix(p: &Path, key_path: &[&str], prefix: &str) -> FileEntry;
```

- [ ] **The write algorithm, in order — and step 2 is the Global Constraint made executable:**
  1. File absent → **greenfield**: write `seed` if given, else `serde_json::to_string_pretty(&{})`;
     then apply the edit to *that*. Action = `Created`. (Greenfield is the **only** sanctioned
     `to_string_pretty` — the TS byte contract for a new file is `JSON.stringify(obj, null, 2) +
     '\n'`, so the greenfield emission ends with **exactly one** `\n`.)
  2. File present → read bytes → **`prove_lossless_roundtrip`**. On `Err`: **do not write.** Record
     the note `` `{path}: unrecognized syntax — left untouched` ``, return `Kept`.
  3. Read the value currently at `key_path`. If it **`json_deep_equal`s** the desired value →
     return `Unchanged` **without opening the file for writing**. (The file's mtime must not move.
     A test asserts the mtime.)
  4. Else mutate the CST (`object_value_or_set` down the path, then `append` or `prop.set_value`)
     → `root.to_string()` → `atomic_write`. Action = `Updated`.
- [ ] **Unparseable ≠ lossy.** Two different failures, two different handlings, and conflating them
  is a data-loss bug: **unparseable** JSON (`read_json_file`'s case) is backed up to `<p>.backup`
  and treated as `{}` — we **overwrite** it, having saved the original. **Lossy-but-parseable** (the
  CST parses but does not round-trip) is **left alone entirely** — we do *not* overwrite, because we
  cannot promise to put it back. Both produce a note; neither produces an error. Test both.
- [ ] **Key order and comments survive an edit to a NEIGHBORING key.** The fixture: three sibling
  MCP servers with a `// keep me` comment between two of them, key order `zeta, alpha, mu`. After
  `upsert_key_path(["mcpServers","selene"], …)`: the comment is still between the same two servers,
  the three siblings are still in `zeta, alpha, mu` order, and `selene` is **appended last**. Assert
  the **bytes** of the region outside our key, not a parsed `Value`.
- [ ] **Trailing commas.** A file with a trailing comma parses (`allow_trailing_commas`), and after
  our edit the trailing comma is **still there** (`uses_trailing_commas()` — the CST handles it).
  ⚠ This is one of the two fixtures the gate names explicitly. If the CST normalizes it away,
  `prove_lossless_roundtrip` catches it at step 2 and we refuse to write — **which is the correct
  behavior, and the test must assert whichever it is.** Do not paper over it.
- [ ] **CRLF and indentation are the FILE's, not ours.** `root.newline_kind()` and
  `root.single_indent_text()` infer them; a 4-space-indented CRLF file gets a 4-space-indented CRLF
  insertion. Test with a tab-indented file too.
- [ ] **⚠ This is a deliberate improvement over the TS, and it must be flagged as such.** The map
  says: *"`writeJsonFile` **re-serializes the whole file** … Claude/Cursor/Gemini/Kiro/Antigravity
  JSON edits are **not** format-preserving … only opencode (JSONC) is surgical. **Port that
  asymmetry as-is.**"* **This plan does not port the asymmetry** — it uses the CST for *all* JSON,
  because (a) the roadmap's Phase 7 gate demands neighbor preservation and a whole-file
  re-serialize of `~/.claude.json` **loses the user's key order and any comment**, and (b) the
  surgical writer must exist for opencode anyway, so the asymmetry buys nothing but a second code
  path and a worse guarantee. **This is Open Question 2** — the maintainer adjudicates. If the
  ruling is "port the asymmetry", this task adds a `write_json_file` (whole-file re-serialize,
  2-space + `\n`) and the five plain-JSON targets call it instead; everything else in the plan is
  unchanged.
- [ ] TDD, **real files in a `TempDir`** (never a mock FS): each of the four algorithm steps; the
  mtime-unchanged assertion on `Unchanged`; the three-sibling + comment + trailing-comma fixture
  byte-asserted; a lossy file left untouched; a corrupt file backed up **with its original bytes**;
  `append_to_string_array` dedupes (`mcp__selene__*` twice ⇒ one entry);
  `remove_from_string_array_by_prefix("mcp__selene__")` deletes the emptied `allow`, then the
  emptied `permissions`.
- [ ] Commit: `feat(installer): the surgical JSON/JSONC writer — comments, key order, formatting preserved`

### Task 5: `toml_write.rs` — the Codex TOML writer

**Files:** Create: `crates/selene-installer/src/toml_write.rs`,
`crates/selene-installer/tests/toml_write_test.rs`. Modify: `src/lib.rs` (one line).

**Production call site:** the codex target (Task 10) — its `~/.codex/config.toml`. **The only TOML
target.**

**Interfaces (map §`targets/toml.ts`):**
```rust
pub fn upsert_toml_table(content: &str, header: &str, values: &BTreeMap<String, TomlValue>)
    -> Result<(String, TomlAction), LossyFile>;   // TomlAction: Inserted | Replaced | Unchanged
pub fn remove_toml_table(content: &str, header: &str)
    -> Result<(String, TomlAction), LossyFile>;   // TomlAction: Removed | NotFound
pub enum TomlValue { Str(String), StrArray(Vec<String>) }   // ⚠ anything else is a BUG, not a value
```

- [ ] **The block, exact** (map §Wire/contract surfaces, renamed):
  ```toml
  [mcp_servers.selene]
  command = "selene"
  args = ["serve", "--mcp"]
  ```
  `header` = `mcp_servers.selene`. Strings quoted with `"`, escaping **only** `\` and `"`. String
  arrays render as `["a", "b"]` — joined with `", "`. **Any other value type is an error**, not a
  best-effort render (the TS throws; we return `Err(LossyFile)` and the caller notes it).
- [ ] **Use `toml_edit`, not the TS's hand-rolled scanner.** The TS hand-rolls a
  find-the-header-at-BOL / scan-to-the-next-`\n[`-that-is-not-`[[` parser *because TS had no
  format-preserving TOML crate*. We have `toml_edit` 0.25, pinned. It preserves comments, key order,
  whitespace and `[[array-of-tables]]` **by construction**. Port the **contract**, not the
  algorithm. ⚠ Consequence, stated so nobody "fixes" it later: the **byte position** of our inserted
  table may differ from the TS's (which appends at EOF as `trimEnd() + '\n\n' + block + '\n'`).
  **Byte-parity with the TS output is NOT a requirement.** Byte-parity with *the user's untouched
  regions* is. Assert the latter; do not assert the former.
- [ ] **`prove_lossless_roundtrip` for TOML too** — same Global Constraint, same shape:
  `content.parse::<DocumentMut>()?.to_string() == content`, else refuse to write. `toml_edit` is
  format-preserving by design, so this should always hold; the point of the check is the *day it
  doesn't*.
- [ ] **Idempotence is byte-equality of the block**, not deep-equality (map §Idempotence rule: JSON
  deep-equals, **TOML block byte-equals**). If the on-disk `[mcp_servers.selene]` table renders
  byte-identical to the desired block → `Unchanged`, no write.
- [ ] **We own the block** (map §Test coverage 2, and it is documented behavior, not a bug): a
  user-added key **inside** `[mcp_servers.selene]` is **overwritten** on re-install (`Replaced`).
  Test it, and comment it — a future reader will otherwise "preserve" it and break idempotence.
- [ ] **Codex's `created` exception** (map §Idempotence rule): empty content ⇒ `Created`, not
  `Updated` — its `config.toml` is "ours". Port the exception; comment the *why*.
- [ ] TDD (map §Test coverage 2, the TOML unit block): build-block contents; insert-into-empty
  starts with the header; a second upsert is `Unchanged` **and byte-equal**; replace-in-place
  preserves `[other_table]` and `[zzz]` siblings; **`[[foo]]` array-of-tables siblings survive an
  upsert** (the special case the TS scanner needed); remove preserves siblings; remove-missing →
  `NotFound` **and content unchanged**; a value that is neither string nor string-array → `Err`.
- [ ] Commit: `feat(installer): format-preserving TOML table upsert/remove for Codex`

### Task 6: `markdown.rs` + `instructions.rs` — the marker strips and the instructions block

**Files:** Create: `crates/selene-installer/src/{markdown.rs, instructions.rs}`,
`crates/selene-installer/tests/markdown_test.rs`. Modify: `src/lib.rs` (one line).

**Production call site:** claude (`CLAUDE.md`), codex + opencode (`AGENTS.md`), gemini
(`GEMINI.md`) — Task 8/10/11. Cursor/Kiro/Hermes/Antigravity **write no instructions**; they only
**strip legacy blocks**, and they strip them with this same code (Task 9/11).

**Interfaces (map §`instructions-template.ts` + §Marked-section upsert):**
```rust
pub const SELENE_SECTION_START: &str = "<!-- SELENE_START -->";  // ⚠ OPEN QUESTION 1
pub const SELENE_SECTION_END:   &str = "<!-- SELENE_END -->";
pub const SELENE_INSTRUCTIONS_BLOCK: &str = /* exact bytes, markers included — below */;
pub enum SectionAction { Created, Updated, Appended, Unchanged }
pub fn replace_or_append_marked_section(p: &Path, body: &str, start: &str, end: &str)
    -> SectionAction;
pub fn upsert_instructions_entry(p: &Path) -> FileEntry;   // maps Appended → Updated
pub fn remove_marked_section(p: &Path, start: &str, end: &str) -> FileAction; // Removed|NotFound|Kept
```

**The instructions block — EXACT BYTES** (map §Instructions block, with the rename table applied and
**nothing else changed**; the markers are Open Question 1):

```
<!-- SELENE_START -->
## Selene

In repositories indexed by Selene (a `.selene/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): `selene_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them, including dynamic-dispatch hops grep can't follow. Name a file or symbol in the query to read its current line-numbered source. If it's listed but deferred, load it by name via tool search.
- **Shell** (always works): `selene explore "<symbol names or question>"` prints the same output.

If there is no `.selene/` directory, skip Selene entirely — indexing is the user's decision.
<!-- SELENE_END -->
```

⚠ **This block is agent-facing guidance, and the MCP `server-instructions` are supposed to be the
single source of it** (CLAUDE.md invariant). They are not in conflict — this block's job is to tell
an agent that Selene *exists* in a repo it has not connected to over MCP, which is a thing
`server-instructions` structurally cannot do. But it **is** a second copy of the pitch, so: it is
**short by design**, it points at the tool rather than restating its guidance, and it is **ported
verbatim** rather than improved. A well-meant rewrite here is how the tuning is lost.

- [ ] **`replace_or_append_marked_section`, verbatim** (map): file absent → write `body + '\n'` →
  `Created`. Markers found (`find(start)` is `Some` **and** `find(end)` is after it) → if
  `content[start_idx .. end_idx + end.len()]` **byte-equals** `body` → `Unchanged` (no write); else
  splice-replace → `Updated`. No markers → `content.trim_end()` + `"\n\n"` (**only if the existing
  content is non-empty**) + `body` + `'\n'` → `Appended`.
- [ ] **`remove_marked_section`, verbatim** (map): missing file → `Kept`; markers absent → `NotFound`
  (**file untouched**); else `before.trim_end()` + `"\n\n"` (**only if both sides are non-empty**) +
  `after.trim_start()`; **if the result trims to `""` → UNLINK the file**; else write
  `joined.trim() + '\n'` → `Removed`.
- [ ] **The legacy-block self-heal is the reason this is exact.** The TS test harness plants a
  `LEGACY_BLOCK` — **the same markers, older wording** — and asserts install **replaces** it
  (`Updated`) while keeping the user's surrounding content. That only works if the markers match
  what the old install wrote. **This is the entire substance of Open Question 1**: if we ship
  `<!-- SELENE_START -->`, a machine with a CodeGraph block installed keeps it **forever**, because
  nothing will ever match it. Port the fixture; the marker constant is the maintainer's call.
- [ ] TDD: create / replace-legacy-keeping-user-content / append-to-a-file-with-no-markers /
  byte-identical `Unchanged` re-run / remove-strips-only-the-block / **remove-unlinks-a
  block-only file** / remove on a file with no markers is `NotFound` **and byte-untouched** /
  remove on a missing file is `Kept`.
- [ ] Commit: `feat(installer): marked-section upsert/remove + the Selene instructions block`

### Task 7: `yaml_lines.rs` — the Hermes YAML line patcher

**Files:** Create: `crates/selene-installer/src/yaml_lines.rs`,
`crates/selene-installer/tests/yaml_lines_test.rs`. Modify: `src/lib.rs` (one line).

**Production call site:** the hermes target (Task 10) — `$HERMES_HOME/config.yaml`. **The only YAML
target, and the only one whose file we patch by line.**

**No YAML crate — and that is a decision, not laziness.** A `serde_yaml`/`yaml-rust` round-trip
**destroys comments and key order**, which fails neighbor preservation on exactly the file where a
Hermes user keeps their model config. The TS is line-based *for that reason*. Port the line
patcher. (A format-preserving YAML CST does not exist in the Rust ecosystem at a quality we would
bet a user's `config.yaml` on — `yaml-rust2` has no round-trip guarantee. If a task finds one,
that is an Open Question, not a unilateral swap.)

**Interfaces (map §Hermes YAML line patcher):**
```rust
pub fn top_level_range(lines: &[String], key: &str) -> Option<Range<usize>>;
pub fn child_range(lines: &[String], parent: Range<usize>, child: &str) -> Option<Range<usize>>;
pub fn list_child_block(lines: &[String], parent: Range<usize>, child: &str)
    -> Option<(Range<usize>, usize)>;         // (range, item_indent) — #456
pub fn join_lines(lines: Vec<String>) -> String;   // pops trailing empties, appends ONE final '\n'
pub fn upsert_hermes_mcp(content: &str) -> (String, FileAction);
pub fn upsert_hermes_toolset(content: &str) -> (String, FileAction);
pub fn remove_hermes_mcp(content: &str) -> (String, FileAction);
pub fn remove_hermes_toolset(content: &str) -> (String, FileAction);
```

- [ ] **Normalize CRLF/CR → LF, split lines** (map). `top_level_range(key)`: start at the line whose
  `trim()` equals `"{key}:"`; end at the next line matching
  `^[A-Za-z_][A-Za-z0-9_-]*:\s*(?:#.*)?$` (**blank lines are skipped**, not terminators).
  ⚠ **Document the known limitation** (map §Suspected dead/quirky TS): `top_level_range` will
  **not** match a quoted or non-`[A-Za-z_]`-initial top-level key. Acceptable — the TS has the same
  hole — but it must be written down, because the failure mode is silent.
- [ ] **`child_range`**: a 2-space-indented `^  {child}:\s*(?:#.*)?$`, ending at the next `^  \S`.
- [ ] **`list_child_block` (#456) — the PyYAML same-indent list case.** PyYAML writes list items at
  the **same** indent as their key. A line ends the block only when its indent is **< 4** *and* it
  is not `^  - `. `item_indent` is read from the first `^( +)- ` match, **defaulting to 4 spaces**,
  so an insert matches the file's existing style. Without this, `- mcp-selene` gets promoted to
  indent 0 and the user's `cli:` block is corrupted — which is exactly what issue #456 was.
- [ ] **The exact 8-line MCP child** (map §Wire/contract surfaces, renamed):
  ```yaml
    selene:
      command: selene
      args:
        - serve
        - --mcp
      timeout: 120
      connect_timeout: 60
      enabled: true
  ```
  under `mcp_servers:`. Upsert = replace the existing child range, or insert if absent.
- [ ] **The toolset entry**: append `- mcp-selene` at `item_indent` at the end of the
  `platform_toolsets: cli:` block — **unless** a line whose `trim()` is exactly `"- mcp-selene"` is
  already present. Greenfield toolset block, exact:
  ```yaml
  platform_toolsets:
    cli:
      - hermes-cli
      - mcp-selene
  ```
- [ ] **`join_lines`**: pop trailing empty lines, append **one** final `\n`.
- [ ] TDD (map §Test coverage 7): install preserves an existing `model:`, a sibling `other:` server
  and a `discord:` toolset, adds the child + `- mcp-selene`; a second run is `Unchanged` **and
  byte-identical**; uninstall removes **only** the selene bits, keeping an appended `custom:` block;
  **#456**: on a PyYAML same-indent config, `- mcp-selene` lands at the **2-space** indent and **no
  item is promoted to indent 0** (assert `^- browser` never matches), the `cli:` block's ordering is
  intact, and it is idempotent; uninstall reverses cleanly on the PyYAML-style config too.
- [ ] Commit: `feat(installer): Hermes YAML line patcher (PyYAML same-indent lists, #456)`

### Task 8: Target — **claude** (the deepest one: MCP entry, permissions, hooks, CLAUDE.md, legacy migration)

**Files:** Create: `crates/selene-installer/src/targets/claude.rs`,
`crates/selene-installer/tests/target_claude.rs`,
`crates/selene-installer/tests/fixtures/messy-claude.json`. Modify: `src/registry.rs` (+1 row),
`src/targets/mod.rs` (+1 line).

**Production call site:** `ALL_TARGETS[0]` — reachable from `selene install --target claude` the
moment the row lands. Un-ignore the `get_target("claude")` assertion in `registry_test.rs`.

**Claude is 454 LOC in the TS and touches five files. It is one task because it is one target, and
splitting it would split the idempotence contract.** Budget accordingly.

**Paths (map §Config file paths — verbatim):**
| | Global | Local |
|---|---|---|
| MCP entry | `~/.claude.json` | `./.mcp.json` — **never** `./.claude.json` (pre-#207 legacy) |
| permissions + hooks | `~/.claude/settings.json` | `./.claude/settings.json` |
| instructions | `~/.claude/CLAUDE.md` | `./.claude/CLAUDE.md` |
| legacy strip | — | `./.claude.json` (#207) |

**The shared MCP entry** (map §Wire/contract surfaces, renamed) — under `mcpServers.selene`:
```json
{"type":"stdio","command":"selene","args":["serve","--mcp"]}
```

- [ ] **Detection** (map §Auto-detection): global `installed` = `~/.claude` **or** `~/.claude.json`
  exists. `already_configured` = the `selene` key is present in the actual MCP config.
- [ ] **Permissions, exact** (map §Permissions contract): the string `mcp__selene__*` — **exactly
  that, deduped** — appended to `permissions.allow` in `settings.json`. Uninstall filters out
  **every** entry with the prefix `mcp__selene__`, then deletes an emptied `allow`, then an emptied
  `permissions`. (Both are `json_edit`'s `append_to_string_array` /
  `remove_from_string_array_by_prefix` — written in Task 4, called here.) Written **only** when
  `auto_allow`.
- [ ] **Legacy hook cleanup — two-pass, and the second pass is conditional** (map §Claude
  specifics): match any hook whose `command` **contains** the substring `selene mark-dirty` or
  `selene sync-if-dirty` (⚠ **Open Question 3** — whether these are `selene …` or `codegraph …`
  strings). Pass 1: drop matching commands from **every** `hooks.<event>[].hooks[]` group. Pass 2 —
  **only if pass 1 removed something** — prune empty groups → then empty events → then an empty
  `hooks` object. **No match ⇒ the file is byte-untouched and the action is `Unchanged`**; file
  absent ⇒ `NotFound`. The conditionality of pass 2 is load-bearing: an unconditional prune
  rewrites a file we were asked not to touch.
- [ ] **Prompt hook** (tri-state `InstallOptions.prompt_hook`): command substring `selene
  prompt-hook`. `Some(true)` ⇒ append `{"type":"command","command":"selene prompt-hook"}` to
  `hooks.UserPromptSubmit` — **unless any group already contains it** (idempotence: a single entry,
  byte-identical on re-run). `Some(false)` ⇒ strip it (an opt-out that round-trips). `None` ⇒ leave
  it exactly as it is. A **sibling** `UserPromptSubmit` hook is preserved **and ordered**
  `['my-own-hook', 'selene prompt-hook']` — ours appends, it never inserts.
  ⚠ It names `selene prompt-hook`, a subcommand **Phase 6** owns. See Open Question 4.
- [ ] **The `./.claude.json` legacy migration (#207)**: **both** install and uninstall strip a
  `selene` entry from `./.claude.json`, and **delete the file only if that leaves it `{}`**. A
  `somethingElse` key or a sibling server ⇒ the file survives with our key gone. This is the one
  place install *removes* something, and it is why install's `WriteResult` can carry a `Removed`.
- [ ] **`CLAUDE.md`**: `upsert_instructions_entry` (Task 6). Created / legacy-block-replaced /
  `Unchanged` on re-run.
- [ ] **The fixture — `messy-claude.json`, and it is the gate's ammunition:** three sibling MCP
  servers, a top-level `customField`, a realistic nested `projects` history blob, keys in a
  non-alphabetical order. Every claude test runs against **this**, not against `{}`.
- [ ] TDD (map §Test coverage 8 + 13, port every assertion): local writes `./.mcp.json` and **not**
  `./.claude.json`; global targets `~/.claude.json`; CLAUDE.md created, then legacy-replaced; the
  legacy `./.claude.json` migration (deleted when selene-only; **siblings and `somethingElse`
  preserved otherwise**); uninstall strips **both** `.mcp.json` and the legacy file; the hook
  cleanup keeps a **GitKraken Stop hook** and still writes permissions; a sibling command **in the
  same matcher group** survives; **byte-for-byte no-op when there are no selene hooks**; `NotFound`
  when `settings.json` is absent; a re-run after cleanup is byte-identical; uninstall strips the
  `npx …` hook form and removes an emptied `hooks` object; the prompt-hook suite (written with
  permissions / absent when not requested / idempotent single entry / `Some(false)` round-trips the
  opt-out / sibling preserved and **ordered** / `remove_prompt_hook_entry` leaves a **Stop-event**
  legacy hook alone). Plus the three `installer.test.ts` shim assertions, **against the target
  directly**: `install(Local)` creates `.mcp.json`; corrupt JSON ⇒ `.backup` with the original bytes
  + a valid rewrite; existing siblings and `customField` preserved.
- [ ] Commit: `feat(installer): the claude target — MCP entry, permissions, hooks, CLAUDE.md, #207 migration`

### Task 9: Targets — **cursor**, **gemini**, **kiro** (the plain-JSON family)

**Files:** Create: `crates/selene-installer/src/targets/{cursor.rs, gemini.rs, kiro.rs}`,
`crates/selene-installer/tests/targets_json_family.rs`. Modify: `src/registry.rs` (+3 rows, **in
frozen order**), `src/targets/mod.rs` (+3 lines).

**Production call site:** `ALL_TARGETS[1]` (cursor), `[5]` (gemini), `[7]` (kiro). Un-ignore their
three `get_target` assertions.

**They are one task because they are one shape**: a plain-JSON `mcpServers.selene` entry via
`json_edit` (Task 4), plus at most one instructions file and one legacy cleanup. Each is ~150–250
LOC of TS.

**Paths + specifics (map §Config file paths, §Cursor `--path` injection, §Kiro):**

| | Global | Local | Instructions | Cleanup |
|---|---|---|---|---|
| **cursor** | `~/.cursor/mcp.json` | `./.cursor/mcp.json` | **none** | `./.cursor/rules/selene.mdc` |
| **gemini** | `~/.gemini/settings.json` | `./.gemini/settings.json` | `~/.gemini/GEMINI.md` (g) / `./GEMINI.md` (l — **project root, NOT under `.gemini/`**) | none |
| **kiro** | `~/.kiro/settings/mcp.json` | `./.kiro/settings/mcp.json` | **none** | `.kiro/steering/selene.md` |

- [ ] **Detection**: cursor = `~/.cursor` (global) / `./.cursor` (local) exists. gemini = `~/.gemini`
  exists. kiro = `~/.kiro` exists. `already_configured` = the `selene` key is in the actual config.
- [ ] **Cursor's `--path` injection** (map): its entry is the shared config **plus** two extra args:
  `args: ["serve","--mcp","--path", <p>]` where `<p>` = **`ctx.cwd()` as an absolute path** when
  `Local`, and the **literal string `${workspaceFolder}`** when `Global`. (Not a resolved path — the
  literal, for Cursor to expand.)
- [ ] **Cursor's rules cleanup (#529)** — the installer **no longer writes** `selene.mdc`; it only
  **removes leftovers**, and the removal is precise: if the marked block is present, strip it; if
  the remainder is `""` **or equals the exact frontmatter**, delete the file; a **pristine
  frontmatter-only** file is also deleted; **foreign content ⇒ `NotFound`, file untouched.** The
  frontmatter, exact (map §Cursor, renamed — `MDC_FRONTMATTER` exists **only** for this matching
  now, so an inexact port means leftover files are never recognized as ours):
  ```
  ---
  description: Selene MCP usage guide — when to use which tool
  alwaysApply: true
  ---
  ```
- [ ] **Gemini's local GEMINI.md is at the PROJECT ROOT**, not under `.gemini/`. This is not a typo
  in the map; it is the contract. A test asserts the path.
- [ ] **Gemini: the user's `security.auth.selectedType: "oauth-personal"` survives install AND
  uninstall.** This is the map's named test and it is the neighbor-preservation canary for this
  target — a whole-file re-serialize would keep the *value* but could reorder or reformat it; the
  CST keeps the **bytes**.
- [ ] **Kiro owns `.kiro/steering/selene.md` outright** (#529): install **self-heals by deleting it**
  (it is no longer written); uninstall deletes it **unconditionally**. **Sibling steering files
  (`product.md`) are untouched** — assert it.
- [ ] **Kiro's two notes, verbatim** (map §printConfig): `Restart Kiro for MCP changes to take
  effect.` **and** `Kiro IDE: also enable MCP in Settings (search "MCP" → "Enabled"). Kiro CLI users
  can skip this step.` **Cursor's note, verbatim:** `Restart Cursor for MCP changes to take effect.`
- [ ] TDD (map §Test coverage 4, 5, 12): gemini's settings entry deep-equals
  `{type:"stdio",command:"selene",args:["serve","--mcp"]}`; the `oauth-personal` survival, both
  ways; gemini local writes `./.gemini/settings.json` + `./GEMINI.md`; uninstall strips a leftover
  GEMINI.md block keeping user content. Kiro: install writes the entry and **no** steering doc;
  install **deletes** a leftover steering file (reported `Removed`); a sibling MCP server survives
  install **and** uninstall; `product.md` untouched. Cursor: uninstall deletes a leftover
  `selene.mdc` **entirely, frontmatter and all**; install **self-heals** it (`Removed`); **user
  content outside the markers is preserved** (block-only strip); the `--path` arg is `ctx.cwd()`
  locally and the literal `${workspaceFolder}` globally.
- [ ] Commit: `feat(installer): cursor, gemini, kiro targets`

### Task 10: Targets — **codex** + **hermes** (the global-only, non-JSON pair)

**Files:** Create: `crates/selene-installer/src/targets/{codex.rs, hermes.rs}`,
`crates/selene-installer/tests/targets_codex_hermes.rs`,
`crates/selene-installer/tests/fixtures/{messy-codex.toml, messy-hermes.yaml}`. Modify:
`src/registry.rs` (+2 rows), `src/targets/mod.rs` (+2 lines).

**Production call site:** `ALL_TARGETS[2]` (codex), `[4]` (hermes). Un-ignore both `get_target`
assertions.

**They are one task because they are the two consumers of Task 5 and Task 7** — the writers exist;
this is the target shell around them. Both are **global-only**.

- [ ] **codex** (global-only): `~/.codex/config.toml` (via `toml_write`, Task 5) **and**
  `~/.codex/AGENTS.md` (via `markdown`, Task 6). Detection: `~/.codex` exists;
  `already_configured` = the config text **contains the substring** `[mcp_servers.selene]` (map —
  a substring check, not a parse; port it).
- [ ] **hermes** (global-only): `${HERMES_HOME:-~/.hermes}/config.yaml` (via `yaml_lines`, Task 7).
  **No instructions file.** Detection: `$HERMES_HOME` (default `~/.hermes`) **or** its `config.yaml`
  exists; `already_configured` = a **line-parse** finds `mcp_servers:` with a `selene:` child (map —
  again, not a YAML parse).
- [ ] **The global-only refusal is success-shaped, and its bytes are contracts.** A `Local` install
  on either returns `WriteResult { files: [], notes: [<the note>] }` — **an empty file list, never
  an error.** The note must contain `no project-local config` / `--location=global` (map §printConfig
  + §Test coverage 6). `print_config(Local)` returns `# <Name> … use --location=global.\n`.
  Hermes' note, verbatim: `Start a new Hermes session for MCP changes to take effect.`
- [ ] **The fixtures are the point of this task.** `messy-codex.toml`: `[other_table]`, **two
  `[[array_of_tables]]` entries**, an inline comment on a value, and a `[zzz]` table positioned
  **after** where ours lands. `messy-hermes.yaml`: **PyYAML same-indent lists** (#456), a `model:`
  key, a sibling `other:` MCP server, a `discord:` toolset, and a user-appended `custom:` block at
  the end. Every assertion in this task runs against these, not against an empty file.
- [ ] TDD (map §Test coverage 2 + 7): codex install writes `config.toml` **and** the AGENTS.md block
  (contains `## Selene` and `selene explore`); re-run all-`Unchanged`; a legacy AGENTS.md block is
  replaced **keeping user content** (`Updated`); a user key added **inside** our TOML block is
  overwritten on re-install (`Updated` — we own the block, documented). Hermes: the full §7 suite
  from Task 7, now **through the target** rather than the patcher — install adds the child +
  `- mcp-selene` while preserving `model:`/`other:`/`discord:`; second run `Unchanged`; uninstall
  keeps the appended `custom:` block; the #456 PyYAML case end-to-end.
- [ ] Commit: `feat(installer): codex (TOML) and hermes (YAML) targets — global-only`

### Task 11: Targets — **opencode** + **antigravity** (the two with a second, legacy path)

**Files:** Create: `crates/selene-installer/src/targets/{opencode.rs, antigravity.rs}`,
`crates/selene-installer/tests/targets_opencode_antigravity.rs`,
`crates/selene-installer/tests/fixtures/messy-opencode.jsonc`. Modify: `src/registry.rs` (+2 rows),
`src/targets/mod.rs` (+2 lines).

**Production call site:** `ALL_TARGETS[3]` (opencode), `[6]` (antigravity). Un-ignore both — and
with them, **`get_target` now returns `Some` for all 8 ids and the registry-completeness test goes
green.** That transition is the phase's "the registry is whole" signal; the gate (Task 13) assumes
it.

**They are one task because they share the one shape nothing else has: a *second, legacy* location
that install must sweep.** Getting this wrong leaves a stale entry in a file the user still loads.

**opencode** (map §JSONC surgical editing, §Config file paths, §Test coverage 3):
- [ ] **Paths**: global `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.jsonc|.json` +
  `…/AGENTS.md`; local `./opencode.jsonc|.json` + `./AGENTS.md`. **File choice, in order:** an
  existing `opencode.jsonc` **>** an existing `opencode.json` **>** a new `opencode.jsonc`.
- [ ] **The entry** (map §Wire/contract surfaces, renamed) — under `mcp.selene`, note the **array**
  command and the `"local"` type, which no other target uses:
  ```json
  {"type":"local","command":["selene","serve","--mcp"],"enabled":true}
  ```
  plus a top-level `"$schema": "https://opencode.ai/config.json"`. **Greenfield seed text, exact:**
  `{\n  "$schema": "https://opencode.ai/config.json"\n}\n`. If `$schema` is missing from an existing
  file, it is **added first**, then the `mcp.selene` entry.
- [ ] **Removal prunes the wrapper**: remove `mcp.selene`; **if `mcp` is then an empty object, remove
  `mcp` too** (`json_edit::remove_key_path(prune_empty_parent: true)` — Task 4 built this for
  exactly here).
- [ ] **The `%APPDATA%` legacy sweep (#535) — env-gated, NOT `cfg!(windows)`.** The map is explicit
  and the reason is testability: the gate must exercise this on macOS/Linux. When `APPDATA` is set
  **and differs from the real config dir**, sweep `%APPDATA%/opencode/{opencode.jsonc,
  opencode.json, AGENTS.md}`. **Install self-heals**: it strips the legacy entry (preserving that
  file's siblings **and comments**) and **unlinks an emptied legacy AGENTS.md**, reporting both as
  `Removed`. **Uninstall sweeps the legacy path even with no prior install.** A **second** install
  reports **no** legacy changes (it already healed them). A legacy-only dir ⇒ `detect.installed` is
  **true** but `already_configured` is **false** (it reads the **real** path only, never the legacy
  one — map §Auto-detection).
- [ ] **The fixture — `messy-opencode.jsonc`, the gate's headline case:** a `//` line comment, a
  `/* */` block comment, **a trailing comma**, and **three sibling MCP servers**. After install:
  comments intact, siblings intact, trailing comma intact, and a re-run is **byte-identical
  `Unchanged`**.

**antigravity** (map §Antigravity path pick — global-only):
- [ ] **The path pick, exact**: use the **unified** `~/.gemini/config/mcp_config.json` if
  `~/.gemini/config/.migrated` exists **OR** the unified file exists; **else** the **legacy**
  `~/.gemini/antigravity/mcp_config.json`. Install writes to the **preferred** path; when the
  preferred path is the unified one, it **also strips a selene entry from the legacy one**
  (the migration). Uninstall sweeps the **preferred** path, then the **other** — the other is
  reported **only when it was actually `Removed`**.
- [ ] **The entry has NO `type` field** — `{"command":"<resolved-or-selene>","args":["serve","--mcp"]}`.
  It is the one target whose entry shape differs by omission, and a test asserts the **absence** of
  the key.
- [ ] **The darwin absolute-path resolution** (map, and **Open Question 5**): on **macOS only**,
  `command` is resolved by running `command -v selene || which selene` under `/bin/bash`, to an
  absolute **existing** path, falling back to the bare `selene`. **All failures swallowed.**
  ⚠ This makes install **non-deterministic across machines** (an absolute path is baked into JSON),
  which collides with this plan's determinism constraint. The map says it is by design:
  `json_deep_equal` idempotence still holds **per machine**, and re-running after moving the binary
  reports `Updated`. Port it, comment it, and — because the gate must be hermetic — **make the
  resolver a `Ctx`-injected function** so tests pin it to a fixed value.
- [ ] **Empty `{}` files are LEFT IN PLACE on uninstall** for antigravity **and** gemini — they
  manage/share those files. (Contrast: claude's legacy `./.claude.json` **is** deleted when emptied.
  The asymmetry is real; port it.)
- [ ] **Detection**: `~/.gemini/config` **or** `~/.gemini/antigravity` **or** the preferred file
  exists. Note, verbatim: `Restart Antigravity for MCP changes to take effect.`
- [ ] TDD (map §Test coverage 3 + 6, port every assertion): opencode prefers `.jsonc` over `.json`
  (and leaves the `.json` **untouched**); uses `.json` when only it exists; a fresh install creates
  `.jsonc` (`Created`); **comments survive and the re-run is byte-identically `Unchanged`**; the
  AGENTS.md block created / legacy-replaced / uninstall-stripped keeping user text; local install
  writes `./opencode.jsonc` + `./AGENTS.md`; uninstall removes **only** `mcp.selene`, keeping
  comments and siblings; the **whole #535 suite** (global install lands in XDG and **never** in
  `%APPDATA%`; greenfield targets XDG even when the dir is absent; `XDG_CONFIG_HOME` is honored; the
  self-heal preserves legacy siblings/comments and unlinks the emptied legacy AGENTS.md, **both
  reported `Removed`**; uninstall sweeps legacy with no prior install; a second install reports no
  legacy changes; a legacy-only dir ⇒ `installed: true, already_configured: false`).
  Antigravity: legacy path when there is no marker (**and `~/.gemini/settings.json` is untouched**);
  unified path when `.migrated` is present (**legacy untouched**); unified when the unified file
  exists without the marker; the entry has **no `type`**, **has `command`**, and
  `args == ["serve","--mcp"]`; install **migrates** the legacy entry out when the marker appears; a
  sibling server is preserved (legacy path); an **Antigravity-managed `disabled: true`** on a
  sibling is preserved; uninstall removes only selene; uninstall sweeps **both** paths; `Local` is
  rejected with `files: []` + a note matching `/no project-local config/`; it **never writes
  GEMINI.md**; **gemini and antigravity coexist, and uninstalling one leaves the other**.
- [ ] Commit: `feat(installer): opencode (JSONC + %APPDATA% sweep) and antigravity (unified/legacy) targets`

### Task 12: The interactive flow, `print_config`, `describe_paths` — and the ledger pass

**Files:** Modify: `crates/selene-installer/src/flow.rs` (fill the Task-3 stubs — **strictly after
Task 3, and after every target**), `src/lib.rs` (facade + ledger pass), root `Cargo.toml` (+
`inquire`), `crates/selene-installer/Cargo.toml`. Create:
`crates/selene-installer/tests/print_config_test.rs`.

**Production call site:** `selene install` with **no** `--yes` and no `--target` — the default,
human path. Task 3 made the flagged path work; this makes the *default* path work.

**NEW DEPENDENCY — the only one in the phase, and here is its justification:**
`inquire` (or `dialoguer` — either is acceptable; pick one and pin it). The TS uses `@clack/prompts`
for a **multiselect pre-checked with the detected targets**, a **select**, and **confirms**. The
roadmap's UX pins (`indicatif` 0.18, `crossterm` 0.29) cover progress bars and raw terminal control,
**neither of which is a prompt**. Building a multiselect on raw `crossterm` is ~300 LOC of TTY
handling that is not this project's problem. **One line of justification, as required:** *a
pre-checked multiselect + select + confirm is the entire interactive surface, and no pinned crate
provides it.* ⚠ If the maintainer would rather ship **non-interactive only** (`--target` + `--yes`
are already complete after Task 3), this task shrinks to `print_config`/`describe_paths`/the ledger
and the dep disappears — see **Open Question 6**.

- [ ] **Fill the Task-3 stubs, in the order Task 3 laid down. Do not re-order the flow.**
  Step 1 — multiselect over `ALL_TARGETS` **in frozen order**, **pre-checked with the detected
  ones** (falling back to an initial `['claude']` when nothing is detected), each labelled exactly
  `"<displayName> (detected|not found)[ — global only]"`.
  Step 3 — the location select, **skipped entirely (forced `Global`) when every selected target is
  global-only** — codex, hermes and antigravity are, so a codex-only install must never ask.
  Step 4 — the `auto_allow` confirm, **shown only when claude is selected**; otherwise `false`
  without a prompt.
  Step 4¾ — the prompt-hook confirm (**default yes**), **only when claude is selected**.
- [ ] **The per-file log line, and the TS's cosmetic bug that we FIX** (map §Suspected dead/quirky
  TS): the TS verb map is `unchanged→Unchanged, created→Created, removed→Removed, else Updated` —
  so `kept` and `not-found` both render as **"Updated"**, which is a lie about a file we did not
  touch. **Render them honestly** (`Kept`, `Not found`). This is the one place this plan knowingly
  diverges from TS behavior, it is cosmetic, it is in the *output* and not the *contract*, and it is
  written down here so it is not mistaken for a port error. Paths are **tildified** for display.
- [ ] **`print_config` MUST NOT touch the filesystem.** The map states it, and the contract suite
  **proves** it with a full before/after byte-listing of home **and** cwd (`snapshot_tree`, Task 2).
  Format, exact: `# Add to <path>\n\n<2-space JSON or TOML snippet>\n`. A **global-only** target
  asked for `Local` returns `# <Name> ... use --location=global.\n`.
- [ ] **`--print-config <id>`** (map §cli-daemon-sync §128 — note it takes **a target id**, it is not
  a bare boolean) prints that target's `print_config` and **exits 0 without writing anything.** It is
  the escape hatch for a user who wants to paste it themselves, and the cheapest possible way for a
  nervous user to trust this tool. An unknown id here is the same `Err` as `--target` (exit 1).
- [ ] **Ledger pass on `src/lib.rs`** (house rule): the crate's role + PRD section; the
  **public-interface ledger** (map §Public interface → Rust item, or *"deferred → Phase N,
  because …"*: `runInstaller`'s telemetry hooks, `offerWatchFallback`, the npm offer, the
  `config-writer.ts` shim and `clack.d.ts` all land here as **explicit, reasoned deferrals**); the
  four contract properties restated where a maintainer will actually read them; and the
  **`prove_lossless_roundtrip` rule**, stated as the crate's prime directive.
- [ ] TDD: `print_config` writes **nothing** (byte-listing before/after, for all 8 targets × both
  locations — 13 supported pairs + the unsupported ones); the global-only forcing (a codex-only
  selection never prompts for location); the verb map renders `Kept`/`NotFound` honestly; `--print`
  exits 0 and the tree is unchanged.
- [ ] Commit: `feat(installer): interactive flow, print_config, describe_paths + the crate ledger`

### Task 13: **THE PHASE 7 GATE** — the ~97 contract tests, on real files, byte-for-byte

**Files:** Create: `crates/selene-installer/tests/targets_contract.rs` (the suite),
`crates/selene-installer/tests/cli_gate.rs` (the binary-driving half),
`docs/benchmarks/2026-07-phase7-installer.md`. Modify: `crates/selene-installer/tests/fixtures/`
(finish the corpus). **Requires every prior task.**

This is the roadmap's Phase 7 gate: *"all ~97 contract tests ported and green (idempotence, neighbor
preservation, reversible uninstall, byte-equal re-run ⇒ `unchanged`)."*

**⚠ Port by ASSERTION, not by count.** The map is explicit: CLAUDE.md says "~97"; the TS file has
**95 static `it()` blocks** which expand to **~155 runtime cases** (5 parameterized contract tests ×
13 target/location pairs + 90 singles). **Chasing the number 97 is how a suite ends up with 97
shallow tests and no gate.** The deliverable is: every assertion in
`__tests__/installer-targets.test.ts` and `__tests__/installer.test.ts` has a Rust counterpart, and
the count is *recorded* (in the benchmark doc), not *targeted*.

**The 13 supported (target × location) pairs** — the parameterized contract's matrix:
`claude×{global,local}`, `cursor×{global,local}`, `codex×{global}`, `opencode×{global,local}`,
`hermes×{global}`, `gemini×{global,local}`, `antigravity×{global}`, `kiro×{global,local}`.

- [ ] **The harness is real files in a temp dir. Not a mock FS.** `TempDir` home + `TempDir` cwd, a
  `Ctx` over them, `APPDATA` and `XDG_CONFIG_HOME` set (→ `<home>/.config`), `HERMES_HOME`
  **absent** — mirroring the TS harness (which swaps `HOME`/`USERPROFILE` and `process.chdir`s).
  ⚠ **Because `Ctx` is injected (Task 2), these tests parallelize.** A `set_current_dir`-based port
  would have to be `--test-threads=1`, and would still be racy. That is why `Ctx` exists.
- [ ] **The parameterized contract — 5 tests × 13 pairs.** For each pair:
  1. **install writes files and flips `already_configured` false→true**;
  2. **re-run install ⇒ EVERY action is `Unchanged`** — *and* (this plan adds the assertion the TS
     only implies) **the file bytes are identical to after run 1**. The reported action **and** the
     bytes. Either alone is a hole: bytes-only misses a silent rewrite that lands on the same
     content; action-only misses a rewrite that changes the bytes;
  3. **a sibling MCP server planted first survives** (opencode uses its `mcp`-shaped seed);
  4. **uninstall flips `already_configured` back to false**;
  5. **`print_config` is non-empty and writes NOTHING** — asserted with a full before/after
     byte-listing of home **and** cwd.
- [ ] **THE FOUR PROPERTIES, asserted BYTE-FOR-BYTE against the messy corpus.** This is the gate
  proper, and it runs over **every** target that has a fixture:
  ```
  before  = snapshot_tree(home) + snapshot_tree(cwd)     // the messy, comment-laden originals
  install(ctx, loc, opts)                                 // run 1
  after_1 = snapshot_tree(...)
  install(ctx, loc, opts)                                 // run 2 — IDEMPOTENCE
  assert!(every FileAction == Unchanged)                  //   …reported
  assert_eq!(snapshot_tree(...), after_1)                 //   …and byte-identical
  uninstall(ctx, loc)                                     // REVERSIBILITY
  assert_eq!(snapshot_tree(...), before)                  //   …byte-for-byte, the whole tree
  ```
  ⚠ The final `assert_eq!` is the strongest line in this plan. It says: **after install-twice-then-
  uninstall, every byte of every file — the user's comments, their trailing comma, their key order,
  their three other MCP servers, their indentation, their line endings — is exactly as we found it.**
  Where a target legitimately cannot reverse (map: antigravity/gemini leave an emptied `{}` file in
  place; claude's legacy `./.claude.json` **is** deleted when emptied), the exception is **declared
  in the test as a named, commented allowance** — not silently tolerated by a weaker assertion.
- [ ] **The messy corpus — this is the ammunition, and the gate is only as good as it is.** Every
  fixture is a config a real user would actually have:
  - `messy-claude.json` — 3 sibling MCP servers, a `customField`, a nested `projects` history, keys
    out of alphabetical order;
  - **`messy-opencode.jsonc` — 3 sibling MCP servers, a `//` line comment, a `/* */` block comment,
    and a TRAILING COMMA** (the case the whole JSONC writer exists for);
  - `messy-codex.toml` — `[other_table]`, two `[[array_of_tables]]`, an inline comment, a `[zzz]`
    after our block;
  - `messy-hermes.yaml` — PyYAML same-indent lists (#456), `model:`, a sibling `other:` server, a
    `discord:` toolset, a user-appended `custom:` block;
  - `corrupt.json` — unparseable. **Must be backed up to `.backup` with its ORIGINAL BYTES**, never
    lost. (The one case where "reversible" means "recoverable".)
  - a **CRLF** variant and a **tab-indented** variant of one JSON config — because line endings and
    indentation are the two things a naive writer silently normalizes.
- [ ] **The registry-completeness assertion goes green here** (it was written failing in Task 3):
  `get_target` returns `Some` for **all 8** ids, `None` for an unknown one; `resolve_target_flag`
  handles `none` / `all` / CSV order; the unknown-id `Err` message matches **exactly**.
- [ ] **The sweeps** (map §Test coverage 10 + 11): `uninstall_targets` — all-installed ⇒ every report
  `Removed` with paths, and `already_configured` false afterward; a clean slate ⇒ **all
  `NotConfigured` with empty paths and NOT an error**; only-configured agents report `Removed`; a
  **local** sweep marks the global-only targets `Unsupported` with a note matching `/global-only/`; a
  second sweep is all-`NotConfigured`; a `--target` subset removes **only** the chosen ones and the
  siblings stay configured. `refresh_targets` — rewrites a stale block (`Refreshed`, `changed_paths`
  contains `CLAUDE.md`); **never first-installs**; **never rewrites user-trimmed permissions**; a
  second sweep is all-`Unchanged`.
- [ ] **⚠ THE BINARY HALF — `cli_gate.rs`. Without it, this is a library nobody invoked.** Four seams
  have shipped in this project with green unit tests and **zero production callers**. A library-only
  gate is precisely that bug: `selene-installer` passing its own tests while `selene install` is
  broken or unwired. So: **shell out to the real binary** (`cargo run -p selene -- install …`) with
  `HOME` and the cwd pointed at a `TempDir`, and assert on the **files it actually wrote**:
  - `selene install --target claude --location local --yes` → `./.mcp.json` exists with
    `mcpServers.selene`, exit **0**;
  - **run it again** → exit 0, the file is **byte-identical**, and stdout reports **`Unchanged`**;
  - `selene uninstall --target claude --location local --yes` → the tree is **byte-identical to
    before the first install**;
  - `selene install --target bogus` → exit **1**, and stderr is the exact `Unknown --target id(s):`
    message;
  - `selene install --target none --yes` → exit 0, **nothing written**;
  - `selene install --print-config kiro` → exit 0, **nothing written**, stdout is
    `# Add to <path>\n\n<snippet>\n`;
  - `selene install --location sideways --yes` → exit **1** (the second and last exit-1 path).
  ⚠ **`--yes` is mandatory in every CLI invocation here** — an interactive prompt in a test harness
  hangs CI, and a hung gate gets disabled, and a disabled gate is no gate.
- [ ] **A NEGATIVE CONTROL, because a gate that cannot fail certifies nothing.** Add a test that
  **deliberately breaks neighbor preservation** — a `#[test]` that writes one config with a naive
  `serde_json::to_string_pretty` round-trip instead of the CST — and **asserts that the byte-equality
  assertion FAILS on it**. If the naive writer passes the gate, the gate is measuring nothing, and
  we would rather find that out here than from a user whose `~/.claude.json` came back reformatted.
- [ ] **Record the results** in `docs/benchmarks/2026-07-phase7-installer.md`: the assertion count
  (ported vs. the TS's 95 static / ~155 runtime — **and any assertion deliberately NOT ported, with
  its reason**), the 13 pairs × 5 contract tests, the four properties per target, the fixtures, and
  every **declared reversibility exception** (antigravity/gemini's empty `{}`; claude's legacy
  delete). That exception list is the honest part of the gate, and it is what a future reader will
  need most.
- [ ] Commit: `test(installer): PHASE 7 GATE — ~97 contract tests, byte-for-byte, on real files`

---

## Definition of done

- [ ] Tasks 1–13 committed; `cargo fmt && cargo clippy --all-targets && cargo test` green.
- [ ] **The gate (Task 13)**: every assertion from `installer-targets.test.ts` (95 static / ~155
      runtime) + `installer.test.ts` (3) has a Rust counterpart, green, **against real files**; the
      four properties are asserted **byte-for-byte**; the negative control **fails** as designed.
- [ ] **`selene install` and `selene uninstall` work from the real binary** — proven by `cli_gate.rs`,
      not by a library test. All 8 targets are in `ALL_TARGETS`, in the frozen order, and
      `get_target` returns `Some` for each.
- [ ] **No config file is ever written without `prove_lossless_roundtrip` passing on it first.** A
      grep for `atomic_write` shows every call site downstream of that proof.
- [ ] `crates/selene-installer/src/lib.rs` carries the ledger: every map §Public-interface item →
      a Rust item, or a deferral **with its phase and its reason**.
- [ ] `docs/benchmarks/2026-07-phase7-installer.md` records the assertion count, the ported/skipped
      ledger, and the declared reversibility exceptions.
- [ ] `docs/plans/2026-07-12-selenecode-roadmap.md` Phase 7 row updated to reflect reality.
- [ ] **Every Open Question below has been adjudicated by the maintainer** and the ruling written
      into this plan. ⚠ **Questions 1, 2 and 4 BLOCK implementation** — 1 and 2 change file bytes,
      4 changes what a task may write. The rest can be answered while the early tasks run.

---

## Open questions — for the maintainer. **Nothing below was invented; every one is a place the map
## is silent, ambiguous, or in tension with this project's own constraints.**

> The rule this section exists to honor: *an invented contract that contradicts the TS build costs
> far more than a question.* Each question states what the map says, what it does not say, and a
> **recommendation** — but the recommendation is not the ruling.

### ⚠ BLOCKING — these change the bytes we write. Answer before Task 4/6/8.

**1. The marker strings: `<!-- SELENE_START -->` or `<!-- CODEGRAPH_START -->`?**
The map is explicit that this is a decision and not a detail: *"Marker strings likewise become
`<!-- SELENE_START -->` **or stay CODEGRAPH-compatible if migrating existing installs is a goal** —
decide explicitly; the strip-on-install self-heal logic depends on matching whatever old installs
wrote."* The stakes are concrete and asymmetric: if we ship `SELENE_*`, then **on any machine that
ever ran the CodeGraph installer, the old `<!-- CODEGRAPH_START -->` block stays in `CLAUDE.md` /
`AGENTS.md` / `GEMINI.md` forever** — nothing will ever match it, and no `selene uninstall` will
ever remove it. If we ship `CODEGRAPH_*`, our markers are a lie about a product that no longer
exists, but every legacy block is self-healed on first install.
*Recommendation:* **`SELENE_*`, plus a one-time legacy strip** — the marked-section remover is
already parameterized by `(start, end)`, so install can call it **twice**: once to remove a
`CODEGRAPH_*` block, once to upsert the `SELENE_*` one. That costs ~4 lines, gets clean branding,
and leaves nothing behind. It does mean carrying the two CodeGraph marker constants forever as
**migration-only** strings. If SeleneCode is not intended to land on machines that ran CodeGraph,
say so and the legacy strip disappears entirely.

**2. Do we port the TS's JSON asymmetry — or write ALL JSON surgically?**
The map says: *"`writeJsonFile` **re-serializes the whole file** (2-space indent + trailing
newline): Claude/Cursor/Gemini/Kiro/Antigravity JSON edits are **not** format-preserving (user
formatting/comments in plain JSON files are lost) — only opencode (JSONC) is surgical. **Port that
asymmetry as-is.**"* But the roadmap's Phase 7 gate demands **neighbor preservation**, and a
whole-file re-serialize of `~/.claude.json` **reorders the user's keys and drops any comment** —
that is the gate failing by construction, on the single most valuable file we touch. The asymmetry
existed in TS because TS's surgical editor (`jsonc-parser`'s `modify`) was only wired into opencode;
in Rust the **same CST serves both**, so the asymmetry now buys nothing but a second code path and
a worse guarantee.
*Recommendation:* **write all JSON surgically (Task 4 as planned) and mark this as a deliberate,
documented improvement over the TS.** Greenfield files still emit TS-byte-identical
`to_string_pretty + '\n'`, so nothing regresses. Ruling needed because it is a knowing divergence
from the map, and the map is the parity authority.

**3. The legacy-hook cleanup: whose hooks does it strip — `codegraph`'s or `selene`'s?**
The map ports a cleanup that strips hooks whose `command` contains `codegraph mark-dirty`,
`codegraph sync-if-dirty`, or the `npx @colbymchenry/codegraph …` form. **Selene has never written
any of those** — so a literal rename (`selene mark-dirty`, …) makes the whole two-pass cleanup
**dead code on day one**, matching nothing, ever. The mechanism is a self-heal for hooks *the
installer itself used to write*; Selene has no such history.
*Recommendation:* **follow Question 1.** If we ship the CodeGraph migration path (recommended), the
hook cleanup strips the **`codegraph` substrings** (migration) **and** the `selene` ones (future
self-heal) — the matcher takes a list. If SeleneCode never lands on a CodeGraph machine, the
`codegraph` substrings go away and the cleanup keeps only the `selene` forms, ported as the
mechanism for a hook Phase 6 will write. Either way the two-pass prune logic is ported in Task 8;
only the substring list is in question.

**4. `selene prompt-hook` — Phase 7 writes a hook naming a Phase 6 subcommand.**
Task 8 writes `{"type":"command","command":"selene prompt-hook"}` into `hooks.UserPromptSubmit`. The
`prompt-hook` subcommand is **Phase 6's** (`selene-cli`, roadmap line 118). Phase 7 runs *after*
Phase 6, so in the normal ordering this is fine — but the phases have been executed out of order
before, and **if Phase 7 lands first, `selene install` writes a hook that errors on every prompt the
user submits.** That is a worse first impression than not having the feature.
*Recommendation:* **keep the prompt-hook code (writer + stripper + the tri-state) but gate the
*write* on the subcommand existing** — or simply confirm that Phase 6 lands first, in which case
this is a non-issue and I will delete the question. The **strip** path must ship regardless (an
uninstall must be able to remove a hook a future install wrote).

### Non-blocking — answer while the early tasks run.

**5. Antigravity's darwin `command -v selene` — port the nondeterminism, or drop it?**
The map: on darwin only, the entry's `command` is resolved via `execSync('command -v codegraph ||
which codegraph')` to an absolute path, falling back to the bare name; *"This makes `install`
non-deterministic across machines (absolute path baked into JSON) — `jsonDeepEqual` idempotence
still holds per-machine, but re-running after moving the binary reports `updated` (by design)."*
It collides head-on with this plan's **determinism** constraint.
*Recommendation:* **port it as-is (parity), but inject the resolver through `Ctx`** so the gate can
pin it — which Task 11 already specifies. Flagging it because "by design" in the TS is not
automatically "by design" here, and if the answer is "just write `selene`", Task 11 gets simpler.

**6. Interactive prompts: add `inquire`, or ship non-interactive only?**
After Task 3, `--target` + `--yes` + `--location` make the installer fully usable with **zero** new
deps. Task 12's multiselect/select/confirm needs a prompt crate; the roadmap's UX pins (`indicatif`,
`crossterm`) are a progress bar and a raw-terminal layer, **not** prompts. Meanwhile **Phase 6 owns
"terminal UI"** (roadmap line 122), so the interactive layer arguably belongs there.
*Recommendation:* **add `inquire` in Task 12** (justification: a pre-checked multiselect + select +
confirm is the entire interactive surface, and no pinned crate provides it). Acceptable alternative:
**cut Task 12's prompts, ship flags-only, and let Phase 6's terminal-UI task add them** — the plan is
structured so this costs nothing (Task 3 already lays the flow down; Task 12's stubs simply stay
stubs). Your call on the dep.

**7. Which crate owns the `install` / `uninstall` subcommands?**
This plan wires them into `crates/selene/src/main.rs` (as instructed, and as Phase 5 is doing for
`index` / `serve --mcp`). But the roadmap gives **Phase 6** `selene-cli` with *"all 22 subcommands"*,
and `install`/`uninstall` are two of the 22 (`maps/cli-daemon-sync.md` §128–129). Phase 7 runs after
Phase 6, so by then `main.rs` may be a thin shim over `selene-cli`.
*Recommendation:* **Task 3 puts them wherever the CLI actually lives at execution time** — `main.rs`
if Phase 6 has not landed, `selene-cli` if it has. The requirement that does **not** move: they are
**real subcommands of the real binary**, driven by the gate. Confirm the crate so the task's Files
list is right.

**8. What goes in `command`: the bare name `selene`, or the absolute path of the running binary?**
Every entry we write says `"command": "selene"` — which **assumes `selene` is on the user's `PATH`**.
That assumption was safe for CodeGraph (`npm install -g` put it there, and the installer *offered*
to run it). SeleneCode is a static binary that a user may have `cargo install`ed, downloaded to
`~/bin`, or built into `target/release/`. **If it is not on `PATH`, every config we write points at
a command the agent cannot execute** — and the failure surfaces inside Claude Code as a silent MCP
server that will not start. The map has nothing to say here; it is a Rust-port question that the TS
never had.
*Recommendation:* **write `std::env::current_exe()`'s absolute path when it is resolvable, else the
bare `selene`** — which is, notably, exactly what the TS already does for Antigravity on darwin
(Question 5), so the precedent is in the map. Cost: the same per-machine nondeterminism as Q5,
scoped to `command`. Benefit: the config works on a machine where `PATH` doesn't. This is the
question most likely to decide whether a first-time install *works*, so I did not want to guess.

**9. `uninstall --keep-cli` / `cliFilename` — what CLI binary would we be deleting?**
`maps/cli-daemon-sync.md` §129 gives `uninstall` a `--keep-cli` flag, and the installer map's
`RunUninstallerOptions` carries `keepCli` + `cliFilename` — implying uninstall can **delete an
installed CLI binary**. Neither map says where that binary lives or who put it there (in TS it was
presumably the npm global bin, or a shim the installer wrote). For a Rust binary owned by
`cargo install` / Homebrew / a manual download, **SeleneCode deleting its own executable is a
footgun**, and refusing to is a one-line no-op.
*Recommendation:* **accept `--keep-cli` for CLI compatibility, make it a no-op, and document it** —
`selene uninstall` removes *configurations*, never the binary; removing the binary is the package
manager's job. Confirm, and I will write that into Task 3's flag docs.

**10. ~~The 8 targets' `display_name` and `docs_url`~~ — RESOLVED, no ruling needed.**
The map declares the fields but never the values, so rather than guess I read them from
`../codegraph/src/installer/targets/*.ts` and put the **verbatim table in Task 2**. Recording it
here because it is a live example of why this section exists: my "reasonable defaults" would have
been `Hermes` and `Antigravity`; the actual strings are **`Hermes Agent`** and **`Antigravity
IDE`**. Two user-visible labels, wrong, in a plan that looked complete. *(Left in the list rather
than deleted, so the numbering in any review thread stays stable.)*

**11. Should a bare `selene` (no arguments) launch the interactive installer?**
`maps/cli-daemon-sync.md` §106: *"`process.argv.length === 2` → run interactive installer."* That is
a real, deliberate UX contract — a user who types `codegraph` and hits enter gets onboarded. Phase 5
has already shipped a `main.rs` where a bare `selene` prints clap's help.
*Recommendation:* **defer to Phase 6** (it owns the CLI's top-level behavior) and note it there. It
is out of Phase 7's scope, but it is an installer contract, so it would be lost if nobody wrote it
down — which is why it is here.

---

## ✅ RULINGS — the maintainer's adjudication (2026-07-13). These are binding; the recommendations above are superseded where they differ.

**Q1 — markers: `SELENE_*` + a one-time legacy strip. RULED (by the maintainer).**
SeleneCode **will** land on machines that ran CodeGraph — starting with this one. So: ship
`<!-- SELENE_START -->` / `<!-- SELENE_END -->`, and have `install` call the marked-section remover
**twice** — once with the `CODEGRAPH_*` pair (migration), once to upsert the `SELENE_*` pair. Carry
the two CodeGraph marker constants forever, named `LEGACY_*` and commented as migration-only. The
failure this prevents is concrete: without it a dead `CODEGRAPH_*` block sits wedged in the user's
`CLAUDE.md` forever, matched by nothing and removable by no `selene uninstall`.

**Q2 — write ALL JSON surgically. RULED.** The map says "port the asymmetry as-is", but the
roadmap's Phase 7 gate demands **neighbor preservation**, and a whole-file re-serialize of
`~/.claude.json` reorders the user's keys and drops their comments — that is the gate failing *by
construction*, on the single most valuable file we touch. The TS asymmetry was an artifact of only
wiring `jsonc-parser` into opencode; in Rust the same CST serves every target, so the asymmetry buys
a second code path and a **worse** guarantee. This is a **knowing, documented divergence from the
parity map** — record it in the deviation ledger with this reasoning, and keep greenfield files
byte-identical to the TS (`to_string_pretty` + `\n`).

**Q3 — the hook cleanup strips BOTH substring families.** Follows Q1: the matcher takes a list —
the `codegraph …` forms (migration) **and** the `selene …` forms (future self-heal for hooks Phase 6
will write). Neither list is dead code under this ruling.

**Q4 — non-issue; Phase 6 lands first.** Phase 6 (CLI, incl. `prompt-hook`) is already planned and
executes before Phase 7. Write the hook unconditionally. Keep the **strip** path regardless — an
uninstall must remove a hook a future install wrote.

**Q5 — port the darwin `command -v` resolution as-is, resolver injected through `Ctx`** (as Task 11
already specifies) so the gate can pin it. Parity holds; determinism is preserved *in test*.

**Q6 — use `dialoguer`, do NOT add `inquire`.** Cross-plan catch: Phase 6's plan already pins
`dialoguer` in `selene-cli`'s dependency list for its terminal-UI task. A second prompt crate for
the same three widgets (multiselect / select / confirm) is pure duplication. Phase 6 lands first, so
`dialoguer` is already in the tree when Task 12 runs.

**Q7 — `install`/`uninstall` live in `selene-cli`.** Phase 6 lands first and turns
`crates/selene/src/main.rs` into a ~15-line shim over `selene_cli::run`. Fix Task 3's Files list to
`selene-cli`. The requirement that does not move: they are **real subcommands of the real binary**,
driven by the gate.

**Q8 — write `current_exe()`'s absolute path, falling back to the bare `selene`. RULED, and this is
the one most likely to decide whether a first install actually works.** SeleneCode is a static
binary that may be `cargo install`ed, dropped in `~/bin`, or run from `target/release/` — it is
**not** guaranteed to be on `PATH` the way an `npm -g` shim was. A config naming a command the agent
cannot execute fails *silently*, inside Claude Code, as an MCP server that simply never starts —
precisely the "one bad first impression and the agent abandons the tool" failure this project's
invariants exist to prevent. Accept the per-machine nondeterminism (the TS already accepts exactly
this for Antigravity/darwin — the precedent is in the map). Inject the resolver so the gate pins it.

**Q9 — `--keep-cli` is accepted and is a documented no-op.** `selene uninstall` removes
*configurations*, never the binary. Deleting an executable that `cargo`/Homebrew/the user owns is a
footgun; refusing to is a one-line no-op. Document it in the flag help.

**Q10 — no ruling needed** (the planner already read the verbatim strings out of the TS source).
Left numbered so review threads stay stable.
