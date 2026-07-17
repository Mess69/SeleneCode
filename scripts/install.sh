#!/usr/bin/env bash
# SeleneCode installer.
#
#   curl -fsSL https://raw.githubusercontent.com/Mess69/SeleneCode/main/scripts/install.sh | sh
#
# Downloads the prebuilt static binary for this platform from GitHub Releases
# (checksum-verified) and installs it as `selene` on your PATH. Falls back to
# building from source when no release matches (or when run inside a checkout
# with SELENE_FROM_SOURCE=1).
#
#   SELENE_VERSION=v0.2.0   pin a version (default: latest)
#   SELENE_BIN_DIR=…        install dir (default: ~/.local/bin)
#   SELENE_GITHUB_REPO=…    owner/name override (default: Mess69/SeleneCode)
#   --uninstall             remove the installed binary
#
# Once installed, `selene upgrade` updates in place; `selene upgrade --check`
# just looks. Releases are published by `dist` (see dist-workspace.toml): every
# asset ships a .sha256 next to it.
set -euo pipefail

REPO="${SELENE_GITHUB_REPO:-Mess69/SeleneCode}"
BIN_DIR="${SELENE_BIN_DIR:-$HOME/.local/bin}"

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN_DIR/selene"
  echo "removed $BIN_DIR/selene (per-project data in each repo's .selene/ is untouched;"
  echo "run \`selene uninit\` in a project first if you want that gone too)"
  exit 0
fi

# --- platform → release target triple ---------------------------------------
os="$(uname -s)"; arch="$(uname -m)"
case "$os-$arch" in
  Darwin-arm64)              target="aarch64-apple-darwin" ;;
  Darwin-x86_64)             target="x86_64-apple-darwin" ;;
  Linux-x86_64|Linux-amd64)  target="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu" ;;
  *) target="" ;;
esac

fallback_build() {
  echo "$1"
  if ! command -v cargo >/dev/null 2>&1; then
    echo "error: no prebuilt binary and no cargo (Rust) to build one." >&2
    echo "Install Rust first:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
  fi
  repo_dir="$(cd "$(dirname "$0")/.." 2>/dev/null && pwd || true)"
  if [ -z "$repo_dir" ] || [ ! -f "$repo_dir/Cargo.toml" ]; then
    echo "error: not running from a SeleneCode checkout — clone it and re-run:" >&2
    echo "  git clone https://github.com/$REPO && cd $(basename "$REPO") && ./scripts/install.sh" >&2
    exit 1
  fi
  echo "building from source (first build takes a few minutes)…"
  cargo build --release -p selene --manifest-path "$repo_dir/Cargo.toml"
  mkdir -p "$BIN_DIR"
  install -m 755 "$repo_dir/target/release/selene" "$BIN_DIR/selene"
}

# --- resolve the release (redirect first: the unauthenticated API rate-limits) -
resolve_version() {
  if [ -n "${SELENE_VERSION:-}" ]; then
    case "$SELENE_VERSION" in v*) echo "$SELENE_VERSION" ;; *) echo "v$SELENE_VERSION" ;; esac
    return
  fi
  curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null \
    | sed 's|.*/tag/||' || true
}

if [ "${SELENE_FROM_SOURCE:-0}" = "1" ] || [ -z "$target" ]; then
  fallback_build "prebuilt binaries don't cover $os/$arch — building from source."
else
  version="$(resolve_version)"
  asset="selene-$target.tar.xz"
  url="https://github.com/$REPO/releases/download/$version/$asset"
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  if [ -n "$version" ] && curl -fsSL "$url" -o "$tmp/$asset" 2>/dev/null; then
    # Checksum: mandatory when published, skip when absent.
    if curl -fsSL "$url.sha256" -o "$tmp/$asset.sha256" 2>/dev/null; then
      (cd "$tmp" && shasum -a 256 -c "$asset.sha256" >/dev/null) \
        || { echo "error: checksum mismatch for $asset" >&2; exit 1; }
    fi
    tar -xJf "$tmp/$asset" -C "$tmp"
    mkdir -p "$BIN_DIR"
    install -m 755 "$(find "$tmp" -type f -name selene | head -1)" "$BIN_DIR/selene"
    echo "installed selene $version → $BIN_DIR/selene"
  else
    fallback_build "no published release found for $REPO — building from source."
  fi
fi

# --- PATH sanity -------------------------------------------------------------
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    echo "⚠ $BIN_DIR is not on your PATH. Add this to your shell profile (~/.zshrc):"
    echo "    export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac
other="$(command -v selene 2>/dev/null || true)"
if [ -n "$other" ] && [ "$other" != "$BIN_DIR/selene" ]; then
  echo "⚠ another \`selene\` at $other shadows this install — remove it or reorder PATH."
fi

cat <<'EOF'

Next, in any project you want your agent to understand:

    cd /path/to/your/project
    selene install        # indexes the project + wires the MCP server into Claude Code

Then restart Claude Code (or reload its MCP servers) and ask it a structural
question. `selene install -t auto` wires every detected agent; `selene viz
--open` draws the interactive code-graph map; `selene upgrade` updates later.
EOF
