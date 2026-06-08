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
  `origin/main` HEAD: `750e6e6`. Working tree clean at checkpoint.
- **Build-box: fully provisioned & validated** — espup/esp toolchain installed,
  espflash 4.4.0 (matches tuxedo), wasm-pack installed. All proven:
  firmware ×3 (devkitc/xiao/dfr1117 build + package + slot-fit + sidecar),
  WASM hive (`webapp/pkg`), server x86_64 (Alfred) + aarch64 (pi5) tarballs.
- **Release streams formalized (scheme a, `750e6e6`):** firmware `fw-vX.Y.Z`,
  server `server-vX.Y.Z`, never combined. build.rs strips `fw-`,
  build-server.sh strips `server-`; dashboard already matches firmware by
  asset name so no dashboard change. SPEC §13.3/§13.5 + changelog 0.3.4.

## Next steps
1. **Cut the first real release on the new scheme** — tag `fw-v0.3.x` +
   `server-v0.3.x`, clean build (`R2_RELEASE=1`), `gh release create` per
   stream. This is the true end-to-end test.
2. **Demo guard** `.tg_pub_demo_sha256` — deferred (needs upstream demo TG key
   hash; would be dormant for the rocker since its TG key is real).
3. Watch the **vendored core/hive crate re-sync** (upstream churn) before any
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
