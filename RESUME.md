# RESUME — r2-workshop (workshop-worker)

_Owned by this session. Keep current. Last updated 2026-06-09._
Master save (read-only): `claude-fleet/fleet-context/FLEET-CONTEXT-SAVE.md` (+ plan + DEV_STATUS).

## Roles
1. **Build & release owner.** Alfred (this `/remote-control workshop` session,
   host `Alfred`, x86_64, checkout `~/Development/R2/r2-workshop`) builds and
   releases all artefacts; **tuxedo-os runs the live hardware** (BLE/WiFi +
   sensors; live dashboard on `:21042` from `/mnt/data/...` — do NOT
   rebuild/restart it from Alfred). aarch64 server builds run on **pi5**
   (`royspi5`) over SSH.
2. **ESP32 firmware/build reference** for hive's general no_std MCU hive, and
   **own-hive web-UI reference** for composer's proof-UX plugin. Both reference
   notes committed (see below). Workshop firmware is **Path A (std/ESP-IDF)** —
   pattern/architecture reference, not no_std-portable code.

## Branch / state
- **Branch:** `main` (this repo commits direct-to-main per convention).
  `origin/main` HEAD: `265b378`. Working tree clean.
- **Build-box: fully provisioned & validated** — espup/esp toolchain installed,
  espflash 4.4.0 (matches tuxedo), wasm-pack installed. All proven:
  firmware ×3 (devkitc/xiao/dfr1117 build + package + slot-fit + sidecar),
  WASM hive (`webapp/pkg`), server x86_64 (Alfred) + aarch64 (pi5) tarballs.
- **Release streams formalized + FIRST CUT DONE:** firmware `fw-vX.Y.Z`,
  server `server-vX.Y.Z`, never combined. **`fw-v0.3.0` PUBLISHED** (6 assets,
  per-carrier .bin+sidecar). build.rs/build-firmware.sh select `--match 'fw-*'`
  + strip prefix; build-server.sh `--match 'server-*'` + strip; build-firmware
  now emits canonical `<slug>-<carrier>-<ver>` archive names. SPEC §13.3/§13.5
  + changelog 0.3.4. Gaps fixed during the cut: tag ambiguity (`d4b7060`),
  canonical naming (`265b378`).
- **SERVER STREAM HELD.** `server-v0.3.1` tag exists on origin but the release
  is NOT published — Roy authorized firmware only. I had briefly published it
  by mistake and deleted it. Do NOT publish a server release without an
  explicit version from Roy. (Server code is at 0.3.1.)

## Next steps
0. **DEPLOYED to tuxedo @ `d88670a8` (2026-06-12).** Dashboard rebuilt + running
   the latest; `/api/version` correctly shows `d88670a8` clean (build.rs
   version-stamp fix validated). Per-session delete route live (200).
   **Awaiting Roy's verification:**
   - **Hard-refresh the browser (Ctrl-Shift-R)** to load the new webapp →
     confirm the "Sample at" dropdown holds its pick during recording, and
     the 🗑 Delete session button works (local + SD, offline tombstones).
   - **Power the sensors** → confirm the **flapping fix** (idle keepalive 30s→10s
     vs 15s read timeout): idle sensors should now stay connected instead of
     dropping every few seconds. (Deployed but untested — 0 sensors at deploy.)
1. **Server release — HELD.** When Roy gives an explicit version, cut
   `server-vX.Y.Z` (`build-server.sh` is ready; tag `server-v0.3.1` already
   exists if 0.3.1 is wanted). Do not publish without his go.
2. **Live tuxedo dashboard (`8125a18`) predates the list-walk fix (`d066ca1`)**
   — it queries `/releases/latest` (now a server tag, no firmware) so it won't
   see `fw-v0.3.0` until rebuilt to current `main`. Tuxedo-side update.
3. **DFR1195 (ESP32-S3) Path-B no_std build path** (forward dep, not urgent).
   New target NOT covered by the current Path-A/ESP-IDF matrix
   (devkitc/xiao/dfr1117). Pipeline: hive (no_std esp-hal/embassy source) →
   **workshop adds a no_std esp-hal build path + `fw-vX.Y.Z` release +
   OTA sidecars** → composer OTA push. hive coordinates when its scaffold
   compiles (hive branch `platform-trait`).
4. **Demo guard** `.tg_pub_demo_sha256` — deferred (needs upstream demo TG key
   hash; dormant for the rocker since its TG key is real).
5. Watch the **vendored core/hive crate re-sync** (upstream churn) before any
   protocol-touching work.

## Deps / peers (branches per supervisor)
- hive = `platform-trait`; composer = `phase-3-hardware-tier`.
- Chain: specs → core (no_std spec-impl) → hive (crates + platform layers).
  composer orchestrates hives (fleets/plugins/ensembles/OTA/proof-UX); it is
  NOT the hive. North-star: ONE hive codebase everywhere.

## Key references
- `docs/esp32-firmware-build-reference.md` — toolchain, firmware structure,
  `crates/r2-esp` platform-layer modules, Path A vs B honesty (`b2728a5`).
- `docs/own-hive-web-ui-recipe.md` — two-hive split, `registrations.r2-web`,
  reuse vs rebuild (`fb8da79`).
- Memory: `north-star-one-hive-codebase`, `build-release-roles-alfred-tuxedo`,
  `server-vs-firmware-release-streams`, `workshop-superseded-by-composer`.

## Safety (mandatory)
- Never `git add -A` / `git add .`; stage named files only (`git add -u` +
  named new files). No secrets. Public repo `reality2-ai.github.io` gets NO
  internal/fleet context.
