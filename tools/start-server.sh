#!/usr/bin/env bash
# tools/start-server.sh — one-command launcher for the r2-workshop
# dashboard (the website operators use to monitor and control the rig).
#
# Serves the web app at http://localhost:21042/ from this checkout, and
# opens it in the default browser. Safe to re-run: if the server is
# already up, it just opens the browser and exits.
#
# Usage:
#   ./tools/start-server.sh            # build if needed, run, open browser
#   ./tools/start-server.sh --rebuild  # force a fresh release build first
#   ./tools/start-server.sh --no-open  # start the server, don't open a browser
#
# Stop the server with Ctrl-C in this terminal.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT=21042
URL="http://localhost:${PORT}/"

# Locate the dashboard binary. Two layouts are supported with one script:
#   • released bundle  → r2-dashboard sits next to webapp/ at the root
#   • dev checkout     → built into target/release/ by cargo
if [ -x "${REPO_ROOT}/r2-dashboard" ]; then
    BIN="${REPO_ROOT}/r2-dashboard"
else
    BIN="${REPO_ROOT}/target/release/r2-dashboard"
fi

REBUILD=0
OPEN=1
for arg in "$@"; do
    case "$arg" in
        --rebuild) REBUILD=1 ;;
        --no-open) OPEN=0 ;;
        --help|-h)
            sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
    esac
done

port_is_up() { ss -ltn 2>/dev/null | grep -qE ":${PORT}([^0-9]|$)"; }

open_browser() { [ "$OPEN" = 1 ] && command -v xdg-open >/dev/null 2>&1 && xdg-open "$URL" >/dev/null 2>&1 || true; }

# Already running? Don't start a second copy — just surface the URL.
if port_is_up; then
    echo "r2-workshop dashboard is already running → ${URL}"
    open_browser
    exit 0
fi

# Build the release binary if it's missing, or if --rebuild was asked for.
# A released bundle already ships the binary; only a dev checkout builds.
if [ "$REBUILD" = 1 ] || [ ! -x "$BIN" ]; then
    if ! command -v cargo >/dev/null 2>&1; then
        echo "ERROR: no r2-dashboard binary at ${BIN} and cargo is not installed." >&2
        echo "       In a released bundle the binary ships alongside this script —" >&2
        echo "       re-extract the tarball, or install Rust to build from source." >&2
        exit 1
    fi
    echo "Building r2-dashboard (release) — this can take a few minutes the first time…"
    ( cd "$REPO_ROOT" && cargo build --release -p r2-dashboard )
    BIN="${REPO_ROOT}/target/release/r2-dashboard"
fi

# Open the browser once the port comes up, while the server holds the
# foreground of this terminal.
if [ "$OPEN" = 1 ]; then
    (
        for _ in $(seq 1 90); do
            port_is_up && { open_browser; break; }
            sleep 1
        done
    ) &
fi

echo "Starting r2-workshop dashboard → ${URL}"
echo "(Press Ctrl-C to stop.)"
# The dashboard resolves webapp/ relative to its working directory.
cd "$REPO_ROOT"
exec "$BIN"
