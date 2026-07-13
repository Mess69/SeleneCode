#!/bin/bash
# Drive the REAL `selene` binary over REAL MCP stdio and report what `explore` actually returns.
#
# This exists because unit tests lie here. `phase4_gate.rs` is green (7/7) while `explore`
# cannot answer the milestone gate's own question — it passes on planted fixtures. The only
# evidence that counts for the relevance/Flow bug is the real binary on a real repo.
# See RESUME.md §2.
#
#   ./scripts/ask.sh "how does an unresolved reference become a graph edge"
#   CORPUS=/tmp/cg ./scripts/ask.sh "how are edges created during resolution"
#
# Setup (once, ~90s):
#   cargo build --release -p selene
#   rm -rf /tmp/dogfood-selene && mkdir -p /tmp/dogfood-selene
#   cp -R crates docs Cargo.toml /tmp/dogfood-selene/
#   ./target/release/selene index /tmp/dogfood-selene
set -euo pipefail

q="${1:?usage: ./scripts/ask.sh \"<query>\"}"
corpus="${CORPUS:-/tmp/dogfood-selene}"
bin="${SELENE_BIN:-./target/release/selene}"

[ -x "$bin" ] || { echo "no binary at $bin — run: cargo build --release -p selene" >&2; exit 1; }
[ -d "$corpus/.selene" ] || { echo "$corpus is not indexed — run: $bin index $corpus" >&2; exit 1; }

printf '%s\n%s\n%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"explore\",\"arguments\":{\"query\":\"$q\"}}}" \
 | "$bin" serve --mcp --path "$corpus" 2>/dev/null | sed -n '2p' \
 | python3 -c "
import sys, json, re
d = json.load(sys.stdin)
r = d.get('result', {})
txt = ''.join(c.get('text', '') for c in r.get('content', []))
s = re.search(r'Starting from: (.*)', txt)
print('  isError:', r.get('isError'), '| chars:', len(txt))
print('  seeds:  ', (s.group(1)[:85] if s else '(none)'))
print('  Flow:   ', '### Flow' in txt, '| steps:', len(re.findall(r'(?m)^\d+\.\s+\`', txt)))
# The four symbols + two files Task 20 requires for THIS question. The gate's own bar.
print('  batch.rs:', 'selene-resolve/src/batch.rs' in txt, '| resolve_one:', 'resolve_one' in txt,
      '| resolve_and_persist_batched:', 'resolve_and_persist_batched' in txt)
"
