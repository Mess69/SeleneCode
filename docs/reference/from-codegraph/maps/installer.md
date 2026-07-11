# INSTALLER Map

## File inventory

| Relative path (under `codegraph/`) | LOC | Responsibility |
|---|---|---|
| `src/installer/index.ts` | 708 | Orchestrator: interactive install/uninstall flows (clack UI), `uninstallTargets`/`refreshTargets` pure sweeps, telemetry hooks, git-hook watch fallback |
| `src/installer/config-writer.ts` | 60 | Deprecated backwards-compat shim → delegates to the Claude target (`writeMcpConfig`, `writePermissions`, `hasMcpConfig`, `hasPermissions`) |
| `src/installer/instructions-template.ts` | 51 | Exports the `<!-- CODEGRAPH_START/END -->` markers and the short marker-fenced `## CodeGraph` instructions block (#704) |
| `src/installer/clack.d.ts` | 50 | Type shim for `@clack/prompts` (ESM-only dep loaded via dynamic import) |
| `src/installer/targets/types.ts` | 113 | `AgentTarget` interface, `Location`, `TargetId`, `DetectionResult`, `WriteResult`, `InstallOptions` |
| `src/installer/targets/registry.ts` | 91 | `ALL_TARGETS` frozen list, `getTarget`, `listTargetIds`, `detectAll`, `resolveTargetFlag` (`auto|all|none|csv`) |
| `src/installer/targets/shared.ts` | 233 | Cross-target helpers: MCP config shape, permissions list, JSON read/atomic-write, deep-equal, marked-section replace/remove/upsert |
| `src/installer/targets/toml.ts` | 154 | Hand-rolled format-preserving TOML table upsert/remove for Codex (`[mcp_servers.codegraph]` only) |
| `src/installer/targets/claude.ts` | 454 | Claude Code target: `.mcp.json`/`~/.claude.json`, `settings.json` permissions + hooks (legacy cleanup, prompt hook), `CLAUDE.md` block, legacy `./.claude.json` migration (#207) |
| `src/installer/targets/cursor.ts` | 247 | Cursor target: `mcp.json` with injected `--path`, `.cursor/rules/codegraph.mdc` cleanup (#529) |
| `src/installer/targets/codex.ts` | 175 | Codex CLI target: `~/.codex/config.toml` (TOML), `~/.codex/AGENTS.md` block; global-only |
| `src/installer/targets/opencode.ts` | 288 | opencode target: `opencode.jsonc`/`.json` via jsonc-parser surgical edits, `AGENTS.md` block, legacy `%APPDATA%` sweep (#535) |
| `src/installer/targets/hermes.ts` | 356 | Hermes target: line-based YAML patcher for `$HERMES_HOME/config.yaml` (`mcp_servers.codegraph` + `platform_toolsets.cli` entry); global-only |
| `src/installer/targets/gemini.ts` | 162 | Gemini CLI target: `.gemini/settings.json` + `GEMINI.md` block (project-root when local) |
| `src/installer/targets/antigravity.ts` | 289 | Antigravity IDE target: unified vs legacy `mcp_config.json`, no `type` field, macOS absolute-path resolution; global-only |
| `src/installer/targets/kiro.ts` | 165 | Kiro target: `.kiro/settings/mcp.json`, deletes owned steering file `.kiro/steering/codegraph.md` |
| `__tests__/installer-targets.test.ts` | 1711 | The contract suite (95 static `it()` blocks; parameterized loop expands to ~155 runtime cases) |
| `__tests__/installer.test.ts` | 104 | Legacy shim tests: `readJsonFile` corruption backup, sibling preservation |

## Public interface

```ts
// targets/types.ts
type Location = 'global' | 'local';
type TargetId = 'claude'|'cursor'|'codex'|'opencode'|'hermes'|'gemini'|'antigravity'|'kiro';
interface DetectionResult { installed: boolean; alreadyConfigured: boolean; configPath?: string }
interface WriteResult {
  files: Array<{ path: string; action: 'created'|'updated'|'unchanged'|'removed'|'not-found'|'kept' }>;
  notes?: string[];
}
interface InstallOptions { autoAllow: boolean; promptHook?: boolean }  // promptHook: true=write, false=strip, undefined=leave
interface AgentTarget {
  readonly id: TargetId; readonly displayName: string; readonly docsUrl?: string;
  supportsLocation(loc: Location): boolean;
  detect(loc: Location): DetectionResult;
  install(loc: Location, opts: InstallOptions): WriteResult;
  uninstall(loc: Location): WriteResult;          // safe on clean slate (returns not-found)
  printConfig(loc: Location): string;             // MUST NOT touch the filesystem
  describePaths(loc: Location): string[];
}

// targets/registry.ts
const ALL_TARGETS: readonly AgentTarget[];  // frozen, order = [claude,cursor,codex,opencode,hermes,gemini,antigravity,kiro]
function getTarget(id: string): AgentTarget | undefined;
function listTargetIds(): TargetId[];
function detectAll(loc: Location): Array<{ target: AgentTarget; detection: DetectionResult }>;
function resolveTargetFlag(value: string, loc: Location): AgentTarget[];  // throws on unknown id

// targets/shared.ts
function getMcpServerConfig(): { type: 'stdio'; command: 'codegraph'; args: ['serve','--mcp'] };
function getCodeGraphPermissions(): string[];  // exactly ['mcp__codegraph__*']
function readJsonFile(p: string): Record<string, any>;                 // {} on missing; backs up unparseable to <p>.backup
function atomicWriteFileSync(p: string, content: string): void;        // tmp `<p>.tmp.<pid>` + rename; mkdir -p parent
function writeJsonFile(p: string, data: object): void;                 // JSON.stringify(data, null, 2) + '\n'
function jsonDeepEqual(a: unknown, b: unknown): boolean;               // key-order-insensitive
function replaceOrAppendMarkedSection(p, body, start, end): 'created'|'updated'|'appended'|'unchanged';
function upsertInstructionsEntry(file: string): { path: string; action: 'created'|'updated'|'unchanged' };  // maps 'appended'→'updated'
function removeMarkedSection(p, start, end): 'removed'|'not-found'|'kept';  // deletes file if emptied

// targets/toml.ts
function serializeTomlTableBody(values: Record<string, string|string[]>): string;  // throws on other types
function buildTomlTable(header: string, values): string;   // `[${header}]\n` + body
function upsertTomlTable(fileContent, header, block): { content: string; action: 'inserted'|'replaced'|'unchanged' };
function removeTomlTable(fileContent, header): { content: string; action: 'removed'|'not-found' };

// targets/claude.ts (extra named exports beyond claudeTarget)
function writeMcpEntry(loc): FileAction;  function writePermissionsEntry(loc): FileAction;
function cleanupLegacyHooks(loc): FileAction;   // strips `codegraph mark-dirty`/`codegraph sync-if-dirty` hooks
function writePromptHookEntry(loc): FileAction; function removePromptHookEntry(loc): FileAction;

// installer/index.ts
interface RunInstallerOptions { target?: string; location?: Location; autoAllow?: boolean; yes?: boolean }
async function runInstaller(): Promise<void>;
async function runInstallerWithOptions(opts: RunInstallerOptions): Promise<void>;
interface RunUninstallerOptions { target?: string; location?: Location; yes?: boolean; keepCli?: boolean; cliFilename?: string }
async function runUninstaller(opts: RunUninstallerOptions): Promise<void>;
type UninstallStatus = 'removed'|'not-configured'|'unsupported';
interface UninstallReport { id; displayName; status; removedPaths: string[]; notes: string[] }
function uninstallTargets(targets, location): UninstallReport[];   // pure, no prompts
type RefreshStatus = 'refreshed'|'unchanged'|'not-configured'|'unsupported';
interface RefreshReport { id; displayName; location; status; changedPaths: string[] }
function refreshTargets(targets, location): RefreshReport[];       // re-install() only where alreadyConfigured; autoAllow:false, promptHook:undefined
async function offerWatchFallback(clack, projectPath, opts?): Promise<void>;
// re-exported shims: writeMcpConfig, writePermissions, hasMcpConfig, hasPermissions, InstallLocation

// instructions-template.ts
const CODEGRAPH_SECTION_START = '<!-- CODEGRAPH_START -->';
const CODEGRAPH_SECTION_END   = '<!-- CODEGRAPH_END -->';
const CODEGRAPH_INSTRUCTIONS_BLOCK: string;  // full block incl. markers, exact text below
```

## Key algorithms & data flow

**`--target` flag resolution** (`resolveTargetFlag`): `'none'` → `[]`; `'all'` → whole registry; `'auto'` → all targets with `detect(loc).installed === true`, falling back to `[claude]` if none detected; otherwise CSV split on `,`, trimmed, empty entries dropped; any unknown id throws `Unknown --target id(s): <bad,...>. Known: claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro, plus 'auto' / 'all' / 'none'.`

**Auto-detection** (`detect`): per-target "installed" heuristic = existence of the agent's config dir or config file (Claude global: `~/.claude` or `~/.claude.json`; Cursor: `~/.cursor` / `./.cursor`; Codex: `~/.codex`; opencode global: XDG dir **or** legacy `%APPDATA%/opencode`; Hermes: `$HERMES_HOME` (default `~/.hermes`) or its `config.yaml`; Gemini: `~/.gemini`; Antigravity: `~/.gemini/config` or `~/.gemini/antigravity` or the preferred file; Kiro: `~/.kiro`). `alreadyConfigured` = codegraph key present in the actual config (Codex: substring `[mcp_servers.codegraph]`; Hermes: line-parse for `mcp_servers:` + child `codegraph:`; opencode: parse and check `mcp.codegraph` — read from the **real** path only, never legacy).

**Interactive install flow** (`runInstallerWithOptions`): (1) resolve targets — explicit `--target` wins, `--yes` → `auto`, else clack multiselect over `ALL_TARGETS` pre-checked with detected ones (fallback initial `['claude']`), label `"<displayName> (detected|not found)[ — global only]"`; (2) unless `--yes`, offer `npm install -g @colbymchenry/codegraph` (execSync, 120 000 ms timeout); (3) location — flag > `--yes`→`global` > if every selected target is global-only force `global` > select prompt; (4) autoAllow — flag > `--yes`→true > confirm only if claude selected, else false; (4½) telemetry opt-in prompt once; (4¾) promptHook — only if claude selected; `--yes`→true, else confirm (default yes); (5) loop targets: skip unsupported location with warn, else `install(location, {autoAllow, promptHook})` and log one line per file: verb map `unchanged→Unchanged, created→Created, removed→Removed, else Updated`, path tildified; (6) telemetry `install` lifecycle with `kind: sawCreated?'fresh':sawUpdated?'upgrade':'reinstall'`; never indexes (that's `codegraph init`).

**Idempotence rule (all targets):** compute desired payload; if on-disk value already deep-equals (JSON) / byte-equals (TOML block, marked section, Hermes lines) → return `action:'unchanged'` **without writing** (file stays byte-identical). `created` means the *file* did not exist before; adding a key to an existing file is `updated`. Exception: Codex uses empty-content⇒`created` (its config.toml is "ours"). All writes are atomic (temp + rename).

**Marked-section upsert** (`replaceOrAppendMarkedSection`): file absent → write `body + '\n'` → `created`. Markers found (`indexOf(start)` != -1 and `indexOf(end)` > startIdx) → if `content[startIdx..endIdx+len]` byte-equals `body` → `unchanged`, else splice-replace → `updated`. No markers → `trimEnd()` existing + `'\n\n'` (if non-empty) + body + `'\n'` → `appended`. **Removal** (`removeMarkedSection`): missing file → `kept`; markers absent → `not-found`; else join `before.trimEnd()` + `'\n\n'` (if both sides non-empty) + `after.trimStart()`; if result trims to `''` → **unlink the file**, else write `joined.trim() + '\n'` → `removed`.

**TOML upsert** (Codex): header line literal `[mcp_servers.codegraph]` found at BOL (start-of-file or after `\n`). Block end = next `\n[` not followed by another `[` (so `[[array-of-tables]]` is skipped and preserved), else EOF. Insert: `trimEnd()` + `'\n\n'` + block + `'\n'`. Replace: `beforeClean('\n+$'→'')` + `'\n\n'` + block + (`'\n\n'` if content after, else `'\n'`) + `afterClean('^\n+'→'')`. Values: strings quoted with `"` escaping only `\` and `"`; string arrays as `[“a”, “b”]` joined `', '`. Anything else throws. Everything outside the block is preserved byte-for-byte.

**JSONC surgical editing** (opencode): read text; parse with `jsonc-parser.parse(text, errors, {allowTrailingComma:true})` (non-object ⇒ `{}`); greenfield seed text is exactly `'{\n  "$schema": "https://opencode.ai/config.json"\n}\n'`; if `$schema` missing, `modify(text, ['$schema'], 'https://opencode.ai/config.json', fmt)` first; then `modify(text, ['mcp','codegraph'], entry, fmt)` + `applyEdits` — formatting options `{ tabSize: 2, insertSpaces: true, eol: '\n' }`. Removal: `modify(...,undefined)`; if `mcp` parses to empty object afterwards, remove the `mcp` wrapper too. Comments/format/key order of untouched keys survive. File choice: existing `opencode.jsonc` > existing `opencode.json` > new `opencode.jsonc`.

**Hermes YAML line patcher:** normalize CRLF/CR→LF, split lines. `topLevelRange(key)`: start at line whose trim equals `key:`; end at next line matching `/^[A-Za-z_][A-Za-z0-9_-]*:\s*(?:#.*)?$/` (blank lines skipped). `childRange`: 2-space-indented `^  <child>:\s*(?:#.*)?$`, ends at next `/^  \S/`. `listChildBlock` (issue #456): treats `  - item` (PyYAML same-indent list style) as part of the block — a line ends the block only when indent < 4 and it isn't `^  - `; reports `itemIndent` from the first `/^( +)- /` match (default 4 spaces) so inserts match existing style. Upsert MCP: replace/insert the exact 8-line child (below); upsert toolset: append `- mcp-codegraph` at `itemIndent` at block end if `line.trim() === '- mcp-codegraph'` not already present. `joinLines` pops trailing empty lines and appends one final `\n`.

**Claude specifics:** local MCP file is `./.mcp.json` (never `./.claude.json` — pre-#207 legacy; install and uninstall both strip a codegraph entry from `./.claude.json`, deleting the file only if that leaves it `{}`). Legacy hook cleanup: match hook `command` containing substring `codegraph mark-dirty` or `codegraph sync-if-dirty`; two-pass — drop matching commands from every `hooks.<event>[].hooks[]` group, then (only if something was removed) prune empty groups → empty events → empty `hooks`; no match ⇒ file byte-untouched, `unchanged`; file absent ⇒ `not-found`. Prompt hook: command substring `codegraph prompt-hook`; write appends `{ hooks: [{ type: 'command', command: 'codegraph prompt-hook' }] }` to `hooks.UserPromptSubmit` unless any group already contains it.

**Cursor `--path` injection:** entry = shared config with `args: ['serve','--mcp','--path', <p>]` where `<p>` = `process.cwd()` absolute path (local) or literal `${workspaceFolder}` (global). Local install/uninstall also removes `.cursor/rules/codegraph.mdc`: if the marked block is present, strip it; if the remainder is `''` or equals the exact frontmatter (`---\ndescription: CodeGraph MCP usage guide — when to use which tool\nalwaysApply: true\n---`) delete the file; a pristine frontmatter-only file is also deleted; foreign content ⇒ `not-found` (untouched).

**Antigravity path pick:** unified `~/.gemini/config/mcp_config.json` if `~/.gemini/config/.migrated` exists **or** the unified file exists; else legacy `~/.gemini/antigravity/mcp_config.json`. Install to preferred; when preferred is unified, also strip a codegraph entry from legacy (migration). Uninstall sweeps preferred, then the other path (reported only when `removed`). Entry has **no `type` field**; on darwin only, `command` is resolved via `execSync('command -v codegraph || which codegraph', {shell:'/bin/bash'})` to an absolute existing path, falling back to bare `codegraph`. Empty `{}` files are left in place on uninstall (Antigravity + Gemini manage/share them).

**Kiro:** owns `.kiro/steering/codegraph.md` outright — install self-heals by deleting it (no longer written, #529); uninstall deletes it unconditionally; sibling steering files untouched.

**refreshTargets:** for each target: unsupported location → `unsupported`; `!detect(loc).alreadyConfigured` → `not-configured` (never a first install); else `install(loc, {autoAllow:false, promptHook:undefined})`; `changedPaths` = files with action created/updated/removed; any → `refreshed`, none → `unchanged`. CLI `install --refresh` sweeps both locations unless `--location` narrows.

**uninstallTargets:** unsupported → note `` `no ${location} config — this agent is ${other}-only` ``; else `uninstall(loc)`; `removedPaths` = actions === `removed`; any → `removed` else `not-configured`.

## Wire/contract surfaces

**Shared MCP entry (Claude/Cursor-base/Gemini/Kiro):** `{"type":"stdio","command":"codegraph","args":["serve","--mcp"]}` under key `mcpServers.codegraph`. Files written as `JSON.stringify(obj, null, 2) + '\n'`.

**Cursor:** same plus `"--path"` + path arg appended to args. **Antigravity:** `{"command":"<resolved-or-codegraph>","args":["serve","--mcp"]}` — no `type`. **opencode:** under `mcp.codegraph`: `{"type":"local","command":["codegraph","serve","--mcp"],"enabled":true}` with top-level `"$schema":"https://opencode.ai/config.json"`.

**Codex TOML block (exact):**
```toml
[mcp_servers.codegraph]
command = "codegraph"
args = ["serve", "--mcp"]
```

**Hermes YAML (exact lines):** under `mcp_servers:` →
```yaml
  codegraph:
    command: codegraph
    args:
      - serve
      - --mcp
    timeout: 120
    connect_timeout: 60
    enabled: true
```
plus `- mcp-codegraph` inside `platform_toolsets: cli:`. Greenfield toolset block: `platform_toolsets:\n  cli:\n    - hermes-cli\n    - mcp-codegraph`.

**Config file paths:** claude: g `~/.claude.json` + `~/.claude/settings.json` + `~/.claude/CLAUDE.md`; l `./.mcp.json` + `./.claude/settings.json` + `./.claude/CLAUDE.md` (legacy strip: `./.claude.json`). cursor: g `~/.cursor/mcp.json`; l `./.cursor/mcp.json` (+ rules cleanup `./.cursor/rules/codegraph.mdc`). codex (global-only): `~/.codex/config.toml`, `~/.codex/AGENTS.md`. opencode: g `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.jsonc|.json` + `.../AGENTS.md` (legacy sweep `%APPDATA%/opencode/{opencode.jsonc,opencode.json,AGENTS.md}` when `APPDATA` set and ≠ real dir); l `./opencode.jsonc|.json` + `./AGENTS.md`. hermes (global-only): `${HERMES_HOME:-~/.hermes}/config.yaml`. gemini: g `~/.gemini/settings.json` + `~/.gemini/GEMINI.md`; l `./.gemini/settings.json` + `./GEMINI.md` (project root, NOT under `.gemini/`). antigravity (global-only): unified/legacy `mcp_config.json` as above. kiro: g `~/.kiro/settings/mcp.json` (+ steering delete); l `./.kiro/settings/mcp.json`.

**Permissions contract:** exactly the string `mcp__codegraph__*` appended (deduped) to `permissions.allow` in Claude `settings.json`; uninstall filters out every entry with prefix `mcp__codegraph__`, deleting emptied `allow`/`permissions` keys.

**Instructions block (exact bytes, markers included):**
```
<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them, including dynamic-dispatch hops grep can't follow. Name a file or symbol in the query to read its current line-numbered source. If it's listed but deferred, load it by name via tool search.
- **Shell** (always works): `codegraph explore "<symbol names or question>"` prints the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
<!-- CODEGRAPH_END -->
```
Written to CLAUDE.md (claude), AGENTS.md (codex, opencode), GEMINI.md (gemini). Cursor/Kiro/Hermes/Antigravity write no instructions (only strip legacy).

**printConfig:** `# Add to <path>\n\n<2-space JSON or TOML snippet>\n`; global-only targets return `# <Name> ... use --location=global.\n` for local. **Notes:** cursor `Restart Cursor for MCP changes to take effect.`; hermes `Start a new Hermes session for MCP changes to take effect.`; antigravity `Restart Antigravity for MCP changes to take effect.`; kiro two notes: `Restart Kiro for MCP changes to take effect.` + `Kiro IDE: also enable MCP in Settings (search "MCP" → "Enabled"). Kiro CLI users can skip this step.`; codex/hermes/antigravity local-install note contains `no project-local config` / `--location=global`.

**Error semantics:** unparseable JSON never aborts — backed up to `<path>.backup`, treated as `{}`, warning printed. Unknown `--target` id throws (CLI exit 1). No other failures throw; unlink/backup failures swallowed.

## Test coverage

All in `installer-targets.test.ts` (contract suite — port everything) + `installer.test.ts` (3 shim tests). Harness: temp HOME via `HOME`/`USERPROFILE` env, `APPDATA` + `XDG_CONFIG_HOME` → `<home>/.config`, `HERMES_HOME` deleted; temp cwd via `process.chdir`; a planted `LEGACY_BLOCK` (same markers, old wording ``Prefer `codegraph_search` ...``) exercises self-heal.

1. **Parameterized contract ×13 (target×supported-location) — 5 tests each:** install writes files & flips `alreadyConfigured` false→true; re-run install ⇒ every action `unchanged`; sibling MCP server planted first survives (opencode uses `mcp`-shape seed); uninstall flips `alreadyConfigured` back false; `printConfig` non-empty and writes nothing (full before/after file listing of home+cwd compared).
2. **Codex:** install writes config.toml **and** AGENTS.md block (`## CodeGraph`, `codegraph explore`), re-run all-`unchanged`; legacy block replaced keeping user content (`action:'updated'`); user-added key inside our TOML block gets overwritten on re-install (`updated`, documented we own the block); TOML unit tests — build block contents, insert-into-empty starts with header, idempotent second upsert `unchanged` + byte-equal, replace-in-place preserves `[other_table]`/`[zzz]` siblings, remove preserves siblings, remove-missing `not-found` + content unchanged, `[[foo]]` array-of-tables siblings preserved through upsert.
3. **opencode:** prefers `.jsonc` over `.json` (and `.json` untouched); uses `.json` when only it exists; fresh install creates `.jsonc` (`created`); line+block comments survive install and byte-identical `unchanged` re-run; AGENTS.md block created / legacy-replaced / uninstall-stripped keeping user text; local install writes `./opencode.jsonc` + `./AGENTS.md`; uninstall removes only `mcp.codegraph`, keeps comments+siblings; **#535 suite** (APPDATA split from ~/.config): global install lands in XDG never `%APPDATA%`; greenfield targets XDG even when dir absent; honors `XDG_CONFIG_HOME`; install self-heals legacy `%APPDATA%` entry preserving siblings/comments and unlinks emptied legacy AGENTS.md, both reported `removed`; uninstall sweeps legacy without prior install; second install reports no legacy changes; legacy-only dir ⇒ `detect.installed` true but `alreadyConfigured` false.
4. **Gemini:** install writes settings.json entry equal to `{type:'stdio',command:'codegraph',args:['serve','--mcp']}` + GEMINI.md block; pre-existing `security.auth.selectedType:'oauth-personal'` survives install and uninstall; local writes `./.gemini/settings.json` + project-root `./GEMINI.md`; uninstall strips leftover GEMINI.md block keeping user content.
5. **Kiro:** install writes mcp.json entry (exact deep-equal) and **no** steering doc; install deletes leftover steering file (`removed`); sibling MCP server survives install/uninstall; uninstall removes leftover steering file; sibling `product.md` untouched; local paths.
6. **Antigravity:** legacy path when no marker (and Gemini settings.json untouched); unified path when `.migrated` present (legacy untouched); unified path when unified file exists without marker; entry has NO `type`, has `command`, `args === ['serve','--mcp']`; install migrates legacy entry out when marker appears; sibling server preserved (legacy path); Antigravity-managed `disabled:true` on siblings preserved; uninstall removes only codegraph; uninstall sweeps BOTH paths; local rejected with `files: []` + note matching `/no project-local config/`; never writes GEMINI.md; gemini+antigravity coexist and uninstalling one leaves the other.
7. **Hermes:** install preserves existing yaml (`model:`, sibling `other:` server, `discord:` toolset), adds child + `- mcp-codegraph`, second run `unchanged`; uninstall removes only codegraph bits keeping appended `custom:` block; **#456** PyYAML same-indent lists: `- mcp-codegraph` appended at 2-space indent, no items promoted to indent 0 (`^- browser` regex), `cli:` block ordering intact, idempotent; uninstall reverses on PyYAML-style config.
8. **Claude:** local writes `./.mcp.json` not `./.claude.json`; CLAUDE.md block created / legacy-replaced; global targets `~/.claude.json`; legacy `./.claude.json` migration (file deleted when codegraph-only; siblings + `somethingElse` preserved otherwise); uninstall strips both `.mcp.json` and legacy; legacy hook cleanup — install strips `mark-dirty`/`sync-if-dirty` keeping the GitKraken Stop hook and still writes permissions; sibling command in the same matcher group survives; byte-for-byte no-op without codegraph hooks; `not-found` when settings.json absent; re-run after cleanup byte-identical; uninstall strips `npx @colbymchenry/codegraph …` hook form, emptied `hooks` object removed; prompt hook — written with permissions, absent when not requested, idempotent (single entry, byte-identical), `promptHook:false` round-trips an opt-out, sibling UserPromptSubmit hook preserved and ordered `['my-own-hook','codegraph prompt-hook']`, uninstall keeps sibling, `removePromptHookEntry` leaves Stop-event legacy hook alone.
9. **Registry:** `getTarget` for all 8 ids + undefined for unknown; `resolveTargetFlag` none/all/csv order; throws `/Unknown --target/`.
10. **uninstallTargets sweep:** all-installed ⇒ every report `removed` with paths and `alreadyConfigured` false after; clean slate ⇒ all `not-configured`, empty paths; only-configured agents report `removed`; local sweep marks global-only targets `unsupported` with note `/global-only/`; second sweep all `not-configured`; `--target` subset removes only chosen, siblings stay configured.
11. **refreshTargets:** rewrites stale block (`refreshed`, changedPaths contains CLAUDE.md, `codegraph_search`→`codegraph_explore`); never first-installs; never rewrites user-trimmed permissions; second sweep all `unchanged`.
12. **Cursor rules cleanup:** uninstall deletes leftover `codegraph.mdc` entirely (frontmatter too); install self-heals it (`removed`); user content outside markers preserved (block-only strip).
13. **installer.test.ts:** `writeMcpConfig('local')` creates `.mcp.json`; corrupted JSON ⇒ warn + `.backup` with original bytes + valid rewrite; existing siblings/`customField` preserved.

## Rust port notes

- **Crate placement:** everything here → `selene-installer`; the `AgentTarget` trait, registry, and pure `uninstall_targets`/`refresh_targets` are library code; the clack interactive flow maps to `selene-cli` (e.g. `dialoguer`/`inquire`). `getMcpServerConfig` constants must obviously say `selene serve --mcp` (rename decision — the *shape* is the contract). Marker strings likewise become `<!-- SELENE_START -->` or stay CODEGRAPH-compatible if migrating existing installs is a goal — decide explicitly; the strip-on-install self-heal logic depends on matching whatever old installs wrote.
- **TS idioms to redesign:** targets read `process.cwd()` and env (`HOME`,`APPDATA`,`XDG_CONFIG_HOME`,`HERMES_HOME`) lazily at call time — the test harness depends on this (chdir + env swap). In Rust, inject a `Ctx { home, cwd, env }` into the trait instead of globals; `std::env::set_current_dir` in parallel tests is a hazard. `os.homedir()` reads `HOME`/`USERPROFILE` first — replicate that override order or tests can't fake home.
- **JSON handling:** `readJsonFile` accepts *any* JSON and preserves unknown keys — use `serde_json::Value`, never typed structs, or sibling preservation breaks. Note `writeJsonFile` **re-serializes the whole file** (2-space indent + trailing newline): Claude/Cursor/Gemini/Kiro/Antigravity JSON edits are *not* format-preserving (user formatting/comments in plain-JSON files are lost) — only opencode (JSONC) is surgical. Port that asymmetry as-is. For JSONC, no jsonc-parser equivalent exists off-the-shelf with `modify/applyEdits` semantics; you'll need a span-tracking JSONC editor (consider porting the minimal `modify` algorithm: locate/insert property spans, keep all other bytes).
- **`jsonDeepEqual` is key-order-insensitive but array-order-sensitive** — `serde_json::Value::eq` already matches this (maps unordered, arrays ordered).
- **Atomic write:** temp file `path + ".tmp." + pid` then rename; cleanup on failure. Same-directory rename keeps it atomic — keep that.
- **Windows:** paths are compared with suffix matching in tests because of `/var`→`/private/var` symlinks on macOS; use canonicalization carefully. The opencode legacy sweep is gated on `APPDATA` env (not `cfg!(windows)`) deliberately so tests exercise it cross-platform — keep the env gate.
- **execSync in antigravity** (`command -v codegraph || which codegraph` under `/bin/bash`, darwin only) — `std::process::Command` with `sh -c`; must swallow all failures and fall back to bare name. This makes `install` non-deterministic across machines (absolute path baked into JSON) — `jsonDeepEqual` idempotence still holds per-machine, but re-running after moving the binary reports `updated` (by design).
- **Suspected dead/quirky TS:** `WriteResult.action` includes `'kept'` but no target ever *returns* it from install/uninstall reporting except via `removeMarkedSection`'s `kept` (missing file) which flows into uninstall `files` — the orchestrator's verb map renders `kept`/`not-found` as `Updated` (fallthrough) in the install logger, a cosmetic bug; the uninstall path filters on `removed` only so it's harmless there. `config-writer.ts` shims and `clack.d.ts` need no port (drop, or keep `has_mcp_config`-style probes if `status` uses them). `DetectionResult` doc mentions a `--check` flag that doesn't exist on `install` (only `upgrade --check`). Hermes `topLevelRange` won't match quoted or non-`[A-Za-z_]`-initial top-level keys — acceptable, but document it. Cursor `MDC_FRONTMATTER` exists only for cleanup matching now (installer no longer writes the rules file) — port the exact string or leftover files won't be recognized as ours.
- **Test count discrepancy:** the CLAUDE.md says "~97 contract tests"; the file today has 95 static `it()` blocks which expand to ~155 runtime cases (5 contract tests × 13 target/location pairs + 90 singles). Port by *assertion*, not by count.
- **Ordering contract:** `ALL_TARGETS` order (claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro) is user-visible (prompt order, `--target=all`, reports) — freeze it.
