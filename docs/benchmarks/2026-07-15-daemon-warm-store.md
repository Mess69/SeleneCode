# Daemon warm-store latency (2026-07-15)

Measured against the **real binary** (`e580537`), release build, on a real Rust codebase
(SeleneCode's own `crates/` tree copied into an isolated project).

## Index under test

| | |
|---|---:|
| files | 344 |
| nodes | 5 382 |
| edges | 18 138 |
| index time | ~4.0 s |

## What the daemon changes

Before this session, **every MCP handler opened RocksDB per call and dropped it** — a five-call
agent session paid five opens. The daemon is now the sole store owner and holds it warm for its
whole life; each agent's `serve --mcp` proxies to it.

## Numbers

**Store open cost alone** (`selene status` = open + read stats + exit): **~0.08 s**. So the open is
~80 ms on this index — a fixed tax the warm store removes on calls 2..N (and it grows with repo
size: the large-repo notes had first-call latency in seconds).

**Cold vs warm `explore`:**

| path | latency |
|---|---:|
| cold CLI `explore` (fresh process, fresh open — *every* call) | ~0.76–0.97 s |
| warm daemon, call 1 (store open, caches cold) | ~0.72 s |
| warm daemon, call 2 | ~0.66 s |
| warm daemon, calls 3–6 (RocksDB block cache + OS page cache hot) | **~0.18–0.45 s** |

The warm daemon drops repeat-query latency to roughly **2–4×** faster than the cold per-call path,
once the store and its caches are hot. The first daemon call is ~cold (the open is amortized but the
query-path caches aren't primed yet); the win compounds over a session.

## Method (reproducible)

```sh
BIN=target/release/selene
# cold: fresh process each call
for i in 1 2 3 4 5; do /usr/bin/time -p "$BIN" explore SurrealStore --path "$PROJ"; done
# warm: one daemon, N queries over its socket (newline-delimited MCP JSON-RPC)
SELENE_DAEMON_INTERNAL=1 "$BIN" serve --mcp --path "$PROJ" &   # binds .selene/daemon.sock
# then connect a socket client, initialize, and time repeated tools/call explore
```

## Caveat — not yet an A/B vs TS+SQLite query latency

This measures SeleneCode's own cold-vs-warm delta, which is the daemon's contribution. It is **not**
a head-to-head query-latency comparison against CodeGraph TS+SQLite — TS's daemon kept SQLite warm
too, and SQLite is multi-reader, so the honest framing is: the daemon brings SeleneCode's warm-store
behavior up to parity with what TS already had, on top of the exclusive-lock correctness it *needs*.
The indexing-throughput win over TS is measured separately (`2026-07-14-rust-vs-ts-speed.md`).
