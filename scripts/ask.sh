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

# A file is only DELIVERED if its body is rendered under its own section header.
# Being *named* in the blast-radius list is NOT delivery — it is the worst outcome there is:
# it points the agent at a file it must then Read, which is precisely the failure the
# sufficiency/anti-Read invariant forbids. An earlier version of this probe substring-matched
# the whole response and so scored a blast-radius mention as a hit; it reported the gate
# question at 2/3 when the truth was 1/3. The instrument must not flatter the product.
shown_files = set(re.findall(r'(?m)^\*\*\`([^\`]+)\`\*\*', txt))
def delivered(path): return any(f.endswith(path) for f in shown_files)

s = re.search(r'Starting from: (.*)', txt)
print('  isError:', r.get('isError'), '| chars:', len(txt))
print('  seeds:  ', (s.group(1)[:85] if s else '(none)'))
print('  Flow:   ', '### Flow' in txt, '| steps:', len(re.findall(r'(?m)^\d+\.\s+\`', txt)))
print('  files shown:', ', '.join(sorted(f.split('/')[-1] for f in shown_files)) or '(none)')
# Task 20's bar for THIS question: the flow's files must be SHOWN and its functions PRESENT.
b  = delivered('selene-resolve/src/batch.rs')
r1 = 'resolve_one' in txt
r2 = 'resolve_and_persist_batched' in txt
print('  batch.rs SHOWN:', b, '| resolve_one:', r1, '| resolve_and_persist_batched:', r2,
      '=> %d/3' % sum([b, r1, r2]))
"
