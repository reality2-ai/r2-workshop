#!/usr/bin/env bash
# tools/install-launcher.sh — give the current user two easy ways to start
# the r2-workshop dashboard, both pointing at THIS checkout:
#
#   1. A desktop icon ("R2 Workshop") in the application menu / launcher.
#   2. A `r2-workshop` command on the PATH (~/.local/bin).
#
# No root needed — everything installs under ~/.local. Re-running is safe
# (it overwrites the previous install). Move or rename this checkout and
# just re-run to repoint both.
#
# Uninstall:  ./tools/install-launcher.sh --uninstall

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APPS_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
BIN_DIR="${HOME}/.local/bin"
DESKTOP="${APPS_DIR}/r2-workshop.desktop"
ICON="${ICON_DIR}/r2-workshop.svg"
CMD="${BIN_DIR}/r2-workshop"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$DESKTOP" "$ICON" "$CMD"
    update-desktop-database "$APPS_DIR" 2>/dev/null || true
    echo "Removed the R2 Workshop desktop icon and r2-workshop command."
    exit 0
fi

mkdir -p "$APPS_DIR" "$ICON_DIR" "$BIN_DIR"

# 1. Icon — reuse the web app's favicon (SVG scales to any size).
install -m644 "${REPO_ROOT}/webapp/favicon.svg" "$ICON"

# 2. Desktop entry, templated with the absolute path to start-server.sh.
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Name=R2 Workshop
Comment=Start the r2-workshop dashboard and open it in your browser
GenericName=Sensor rig dashboard
Exec=${REPO_ROOT}/tools/start-server.sh
Path=${REPO_ROOT}
Icon=r2-workshop
Terminal=true
Type=Application
Categories=Science;Monitor;Utility;
Keywords=r2;workshop;sensor;rig;dashboard;reality2;
StartupWMClass=r2-dashboard
EOF
chmod +x "$DESKTOP"

# 3. CLI command on the PATH.
ln -sf "${REPO_ROOT}/tools/start-server.sh" "$CMD"

update-desktop-database "$APPS_DIR" 2>/dev/null || true

echo "Installed:"
echo "  • Desktop icon  → search 'R2 Workshop' in your application menu"
echo "  • CLI command   → r2-workshop"
echo
case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *) echo "Note: ${BIN_DIR} is not on your PATH yet. Add this to ~/.bashrc:"
       echo "      export PATH=\"\$HOME/.local/bin:\$PATH\""
       echo "    (then open a new terminal). The desktop icon works regardless." ;;
esac
