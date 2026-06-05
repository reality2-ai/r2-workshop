#!/usr/bin/env bash
# tools/build-firmware.sh — build the ESP32-S3 firmware for a chosen
# carrier, package the OTA-ready application image (.bin), AND archive
# a copy under `firmware/esp32-s3/<carrier>/releases/<fw_ver>.bin` for
# git-tracked posterity.
#
# `cargo espflash flash` does the ELF→app-image conversion internally
# when flashing over USB, but doesn't write the .bin to disk; the OTA
# receiver (r2-esp::ota_tcp) needs the same image format on the wire,
# so this script runs `espflash save-image` after the build.
#
# Usage:
#   tools/build-firmware.sh                # defaults to devkitc
#   tools/build-firmware.sh devkitc        # esp32-s3 (xtensa)
#   tools/build-firmware.sh xiao           # esp32-s3 (xtensa)
#   tools/build-firmware.sh dfr1117        # esp32-c6 (riscv) — Beetle
#
# After this completes:
# * Latest build artifact (overwritten on each run) is at
#   `firmware/esp32-s3/<carrier>/target/xtensa-esp32s3-espidf/release/r2-workshop-firmware.bin`
#   — push this via /api/ota/{addr}.
# * Versioned archive copy lives at
#   `firmware/esp32-s3/<carrier>/releases/r2-workshop-firmware-<fw_ver>.bin`
#   — `git add` this when you want to record the release for posterity.
#   The filename matches the `fw_ver` string the firmware bakes into
#   `r2.sensor.announce`, so a sensor's reported version is searchable
#   directly against the releases directory.

set -euo pipefail

CARRIER="${1:-devkitc}"

# Carrier → (architecture dir, Rust target triple, espflash chip).
# devkitc + xiao are xtensa ESP32-S3; dfr1117 is a RISC-V ESP32-C6.
case "${CARRIER}" in
    devkitc|xiao) ARCH_DIR="esp32-s3"; TARGET="xtensa-esp32s3-espidf";  CHIP="esp32s3" ;;
    dfr1117)      ARCH_DIR="esp32-c6"; TARGET="riscv32imac-esp-espidf"; CHIP="esp32c6" ;;
    *)
        echo "ERROR: unknown carrier '${CARRIER}'" >&2
        echo "Known carriers: devkitc, xiao (esp32-s3); dfr1117 (esp32-c6)" >&2
        exit 1
        ;;
esac

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW_DIR="${REPO_ROOT}/firmware/${ARCH_DIR}/${CARRIER}"
REL_DIR="${FW_DIR}/releases"

if [[ ! -f "${FW_DIR}/Cargo.toml" ]]; then
    echo "ERROR: no Cargo.toml at ${FW_DIR}" >&2
    exit 1
fi

# Trust-Group key guard. The firmware embeds trust_keys/tg_pub.bin
# at compile time via include_bytes!. Two failure modes to catch
# before kicking off a 30 s xtensa build:
#
#   1. No tg_pub.bin / tg_cert.bin at all — fresh clone, no deployment
#      keys yet. The build would fail late with an include_bytes!
#      error; we'd rather print actionable setup instructions early.
#
#   2. tg_pub.bin is the canonical upstream demo key. Identifiable
#      by SHA-256 against a hash recorded under
#      `trust_keys/.tg_pub_demo_sha256`. Building against this key
#      means every lab that clones the public repo gets the same TG
#      embedded, which only works for the one deployment that owns
#      the matching tg_priv.bin. Per
#      audits/2026-05-23-architectural-gaps.md (post-handoff
#      recommendation), refuse this case so the new lab is forced
#      to keygen first.
TG_PUB="${REPO_ROOT}/trust_keys/tg_pub.bin"
TG_CERT="${REPO_ROOT}/trust_keys/tg_cert.bin"
KEYGEN_HINT=$(cat <<'EOF'
This deployment has no Trust Group keys / sensor class file, or is
still on the upstream demo values. Each r2-workshop deployment needs:

  * its own TG keypair (sensors verify certs against the public key
    baked into the firmware at build time);
  * its own BLE-beacon class string (so sensors from different labs
    don't BLE-bootstrap onto each other's dashboards).

Generate both for this deployment in one shot (one-time per lab):

    cd "$REPO_ROOT" && cargo run -p r2-workshop-tg --release -- init

That writes:
  trust_keys/tg_pub.bin            (committed; embedded into firmware)
  trust_keys/tg_cert.bin           (committed; self-signed KeyHolder cert)
  trust_keys/sensor_class.txt      (committed; read by firmware + dashboard at build time)
  ~/.config/r2-workshop/tg_signer/tg_priv.bin   (off-tree; read by dashboard)

Pass --class "your.reverse.dns.sensor" to set the BLE class string
explicitly; otherwise the tool derives one from the TG name. After it
completes, re-run this script. See SECRETS-POLICY.md for the full
key-handling policy.
EOF
)

if [[ ! -s "${TG_PUB}" || ! -s "${TG_CERT}" ]]; then
    echo "ERROR: no Trust Group keys at trust_keys/tg_pub.bin (or tg_cert.bin)." >&2
    echo "" >&2
    echo "$KEYGEN_HINT" >&2
    exit 1
fi

DEMO_HASH_FILE="${REPO_ROOT}/trust_keys/.tg_pub_demo_sha256"
if [[ -s "${DEMO_HASH_FILE}" ]]; then
    DEMO_HASH=$(cat "${DEMO_HASH_FILE}")
    ACTUAL_HASH=$(sha256sum "${TG_PUB}" | awk '{print $1}')
    if [[ "${ACTUAL_HASH}" == "${DEMO_HASH}" ]]; then
        echo "ERROR: trust_keys/tg_pub.bin matches the upstream demo key SHA." >&2
        echo "" >&2
        echo "$KEYGEN_HINT" >&2
        exit 1
    fi
