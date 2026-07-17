#!/usr/bin/env bash
# SeleneCode machine install: build the release binary and put `selene` on PATH.
#
#   ./scripts/install.sh              # builds + installs to ~/.local/bin/selene
#   SELENE_BIN_DIR=/opt/bin ./scripts/install.sh
#
# Idempotent: re-running rebuilds and overwrites the installed binary.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${SELENE_BIN_DIR:-$HOME/.local/bin}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo (Rust) is required to build SeleneCode." >&2
  echo "Install it with:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  exit 1
fi

echo "building selene (release)… first build takes a few minutes"
cargo build --release -p selene --manifest-path "$REPO_DIR/Cargo.toml"

mkdir -p "$BIN_DIR"
install -m 755 "$REPO_DIR/target/release/selene" "$BIN_DIR/selene"
echo "installed: $BIN_DIR/selene ($("$BIN_DIR/selene" version 2>/dev/null || echo ok))"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    echo "⚠ $BIN_DIR is not on your PATH. Add this to your shell profile (~/.zshrc):"
    echo "    export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac

cat <<'EOF'

Next, in any project you want your agent to understand:

    cd /path/to/your/project
    selene install        # indexes the project + wires the MCP server into Claude Code

Then restart Claude Code (or reload its MCP servers). Ask it a structural
question — it will answer from the graph instead of reading files.

    selene install -t auto      # wire every detected agent (cursor, codex, …)
    selene viz --open           # interactive HTML map of the code graph
EOF
