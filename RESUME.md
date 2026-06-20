# RESUME — r2-workshop (workshop-worker)

_Owned by this session. Keep current. Last updated 2026-06-09._
Master save (read-only): `r2-fleet/fleet-context/FLEET-CONTEXT-SAVE.md` (+ plan + DEV_STATUS).
(Relocated 2026-06-18 from `claude-fleet/fleet-context/`; claude-fleet is now tooling-code-only.)

## ACTIVE: TRUE TN — RouteEngine over my transports (branch `tn-routeengine-bringup`)
**Frame ROUTES board-to-board through core's RouteEngine — incl. over REAL UDP
sockets — done on Alfred, pushed.** Branch HEAD `ff01a04`.
- **`crates/r2-tn` (new, host-buildable) — `cargo test -p r2-tn` = 5/5:**
  - `RouteNode<N,P,D>` driver glue (+ `McuRouteNode = <16,16,32>`): `originate()`
    + `on_inbound()` (parse → deliver-if `target_hive==self` → learn-peer +
    reverse-path → `plan_forward` → `prepare_relay_extended` → `Transport::send`;
    flood excludes the inbound peer). Tests: direct A→B, 2-hop relay A→R→C
    (asserts TTL−1), not-for-us.
  - `udp::UdpTransport` (pure-std `r2-transport::Transport`): send/broadcast,
    non-blocking recv, hive_id↔addr both ways. Test `routes_a_to_b_over_real_udp`
    routes a frame A→B over **actual loopback UDP**.
  - `node::Node`/`McuNode` (firmware-facing): `poll(now)`→`Delivered` +
    `originate(...)`. Test `node_api_routes_a_to_b_over_udp` over real UDP.
- **`crates/r2-esp/peer_wifi_udp.rs`:** re-exports `r2_tn::udp::UdpTransport as
  WifiUdpTransport` (single source). r2-esp deps r2-route/r2-transport/r2-tn.
  **devkitc `cargo check` (ESP32-S3) green** with the full dep graph.
- **Seam = core's, applied verbatim:** originate TTL=5, K=15 (FLOOD_SENTINEL —
  floods new dest, downgrades to Directed once a path is learned; NOT K=1=hold),
  full-u32 `target_hive` addressing, `source_hop=(id>>16)` INLINED (canonical
  r2-wire has no `compress_hive_id_16`; our vendored copy is forked — watch the
  re-sync), relay via `prepare_relay_extended`. Ref loop `r2-harness::MeshNode`.
- **DONE — firmware run-loop wired** (`069ed0e`): `r2_esp::tn::spawn(TnConfig)`
  (feature `tn`, off by default → production build byte-identical) binds
  `WifiUdpTransport` on the SoftAP IP, builds `McuNode`, seeds static peers,
  runs `poll`/`originate` on a thread. devkitc `tn_start()` derives my_hive_id
  (FNV of §6.2.1 hive_id UUID) + reads `R2_TN_PEER_ID/IP/PORT/ORIGINATE` via
  `option_env!`. **Verified ESP32-S3: `cargo check` default AND `--features tn`
  both green.**
- **DONE — DFR1195 carrier built + flash-ready** (`a0a1e1c`): `firmware/esp32-s3/dfr1195`,
  a minimal TN-only carrier. Boards on ttyACM9/10/11 probed via `espflash board-info`:
  **ESP32-S3, 4MB, no-PSRAM** (MACs f4:12:fa:b6:0a:a0 / 52:99:28 / 50:23:e4).
  Config: esp32s3 / FLASHSIZE_4MB / NO SPIRAM / 4MB 2-OTA table / r2-esp `tn`
  no-`ble`. **Board-hosted SoftAP + role-by-MAC** (ONE image): board whose MAC ==
  `R2_TN_AP_MAC` hosts the AP (`r2_esp::wifi_ap`, default SSID r2-tn-lab) + listens;
  others join STA + originate to the AP → STA→AP delivery = the hardware frame.
  Aligned to hive field.lab: UDP **21042**, matching partition table.
- **HARDWARE FIRST-LIGHT — RUNNING ON REAL BOARDS (link-gated on final confirm).**
  Flashed ttyACM9 (AP, f4:12:fa:b6:0a:a0) + ttyACM10 (STA) over SSH→tuxedo with
  the role-by-MAC image (`R2_TN_AP_MAC` baked). **Serial-confirmed over the real
  radio:** AP boots → SoftAP `r2-tn-lab` up → TN node on 192.168.71.1; STA boots →
  **joins the AP** → IP 192.168.71.2 → **originates R2-WIRE frames every 3s**
  through RouteNode (`[tn] originated ev=8127232d -> next_hop 4767b7f3`). Board-to-
  board TN over real WiFi works end-to-end **except final delivery**.
- **BUG FOUND + FIXED (`9c5709f`, blob sha b5749be8):** STA targeted **192.168.4.1**
  (hive's *embassy-net* AP IP) but **esp-idf-svc's SoftAP is 192.168.71.1** → frames
  missed the AP. Fixed: `wifi_sta::get_gateway()` — STA targets its actual gateway
  (= the AP board) regardless of stack. Rebuilt + re-flashed both boards.
- **SPEC finding (route to specs):** the SoftAP/TN AP IP is **platform-stack
  dependent** (esp-idf-svc 192.168.71.1 vs embassy-net 192.168.4.1) — interop needs
  the AP IP discoverable (gateway), not assumed.
- **NEXT — confirm DELIVERED (link-blocked, not code-blocked):** tuxedo's Tailscale
  link went too flaky to hold a serial-monitor session after the re-flash. To finish:
  on tuxedo, monitor the AP `espflash monitor --port /dev/ttyACM9` while both boards
  run → expect `[tn] DELIVERED ev=8127232d … HARDWARE FRAME`. (Boards already hold
  the fixed image b5749be8; just need a stable monitor window or someone at tuxedo.)
  Merged blob: `/tmp/dfr1195-merged.bin` (Alfred + tuxedo). For 3 boards = 1 AP +
  2 STA; STA↔STA may be client-isolated → AP relays (RouteNode does).
- **THEN — trust tier** (core's TRUST-INTEGRATION-BRIEF @ r2-core 905502c): add the
  trust gate at RouteNode's DELIVER branch only (relay stays trust-agnostic):
  intra-TG GroupHmac verify; inter-TG PeeringHmac entanglement. Host-testable in
  r2-tn first.
- **DONE — delivery-dedup** (`c64a354`): NOT a spec change (core: spec already
  answers it). RouteNode gates the whole message on `(msg_id, SOURCE)` first;
  SOURCE = route_stack[0] (R2-WIRE §3.3) else transport source for a direct frame.
  Separate DedupCache from the engine's relay-dedup. r2-tn 6/6 green + ESP-compiles.
- **Interop-canon flagged to specs (spec-first):** (a) `target_hive` =
  FNV-1a(§6.2.1 hive_id_uuid) — TG-scoped, required at the trust tier; both
  workshop-dfr1195 (FNV full-MAC) + hive (MAC-low3) must converge from their
  shortcuts. (b) TN AP-IP must be discoverable ("AP = the gateway"), not constant.
- **Working mode:** any on-hardware divergence → refine SPEC first (specs), then code.
- **Doctrine:** [[fleet-operating-doctrine]] — branch experiment; not for `main`
  until proven on hardware. The OTA-gate fix (`699ee32`) is already on `main`.

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

## OTA validity-gate change (2026-06-16) — DRAFTED, hardware-unverified
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
