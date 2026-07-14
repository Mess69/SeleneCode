# Task 20 — the milestone gate: the binary answers a flow question with zero Read

**The vertical-slice proof.** The real `selene` binary, on a real repo, answering a real flow
question — with the agent never opening a file. Two halves, both required.

## Half A — deterministic sufficiency (CI, `cargo test`)

`crates/selene-mcp/tests/dogfood_gate.rs` drives the release binary (`index` then `serve --mcp`),
speaks real MCP over its stdio, and asserts on the response bytes: every required symbol rendered as
a definition, every required file as a section, a Flow section with ≥3 steps, a blast-radius
section, and **no Read/Grep advice** outside the sanctioned banners. Plus a **negative control** —
the same assertions against a stopword must FAIL, proving they can tell a real answer from noise.

Run: `cargo test -p selene-mcp --test dogfood_gate -- --ignored --nocapture`

| repo | files | tier | query | symbols rendered | flow steps | Read-advice | verdict |
|---|---:|---|---|---|---:|---|---|
| SeleneCode (`.`) | _TBD_ | <500 | how does an unresolved reference become a graph edge | resolve_pending, resolve_all, resolve_one | _TBD_ | 0 | _TBD_ |
| CodeGraph (`../codegraph`) | _TBD_ | <500 | how does an MCP tools/call request reach handleExplore | handleMessage, handleToolsCall, handleExplore | _TBD_ | 0 | _TBD_ |
| **VS Code (`../vscode`)** | _TBD_ | **≥5000** | how does a keypress become an executed command | AbstractKeybindingService, _doDispatch, executeCommand, CommandsRegistry | _TBD_ | 0 | _TBD_ |

Negative control (`"the"` on SeleneCode): must FAIL the sufficiency assertions — _TBD_.

**The VS Code row is the one that matters.** The load-bearing hop is
`_doDispatch → _commandService.executeCommand`, where `_commandService` is typed `ICommandService`
— an **interface**. That hop exists in our graph only if Phase 3's dynamic-dispatch synthesis
bridged it. A missing bridge renders the flow as `_doDispatch → ?` and sends the agent to Read — the
exact failure the sufficiency invariant forbids, now load-bearing on a 12k-file repo instead of a
fixture.

## Half B — the real-agent zero-Read run (`scripts/dogfood.sh`, manual)

A headless `claude -p` session with the binary registered as an MCP server, tool_use blocks counted
mechanically: a run passes only if the agent used `selene_explore` within budget, opened **nothing**
(Read/Grep/Glob/Task == 0), and its answer **named** the required symbols. n=3 per repo; ≥2 must pass.

Run: `./scripts/dogfood.sh` — results appended below.

| repo | run | explore calls | Read | Grep | Glob | answered | verdict |
|---|---:|---:|---:|---:|---:|---|---|
| _pending manual run_ | | | | | | | |

## Indexing cost (for reference)

| repo | files | nodes | index time |
|---|---:|---:|---|
| SeleneCode | _TBD_ | _TBD_ | _TBD_ |
| CodeGraph | _TBD_ | _TBD_ | _TBD_ |
| VS Code | 12,123 | 349,737 | _TBD_ |
