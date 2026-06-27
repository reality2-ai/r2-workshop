# RESUME — r2-workshop (workshop-worker)

_Owned by this session. Keep current. Last updated 2026-06-28._
**STATE: STABLE maintenance-mode** (rocker-lab, held per Roy). Verified baseline before this
RESUME-only watchdog update: `main` @ `a98720f`
(local==origin; includes companion re-anchor after Roy's firmware OTA-gate `29b1b0a`, devkitc-verified),
`profile-a-pathdep` @ `3251c23` (local==origin, R2-TN reference,
composer's). Tree clean. tuxedo-dashboard-rebuild artifact pre-built + staged in
`dist/` (local). All open items Roy/tuxedo-gated — see Next steps. Parked: R2-TN
(composer owns). No active workshop change pending.
Master save (read-only): `r2-fleet/fleet-context/FLEET-CONTEXT-SAVE.md` (+ plan + DEV_STATUS).
(Relocated 2026-06-18 from `claude-fleet/fleet-context/`; claude-fleet is now tooling-code-only.)

## Companion re-anchor (2026-06-27, workshop-codex)
- **Current objective:** carry on / maintain rocker-lab handoff; no active code task found.
- **Last verified state:** the re-anchor edit was folded and pushed in `a98720f`; on the
  2026-06-28 watchdog check before this RESUME-only update, `git status --short --branch` was clean and
  `git rev-parse HEAD`/`origin/main` both resolved to
  `a98720f4e1d27f1323acac7b34bbc203f7fba1ee`.
- **Changed files this turn:** `RESUME.md` only, to correct stale HEAD notes and record companion
  re-anchors/watchdog idle state. No code, build, release, or deploy files were changed.
- **Coordination:** `fleet ask workshop` succeeded after an initial spend-limit stale inbox entry.
  Base worker confirms it is idle, has no unrecorded rocker-lab task, and will not touch
  `RESUME.md` while this handoff edit is uncommitted. Base also confirms R2-TN remains composer-owned.
- **Next action:** remain parked unless Roy opens a tuxedo deploy window, asks for a server release
  version, or requests Path-B/no_std build-path work after hive/composer sequencing.
- **Do not assume:** R2-TN protocol work is still composer-owned; do not publish server releases
  without Roy's explicit version; do not touch live tuxedo dashboard from Alfred outside a coordinated
  deploy window.

## ★ SCOPE (Roy, 2026-06-22)
workshop's project is the **ROCKER LAB** (this `main`: dense rocker-monitoring rig +
:21042 dashboard/webapp + Path-A firmware/build/release). The **R2 TN cross-platform
protocol cycle** (transient-mesh / EspNow / leaderless-PCO) was mis-routed to me and
**stood down** — **composer owns it now**. That work is committed as a clean REFERENCE
on branch `profile-a-pathdep` (EspNowTransport, heartbeat.rs HB-frame, EspNegotiationRadio,
TN-FORM-CONJECTURES, r2-fnv/discovery/route/wire/transport path-deps) — NOT this branch,
NOT my active track. Don't pick up R2-TN protocol-dev here. Rocker-lab status below.

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
  Verified baseline before the 2026-06-28 watchdog RESUME-only update:
  `origin/main` HEAD: `a98720f` (local==origin), working tree clean. Any later tip should be
  a RESUME-only handoff commit unless new code work has been explicitly opened.
  R2-TN reference lives on `profile-a-pathdep` @ `3251c23` (composer's track —
  don't pick it up here). Earlier rocker-lab cut work was at `265b378`.
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

## OTA-revert diagnosis (2026-06-16) — RESOLVED as not-a-bug
Device 10.42.0.47 (devkitc, running **dev-build** 0.3.0) reverted after a cloud
OTA of release `fw-v0.3.0`. Forensics:
- Released asset `...rocker-devkitc-v0.3.0.bin` is **byte-perfect**: size 1530128
  + sha256 `871db034…` match the OTA-TCP log AND the sidecar. Not corruption.
- Valid image header: magic OK, ESP32-S3, 8MB, DIO/40MHz. Not a bad image.
- **No firmware commits since the `fw-v0.3.0` tag** → release image is
  logically IDENTICAL to the running dev build (only the version string differs).
- Anti-brick has **no rollback timer**; `mark_app_valid()` fires only on the
  **first `send_sample` after a dashboard TCP connect** (sender.rs:278).
- ∴ Revert ⟺ the new image rebooted *before* WiFi-connect → dashboard-connect →
  first frame. On the flaky-WiFi box this is the rollback working **as designed**,
  not an OTA bug. (Matches Roy's confirmed flaky-WiFi/BLE theory.)
- OTA of this release is also **functionally pointless** — identical firmware,
  cosmetic version string only.
- Design note for real upgrades (e.g. 0.3.0→0.4.0): gating mark-valid on a full
  dashboard round-trip makes OTA reliability hostage to network flakiness at the
  worst moment. Consider marking valid after the image self-proves (boot + WiFi
  assoc + self-test) rather than requiring an end-to-end dashboard frame — but
  that's a design change to discuss, low priority given supersede-by-composer.

## OTA validity-gate change — SUPERSEDED + HARDWARE-VERIFIED by Roy @ `29b1b0a`
**UPDATE (2026-06-27):** Roy built on my `699ee32` draft (it's in main's
history) and took it further @ `29b1b0a` (2026-06-24): mark-valid now fires at
**core init right after L2CAP**, fully WiFi-independent (even earlier than my
streaming-stage gate); my `confirm_image_valid` demoted to a diagnostic-only
streaming-stage self-test. He also added clean-shutdown (`graceful_shutdown()`
on real critical-battery ≤3250 mV, gated on `Battery::is_real`), the
safe-power-off LED protocol (ShuttingDown/SecuringData/SafeToPowerOff +
data_tcp `LAST_SERVED`), `Ring::sync()`, and serial print of FATAL run() errors
— ported across devkitc/xiao/dfr1117. **VERIFIED ON DEVKITC HARDWARE (both
sensors).** So the OTA anti-brick gate is now metal-proven on the rocker;
the verification item below is RESOLVED. Original draft notes retained for
context:

## OTA validity-gate change (2026-06-16) — DRAFTED → see SUPERSEDED note above
Fix for the revert above + reusable contract for **composer's OTA plugin**
(Roy: workshop WILL be used in the field before composer is done, so this is a
real fix, not just reference). Moves the anti-brick gate off the dashboard.
- **Before:** `mark_app_valid` fired on the **first dashboard frame round-trip**
  → any single transient reset before that frame rolled back a *good* image.
- **After:** validate at the **streaming stage on LOCAL self-proof** (boot + all
  init + WiFi+DHCP + BLE + driver init all done in `Sender::new`), independent
  of dashboard reachability. ESP-IDF's own docs say mark "as early as possible."
- **Files:** shared `crates/r2-esp/src/ota_tcp.rs` gains `pub fn mark_app_valid()`
  (the reusable contract); each carrier's `sender.rs` (devkitc/xiao/dfr1117)
  replaces the old `mark_app_valid` method + `app_validated` field + loop gate
  with `confirm_image_valid()` called at top of `run()`; main.rs comments updated.
- **Trade-off (documented in code):** no longer rolls back an image that boots
  fine but has a broken dashboard-comms layer — that's an environment/CI concern,
  not an anti-brick concern. Residual: a deterministic panic in the steady-state
  send loop won't roll back (but that returns Err → reconnect, doesn't reset).
- **STATUS:** PUSHED to `origin/main` @ `699ee32` (2026-06-17). Compiles clean
  on all 3 carriers (S3 xtensa + C6 riscv), source-clean. **Source only — NOT
  released**; nothing fetches it until a `fw-v` release is cut, which must wait
  on hardware verification on tuxedo (currently OFF). **Verify when tuxedo is up:**
  flash → OTA-push it → confirm the new image marks valid right after WiFi/streaming
  (serial: "[ota-gate] streaming stage reached … validating image") and survives
  a reboot without rolling back. Only then cut `fw-v0.3.1` (or next).

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
   **ARTIFACT PRE-BUILT (2026-06-22, supervisor-sanctioned non-gated prep):**
   `dist/r2-workshop-server-nz-ac-auckland-rocker-0.3.1+ee6ca256-linux-x86_64.tar.gz`
   (sha256 `7ffadc09e664…`, 2.70MB, git ee6ca256; r2-dashboard Hive +
   webapp + WASM pkg + start/install scripts). Verified (sha matches meta). dist/ is
   gitignored → stays LOCAL on Alfred, ready to scp+deploy to tuxedo. NOT published
   (server stream Roy-held — this is the deploy artifact, not a gh release). DEPLOY
   in the coordinated window (tuxedo busy w/ live R2 #14 mesh): scp the tarball →
   extract → tools/start-server.sh (per install-launcher), restart the :21042 dash.
   Awaiting Roy's window (supervisor relaying the 4 gated items to Roy).
   **STAMP NOTE (2026-06-27):** main has since advanced to `975beb9` (Roy's
   firmware-only OTA-gate commit `29b1b0a`). The staged artifact is git
   `ee6ca256` = behind main, but `29b1b0a` is **firmware-only** (dashboard/
   server tarball bundles no firmware) so the artifact CONTENT is unaffected.
   Optional: re-stamp/rebuild at deploy time so the version string tracks the
   deployed git — but not required for a correct dashboard. dist/ also holds
   the matching aarch64 tarball if tuxedo needs it.
3. **DFR1195 (ESP32-S3) Path-B no_std build path** (forward dep, not urgent).
   New target NOT covered by the current Path-A/ESP-IDF matrix
   (devkitc/xiao/dfr1117). Pipeline: hive (no_std esp-hal/embassy source) →
   **workshop adds a no_std esp-hal build path + `fw-vX.Y.Z` release +
   OTA sidecars** → composer OTA push. hive coordinates when its scaffold
   compiles (hive branch `platform-trait`).
   **DEP CLOSER (2026-06-27):** hive's field triplet is now PROVEN ON METAL
   (one-image role-select + §8.1 LoRa beacon + LoRa data-plane; signed
   receiver + confirmed-boot + anti-rollback + otadata slot-switch all
   metal-validated). The OTA networked round-trip is bench-topology-gated
   (isolated soft-AP), NOT a firmware/signing gap. Path-B convergence for
   the rocker is now nearer — still hive-gated + sequenced LAST, but the
   no_std build-path work is no longer blocked on an unproven scaffold.
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
