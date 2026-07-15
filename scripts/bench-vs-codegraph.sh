#!/usr/bin/env bash
# Head-to-head indexing speed: SeleneCode (Rust) vs CodeGraph (TS).
# Source-only corpora, cold every run, sequential, same machine. Asserts the index landed.
set -u

SELENE="$(pwd)/target/release/selene"
CODEGRAPH="node $(pwd)/../codegraph/dist/bin/codegraph.js"
SCRATCH="${SCRATCH:-$CLAUDE_JOB_DIR/tmp/bench}"
RUNS="${RUNS:-1}"

prep() { # $1=name $2=src-dir  -> copies source-only into $SCRATCH/$1
  local name="$1" src="$2" dst="$SCRATCH/$1"
  rm -rf "$dst"; mkdir -p "$dst"
  # Copy source, excluding generated/vendored trees and wasm grammars.
  rsync -a --quiet \
    --exclude 'node_modules' --exclude 'target' --exclude 'dist' --exclude '.git' \
    --exclude '.selene' --exclude '.codegraph' --exclude '*.wasm' \
    "$src/" "$dst/" 2>/dev/null
  echo "$dst"
}

time_ms() { # runs "$@", prints elapsed ms
  local start end
  start=$(python3 -c 'import time;print(int(time.time()*1000))')
  "$@" >/dev/null 2>&1
  end=$(python3 -c 'import time;print(int(time.time()*1000))')
  echo $((end - start))
}

bench_selene() { # $1=dir -> ms (or FAIL)
  local dir="$1"
  rm -rf "$dir/.selene"
  local ms; ms=$(time_ms $SELENE index "$dir")
  [ -d "$dir/.selene" ] || { echo "FAIL"; return; }
  echo "$ms"
}

bench_codegraph() { # $1=dir -> ms (or FAIL)
  local dir="$1"
  rm -rf "$dir/.codegraph"
  local ms; ms=$(cd "$dir" && time_ms $CODEGRAPH init)
  [ -d "$dir/.codegraph" ] || { echo "FAIL"; return; }
  echo "$ms"
}

best() { # min of N runs of a fn on a dir
  local fn="$1" dir="$2" b=999999999 i ms
  for i in $(seq 1 "$RUNS"); do
    ms=$($fn "$dir"); [ "$ms" = "FAIL" ] && { echo FAIL; return; }
    [ "$ms" -lt "$b" ] && b=$ms
  done
  echo "$b"
}

printf "%-22s %10s %12s %8s\n" corpus selene_ms codegraph_ms gap
printf "%-22s %10s %12s %8s\n" ------ -------- ----------- ---
for row in "$@"; do
  name="${row%%:*}"; src="${row#*:}"
  [ -d "$src" ] || { printf "%-22s  (missing: %s)\n" "$name" "$src"; continue; }
  dir=$(prep "$name" "$src")
  s=$(best bench_selene "$dir")
  c=$(best bench_codegraph "$dir")
  if [ "$s" = FAIL ] || [ "$c" = FAIL ]; then
    printf "%-22s %10s %12s\n" "$name" "$s" "$c"
  else
    gap=$(python3 -c "print(f'{$s/$c:.2f}x')")
    printf "%-22s %10s %12s %8s\n" "$name" "$s" "$c" "$gap"
  fi
done
