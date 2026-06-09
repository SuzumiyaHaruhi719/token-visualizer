#!/usr/bin/env bash
# Claude Monitor — browser mode (macOS double-click launcher).
#
# Double-click this file in Finder to build (if needed) and launch the dashboard
# in your default browser. No Tauri window/tray.
#
# First-time setup (once): make it executable so Finder can run it —
#   chmod +x serve.command
# (Or: right-click → Open the first time to clear Gatekeeper.)
#
# This is a thin wrapper around serve.sh, which does the real work. Env vars
# like CM_PORT / CM_NO_OPEN are honored by serve.sh.
set -euo pipefail

# Resolve this file's directory (works when launched from Finder).
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$DIR/serve.sh"
