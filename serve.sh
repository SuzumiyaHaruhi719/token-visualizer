#!/usr/bin/env bash
# Claude Monitor — browser mode (Linux/macOS).
#
# Builds (if needed) and runs the headless `cm-serve` binary, which serves the
# full dashboard and opens your default browser at it. No Tauri window/tray.
#
# Usage:
#   ./serve.sh                 # build + run on the default port (8788)
#   CM_PORT=9000 ./serve.sh    # pick a port
#   CM_NO_OPEN=1 ./serve.sh    # don't auto-open a browser
#
# Requires: a Rust toolchain (https://rustup.rs) and a built frontend. The
# frontend `dist/` is produced by `npm run build`; this script runs it for you
# if it's missing.
set -euo pipefail

# Resolve the script's own directory so it works from anywhere / double-click.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Make sure cargo is on PATH (covers the common rustup install location).
if ! command -v cargo >/dev/null 2>&1; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: 'cargo' not found. Install Rust from https://rustup.rs and retry." >&2
  exit 1
fi

# Build the frontend bundle if it isn't there yet (browser mode serves dist/).
if [ ! -f "$SCRIPT_DIR/dist/index.html" ]; then
  echo "dist/ not found — building the frontend (npm run build)…"
  if command -v npm >/dev/null 2>&1; then
    npm install
    npm run build
  else
    echo "error: dist/ is missing and 'npm' is not installed to build it." >&2
    echo "       Install Node.js, run 'npm install && npm run build', then retry." >&2
    exit 1
  fi
fi

echo "Starting Claude Monitor (browser mode)…"
exec cargo run -p cm-serve --release
