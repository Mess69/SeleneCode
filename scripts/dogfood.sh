#!/usr/bin/env bash
# Task 20 — Half B: the REAL-AGENT zero-Read run.
#
# Half A (`dogfood_gate.rs`) proves the *output* is sufficient. Half B proves an *agent actually
# uses it* — because the TS build learned the hard way (adaptive-explore dead-end #6) that a
# deterministic probe and a real agent form different queries and disagree. The probe said "reads
# flat"; the agent Read the file back.
#
# This registers the built `selene` binary as an MCP server for a headless `claude -p` session and,
# for each flow question, MECHANICALLY counts the tool_use blocks in the stream: a run passes only
# if the agent used selene, opened NOTHING (Read/Grep/Glob == 0, and no Task delegation), and its
# answer actually NAMES the symbols a correct answer requires. A run that reads nothing because it
# answered nothing is a FAILURE, not a pass.
#
#   ./scripts/dogfood.sh                 # all repos, n=3 each
#   ./scripts/dogfood.sh ../vscode       # one repo
#
# Results go to docs/benchmarks/2026-07-phase5-dogfood.md. Requires: the release binary, the `claude`
# CLI, and the sibling repos (../codegraph, ../vscode) cloned.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/selene"
RUNS="${RUNS:-3}"
QUESTIONS="$ROOT/crates/selene-mcp/tests/fixtures/dogfood/questions.toml"

[ -x "$BIN" ] || { echo "no binary at $BIN — cargo build --release -p selene" >&2; exit 1; }
command -v claude >/dev/null || { echo "the 'claude' CLI is required for Half B" >&2; exit 1; }

# Parse questions.toml into: repo<TAB>query<TAB>sym1,sym2,…<TAB>max_calls  (one line per row).
rows() {
  python3 - "$QUESTIONS" <<'PY'
import sys, tomllib
d = tomllib.load(open(sys.argv[1], "rb"))
for q in d["question"]:
    print("\t".join([
        q["repo"], q["query"],
        ",".join(q["must_contain_symbols"]),
        str(q["max_explore_calls"]),
    ]))
PY
}

# Count tool_use blocks by name in a claude stream-json transcript (one JSON object per line).
count_tool() {  # <transcript> <tool-name>
  python3 - "$1" "$2" <<'PY'
import sys, json
name = sys.argv[2]; n = 0
for line in open(sys.argv[1]):
    try: ev = json.loads(line)
    except Exception: continue
    for block in (ev.get("message", {}) or {}).get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "tool_use" and block.get("name") == name:
            n += 1
print(n)
PY
}

# The final assistant text of the session.
final_text() {  # <transcript>
  python3 - "$1" <<'PY'
import sys, json
out = ""
for line in open(sys.argv[1]):
    try: ev = json.loads(line)
    except Exception: continue
    for block in (ev.get("message", {}) or {}).get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            out = block["text"]
print(out)
PY
}

filter="${1:-}"
echo "# Task 20 Half B — real-agent zero-Read runs ($(date -u +%Y-%m-%dT%H:%MZ))"
echo
printf '%-14s %-6s %-6s %-6s %-6s %-6s %-8s %s\n' repo run explore Read Grep Glob answered verdict
printf '%s\n' "----------------------------------------------------------------------------------"

while IFS=$'\t' read -r repo query syms maxcalls; do
  [ -n "$filter" ] && [ "$repo" != "$filter" ] && continue
  target="$ROOT/$repo"
  [ -d "$target" ] || { echo "SKIP $repo — not present"; continue; }

  # Index into the repo (reused across runs) and register selene as an MCP server for this session.
  [ -d "$target/.selene" ] || "$BIN" index "$target" >/dev/null
  mcp_cfg="$(mktemp)"
  cat > "$mcp_cfg" <<JSON
{"mcpServers":{"selene":{"command":"$BIN","args":["serve","--mcp","--path","$target"]}}}
JSON

  pass=0
  for run in $(seq 1 "$RUNS"); do
    tx="$(mktemp)"
    # Headless, tools restricted so a lazy fallback to Read is *possible* (we WANT to detect it),
    # streaming JSON so every tool_use is on the record.
    claude -p "$query" \
      --mcp-config "$mcp_cfg" \
      --output-format stream-json --verbose \
      > "$tx" 2>/dev/null || true

    ex=$(count_tool "$tx" "mcp__selene__explore")
    rd=$(count_tool "$tx" "Read"); gr=$(count_tool "$tx" "Grep"); gl=$(count_tool "$tx" "Glob")
    tk=$(count_tool "$tx" "Task")
    ans="$(final_text "$tx")"
    named=1
    IFS=',' read -ra want <<< "$syms"
    for s in "${want[@]}"; do case "$ans" in *"$s"*) ;; *) named=0 ;; esac; done

    # PASS iff: used selene within budget, opened nothing, delegated nothing, AND answered.
    if [ "$ex" -ge 1 ] && [ "$ex" -le "$maxcalls" ] && [ "$rd" -eq 0 ] && [ "$gr" -eq 0 ] \
       && [ "$gl" -eq 0 ] && [ "$tk" -eq 0 ] && [ "$named" -eq 1 ]; then
      verdict=PASS; pass=$((pass+1))
    else
      verdict=FAIL
    fi
    printf '%-14s %-6s %-6s %-6s %-6s %-6s %-8s %s\n' "$repo" "$run" "$ex" "$rd" "$gr" "$gl" "$named" "$verdict"
    rm -f "$tx"
  done
  rm -f "$mcp_cfg"
  # The gate's per-repo rule: ≥2 of 3 runs must PASS.
  if [ "$pass" -ge 2 ]; then echo "  => $repo: $pass/$RUNS PASS ✅"; else echo "  => $repo: $pass/$RUNS — GATE FAILED ❌"; fi
  echo
done < <(rows)
