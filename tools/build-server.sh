#!/usr/bin/env bash
# tools/build-server.sh — build & package the r2-workshop controller
# (the "server": core Hive binary + webapp + WASM hive) as self-contained
# per-architecture tarballs for Linux x86_64 and aarch64, published as
# their OWN release stream tagged `server-vX.Y.Z` — separate from the
# firmware's `fw-vX.Y.Z` releases. The server is downloaded + installed by
# an operator; firmware is fetched from GitHub by the running server. The
# two streams have different consumers and lifecycles, so they do not share
# a GitHub Release.
#
# See SPEC-R2-WORKSHOP-DASHBOARD §13.5 for the artefact convention.
#
# Of the three things that compile, only the core Hive binary is
# architecture-specific. The WASM hive (crates/r2-wasm → webapp/pkg) and
# the static webapp are architecture-independent — built once here and
# dropped into BOTH bundles.
#
# Architectures:
#   x86_64   — built natively on THIS host (must be x86_64 Linux).
#   aarch64  — built natively on a real ARM host over SSH (default: pi5),
#              from a clean git checkout of the same commit. Not a
#              cross-compile — the Hive links openssl (native-tls), which
#              makes cross-toolchains painful; a native build sidesteps it.
#
# Usage:
#   tools/build-server.sh                 # both arches, rebuild WASM
#   tools/build-server.sh --no-wasm       # reuse the committed webapp/pkg
#   tools/build-server.sh --arch x86_64   # one arch only (x86_64|aarch64)
#   tools/build-server.sh --pi5-host NAME # SSH host for the ARM build (default: pi5)
#
# Output: dist/r2-workshop-server-<class-slug>-<version>-linux-<arch>.tar.gz
#         plus a <tarball>.meta.json sidecar each.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="${REPO_ROOT}/dist"
WASM_CRATE="${REPO_ROOT}/crates/r2-wasm"
PKG_DIR="${REPO_ROOT}/webapp/pkg"

DO_WASM=1
PI5_HOST="pi5"
PI5_DIR="r2-workshop"          # build checkout on the remote (under $HOME)
ARCHES="x86_64 aarch64"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-wasm)   DO_WASM=0 ;;
        --arch)      ARCHES="${2//,/ }"; shift ;;
        --pi5-host)  PI5_HOST="$2"; shift ;;
        --help|-h)   sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

# ── Identity: class string + slug, version, git sha ───────────────────
CLASS_FILE="${REPO_ROOT}/trust_keys/sensor_class.txt"
if [[ ! -s "${CLASS_FILE}" ]]; then
    echo "ERROR: no sensor class file at trust_keys/sensor_class.txt." >&2
    echo "       Run: cargo run -p r2-workshop-tg --release -- init" >&2
    exit 1
fi
CLASS_STRING="$(tr -d '\n' < "${CLASS_FILE}")"
CLASS_SLUG="${CLASS_STRING//./-}"

SEMVER="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "${REPO_ROOT}/dashboard/Cargo.toml")"
SHA="$(git -C "${REPO_ROOT}" rev-parse --short=8 HEAD)"
REF="$(git -C "${REPO_ROOT}" rev-parse HEAD)"
BUILT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Version label for the bundle filename: the exact tag if HEAD is on one
# (a real release), otherwise <semver>+<sha> (a dev/preview build).
if TAG="$(git -C "${REPO_ROOT}" describe --tags --exact-match --match 'server-*' --match 'v[0-9]*' 2>/dev/null)"; then
    # Select the server stream tag (`server-*` / legacy `v*`), not `fw-*`, so a
    # commit carrying both stream tags resolves unambiguously (§13.5). Strip the
    # `server-` prefix so the bundle version label stays clean (`v0.3.1`).
    VERSION="${TAG#server-}"
else
    VERSION="${SEMVER}+${SHA}"
    echo "NOTE: HEAD is not on a tag — building a preview labelled '${VERSION}'." >&2
    echo "      For a published release, tag the commit 'server-vX.Y.Z' first so" >&2
    echo "      the bundle version matches the GitHub release tag (§13.5)." >&2
fi

# Dirty-tree note (cosmetic for the server — the dashboard's own version
# is not used for OTA matching, only sensors' are — but a clean release
# should build from a committed tree).
if ! git -C "${REPO_ROOT}" diff-index --quiet HEAD -- 2>/dev/null; then
    echo "WARNING: working tree is dirty — binaries will bake a '-dirty' suffix." >&2
fi

echo "==> class:   ${CLASS_STRING}  (slug ${CLASS_SLUG})"
echo "==> version: ${VERSION}"
echo "==> arches:  ${ARCHES}"
mkdir -p "${DIST}"

# ── 1. WASM hive (architecture-independent, built once) ───────────────
if [[ "${DO_WASM}" == 1 ]]; then
    if ! command -v wasm-pack >/dev/null 2>&1; then
        echo "ERROR: wasm-pack not found. Install it or pass --no-wasm to" >&2
        echo "       reuse the committed webapp/pkg." >&2
        exit 1
    fi
    echo "==> wasm-pack build crates/r2-wasm → webapp/pkg"
    ( cd "${REPO_ROOT}" && wasm-pack build crates/r2-wasm --target web --release --out-dir "${PKG_DIR}" )
fi
if [[ ! -f "${PKG_DIR}/r2_wasm_bg.wasm" ]]; then
    echo "ERROR: no WASM bundle at ${PKG_DIR} (build it, or drop --no-wasm)." >&2
    exit 1