fi

# Per-deployment R2-BEACON class string. Unlike the TG check above,
# this one is *soft* — the class string only affects BLE discovery,
# not signing authority, so two labs sharing the upstream class can
# still safely run because the TG check independently prevents them
# from sharing identities. We warn, and if interactive ask whether
# to proceed; CI / non-interactive callers can set
# WORKSHOP_USE_DEFAULT_CLASS=1 to opt in explicitly.
CLASS_FILE="${REPO_ROOT}/trust_keys/sensor_class.txt"
CLASS_DEMO_HASH_FILE="${REPO_ROOT}/trust_keys/.sensor_class_demo_sha256"
if [[ ! -s "${CLASS_FILE}" ]]; then
    echo "ERROR: no sensor class file at trust_keys/sensor_class.txt." >&2
    echo "" >&2
    echo "$KEYGEN_HINT" >&2
    exit 1
fi
# Reverse-DNS class string — read once here so it's available both for the
# demo-class warning below and the meta sidecar emitted after the build
# (SPEC-R2-WORKSHOP-DASHBOARD §13.3).
CLASS_STRING=$(tr -d '\n' < "${CLASS_FILE}")
if [[ -s "${CLASS_DEMO_HASH_FILE}" ]]; then
    CLASS_DEMO_HASH=$(cat "${CLASS_DEMO_HASH_FILE}")
    CLASS_ACTUAL_HASH=$(sha256sum "${CLASS_FILE}" | awk '{print $1}')
    if [[ "${CLASS_ACTUAL_HASH}" == "${CLASS_DEMO_HASH}" ]]; then
        echo "WARNING: trust_keys/sensor_class.txt matches the upstream demo class SHA." >&2
        echo "         Class: ${CLASS_STRING}" >&2
        echo "         Sensors built from this firmware will be BLE-discoverable by any" >&2
        echo "         dashboard using the same upstream class (and vice-versa). Fine for" >&2
        echo "         first bring-up. Before sharing spectrum with another r2-workshop" >&2
        echo "         deployment, mint your own class string:" >&2
        echo "             cargo run -p r2-workshop-tg --release -- init --force --class \"your.reverse.dns.sensor\"" >&2
        echo "" >&2
        if [[ -t 0 ]]; then
            read -r -p "Build anyway with the upstream class? [y/N] " confirm
            case "${confirm,,}" in
                y|yes) echo "Proceeding with upstream class." >&2 ;;
                *) echo "Aborted." >&2; exit 1 ;;
            esac
        else
            if [[ "${WORKSHOP_USE_DEFAULT_CLASS:-}" == "1" ]]; then
                echo "Non-interactive build with WORKSHOP_USE_DEFAULT_CLASS=1 — proceeding." >&2
            else
                echo "Non-interactive build refusing default class. Set WORKSHOP_USE_DEFAULT_CLASS=1 to override." >&2
                exit 1
            fi
        fi
    fi
fi

# Pull in the ESP-IDF / xtensa toolchain if exported. Best-effort —
# users in a fresh shell still need to source `~/export-esp.sh` first.
if [[ -f "${HOME}/export-esp.sh" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/export-esp.sh" >/dev/null 2>&1 || true
fi

cd "${FW_DIR}"

echo "==> cargo build --release (${TARGET}) — carrier=${CARRIER}"
cargo build --release

ELF="target/${TARGET}/release/r2-workshop-firmware"
BIN="${ELF}.bin"

echo "==> espflash save-image  →  ${BIN}"
espflash save-image --chip "${CHIP}" "${ELF}" "${BIN}"

# Compute the same FW_VER string the firmware bakes in via build.rs:
#   <semver>-<YYYY-MM-DD-HH:MM>+<git-short-sha>[-dirty]
# Same git + date inputs as build.rs, so within the same minute the
# script's filename matches the announce string exactly. (May drift by
# 1 minute in pathological build-races; close enough for archival.)
SEMVER=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "${FW_DIR}/Cargo.toml")
SHA=$(git -C "${REPO_ROOT}" rev-parse --short=8 HEAD 2>/dev/null || echo unknown)
DIRTY=""
if ! git -C "${REPO_ROOT}" diff-index --quiet HEAD -- 2>/dev/null; then DIRTY="-dirty"; fi
TS=$(date -u +%Y-%m-%d-%H:%M)
FW_VER="${SEMVER}-${TS}+${SHA}${DIRTY}"

mkdir -p "${REL_DIR}"
ARCHIVE="${REL_DIR}/r2-workshop-firmware-${FW_VER}.bin"
cp "${BIN}" "${ARCHIVE}"

# Meta sidecar (SPEC-R2-WORKSHOP-DASHBOARD §13.3) — authoritative
# (class, carrier, version, sha256) tuple the dashboard's local-fallback
# scanner reads to match a sensor's announce to this build. We keep the
# timestamped local filename (so repeated dev builds at the same semver
# don't clobber each other) and let the sidecar carry the canonical tuple.
SHA256=$(sha256sum "${ARCHIVE}" | awk '{print $1}')
BUILT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
META="${ARCHIVE}.meta.json"
cat > "${META}" <<EOF
{
  "class":   "${CLASS_STRING}",
  "carrier": "${CARRIER}",
  "version": "${FW_VER}",
  "git":     "${SHA}",
  "sha256":  "${SHA256}",
  "built":   "${BUILT}"
}
EOF

echo
echo "OTA-ready image (use this with /api/ota/{addr}):"
ls -la "${BIN}"
echo
echo "Versioned archive copy (git add to record the release):"
ls -la "${ARCHIVE}"
echo
echo "Meta sidecar (git add alongside the .bin):"
ls -la "${META}"