fi

# ── Packaging helper: stage <arch> with the given binary, tar + sidecar ─
package_arch() {
    local arch="$1" binary="$2"
    local name="r2-workshop-server-${CLASS_SLUG}-${VERSION}-linux-${arch}"
    local stage="${DIST}/${name}"

    rm -rf "${stage}"
    mkdir -p "${stage}/tools"
    install -m755 "${binary}"                          "${stage}/r2-dashboard"
    cp -a "${REPO_ROOT}/webapp"                        "${stage}/webapp"
    install -m755 "${REPO_ROOT}/tools/start-server.sh" "${stage}/tools/start-server.sh"
    install -m755 "${REPO_ROOT}/tools/install-launcher.sh" "${stage}/tools/install-launcher.sh"

    cat > "${stage}/RUN.md" <<RUNDOC
# r2-workshop controller — ${VERSION} (linux/${arch})

Self-contained controller for the **${CLASS_STRING}** ensemble. No
internet or system install required.

## Run

    ./tools/start-server.sh

Then open <http://localhost:21042/> in a browser. Ctrl-C stops it.

## Desktop icon + \`r2-workshop\` command (optional)

    ./tools/install-launcher.sh

Adds an "R2 Workshop" launcher to your application menu and an
\`r2-workshop\` command on your PATH (both under ~/.local, no root).

## What's inside

* \`r2-dashboard\` — the controller Hive binary (this is the linux/${arch} build).
* \`webapp/\` — the operator UI + the WASM hive it loads in the browser.

The Trust-Group key and device state live under your user config/data
dirs at runtime; they are NOT part of this bundle.
RUNDOC

    local tarball="${DIST}/${name}.tar.gz"
    ( cd "${DIST}" && tar czf "${tarball}" "${name}" )
    rm -rf "${stage}"

    local sha256 size
    sha256="$(sha256sum "${tarball}" | awk '{print $1}')"
    size="$(stat -c%s "${tarball}")"
    cat > "${tarball}.meta.json" <<META
{
  "class":   "${CLASS_STRING}",
  "kind":    "server",
  "arch":    "${arch}",
  "version": "${VERSION}",
  "git":     "${SHA}",
  "sha256":  "${sha256}",
  "size":    ${size},
  "built":   "${BUILT}"
}
META
    echo "    packaged ${tarball}  (${size} bytes)"
}

# ── 2. x86_64 — native build on this host ─────────────────────────────
build_x86_64() {
    if [[ "$(uname -m)" != "x86_64" ]]; then
        echo "ERROR: x86_64 build requested but this host is $(uname -m)." >&2
        echo "       Run on an x86_64 host, or limit with --arch aarch64." >&2
        exit 1
    fi
    echo "==> [x86_64] cargo build --release -p r2-dashboard (local)"
    ( cd "${REPO_ROOT}" && cargo build --release -p r2-dashboard )
    package_arch x86_64 "${REPO_ROOT}/target/release/r2-dashboard"
}

# ── 3. aarch64 — native build on the ARM host (pi5) over SSH ───────────
build_aarch64() {
    echo "==> [aarch64] native build on '${PI5_HOST}' (git ref ${SHA})"
    # The remote builds from a clean checkout of THIS commit, so it must be
    # on origin first.
    if ! git -C "${REPO_ROOT}" branch -r --contains "${REF}" 2>/dev/null | grep -q .; then
        echo "ERROR: commit ${SHA} is not on any remote branch." >&2
        echo "       Push it first:  git push origin HEAD" >&2
        exit 1
    fi
    local remote_url
    remote_url="$(git -C "${REPO_ROOT}" remote get-url origin)"

    # Prepare the remote checkout at the exact ref, then build.
    ssh "${PI5_HOST}" bash -se -- "${remote_url}" "${PI5_DIR}" "${REF}" <<'REMOTE'
set -euo pipefail
REMOTE_URL="$1"; DIR="$2"; REF="$3"
. "$HOME/.cargo/env" 2>/dev/null || true
if [ ! -d "$HOME/$DIR/.git" ]; then
    git clone "$REMOTE_URL" "$HOME/$DIR"
fi
cd "$HOME/$DIR"
git fetch --quiet origin
git checkout --quiet "$REF"
git reset --hard --quiet "$REF"
echo "remote build host: $(uname -m) $(uname -s), rustc $(rustc --version)"
cargo build --release -p r2-dashboard
REMOTE

    # Pull the freshly-built binary back for packaging.
    local local_bin="${DIST}/.r2-dashboard-aarch64"
    scp -q "${PI5_HOST}:${PI5_DIR}/target/release/r2-dashboard" "${local_bin}"
    package_arch aarch64 "${local_bin}"
    rm -f "${local_bin}"
}

for arch in ${ARCHES}; do
    case "${arch}" in
        x86_64)  build_x86_64 ;;
        aarch64) build_aarch64 ;;
        *) echo "ERROR: unknown arch '${arch}' (want x86_64 or aarch64)" >&2; exit 2 ;;
    esac
done

echo
echo "Done. Artefacts in dist/:"
ls -la "${DIST}"/*.tar.gz "${DIST}"/*.meta.json 2>/dev/null
echo
echo "To publish the SERVER release stream (separate from firmware fw-* releases):"
echo "  gh release create server-${VERSION} --title \"r2-workshop server ${VERSION}\" \\"
echo "    dist/r2-workshop-server-*.tar.gz dist/r2-workshop-server-*.meta.json"
