---
title: ADR-003 — Add DFRobot Beetle ESP32-C6 (DFR1117) as a RISC-V carrier
status: Accepted
date: 2026-05-31
supersedes: none
amends: HARDWARE-WIRING.md (adds C6 carrier row); SPEC-R2-WORKSHOP-SENSOR §1 (generalised "ESP32-S3 only" to "any SoC family the firmware tree supports")
superseded-by: none
---

# ADR-003 — Add DFRobot Beetle ESP32-C6 (DFR1117) as a RISC-V carrier

## Status

**Accepted** — 2026-05-31, released as part of v0.3.0.

## Context

ADR-001 (XIAO) and ADR-002 (revert default to DevKitC) both stayed
inside the **ESP32-S3 / xtensa** family. The firmware tree's
toolchain, build script, and the SPEC-R2-WORKSHOP-* layer were all
written assuming "ESP32-S3" — SPEC-R2-WORKSHOP-SENSOR §1 said
explicitly that adding a new carrier "requires no spec changes so
long as the carrier's SoC is ESP32-S3". A different SoC family was
flagged as needing a new ADR.

In late May 2026 three things made it worth crossing that line:

1. **DFRobot Beetle ESP32-C6 (DFR1117)** arrived on the bench. It's
   a coin-sized board with on-board LiPo charge (TP4057), USB-C +
   native USB-Serial-JTAG, a mono status LED on GPIO15, and the
   same R2-stack-needed peripherals (SPI, I²C, ADC) the S3 carriers
   use — but built around the **ESP32-C6** (RISC-V, single-core
   160 MHz, 4 MB flash / no PSRAM, Wi-Fi 6 / BLE 5 / 802.15.4).

2. **The R2 stack already runs cross-arch.** All the vendored
   `crates/r2-*` build for both xtensa and RISC-V; ESP-IDF 5.2.5
   supports both via the same `esp` Rust toolchain (`espup install`
   installs both). The C6 bring-up turned out to need zero R2-layer
   changes — only carrier config (target triple, MCU id,
   partition table, pin literals).

3. **A different sensor on the same carrier is a useful exercise.**
   The C6 build is the first time we swap out the ADXL355 for a
   different accelerometer chip (LIS2DH, SEN0224 / Gravity I²C),
   which is the first concrete demonstration of the "sensing
   plugin" abstraction the SPEC-R2-WORKSHOP-SENTANTS document calls
   for. The lis2dh driver implements the same `read_xyz_lsb()`
   contract behind a different bus (I²C) — sender + sim-fallback
   path unchanged.

## Decision

**Add `firmware/esp32-c6/dfr1117/` as a third parallel-supported
carrier alongside `firmware/esp32-s3/devkitc/` and
`firmware/esp32-s3/xiao/`.** The DevKitC remains the current default
(per ADR-002); the C6 is an alternative.

Specific commitments:

* **SoC family generalisation.** Update
  SPEC-R2-WORKSHOP-SENSOR §1 to say "any SoC family the firmware
  tree's existing toolchains support — currently ESP32-S3 (xtensa)
  and ESP32-C6 (RISC-V)". The firmware tree layout becomes
  `firmware/<soc-family>/<carrier>/`.
* **Carrier slug naming open.** SPEC-R2-WORKSHOP-WIRE §3.1 row 12
  (announce `carrier` field) is an open enumeration; the firmware
  tree's per-carrier directory name is the canonical slug. No
  spec bump is needed when a new carrier lands — only a wiring doc
  and a `stamp_sensor_carrier()` in `build.rs`.
* **Carrier-specific sensing plugin.** Each carrier owns its
  `sensing/` driver. The C6 uses `lis2dh` over I²C (SEN0224 /
  Gravity); the S3 carriers use `adxl355` over SPI. The
  `read_xyz_lsb()` contract is identical (1 g = 256 000 LSB), so
  the sender thread + sim fallback are unchanged.
* **Build script becomes carrier-aware.** `tools/build-firmware.sh
  <carrier>` dispatches on the carrier slug to pick target triple
  (`xtensa-esp32s3-espidf` vs `riscv32imac-esp-espidf`) and MCU id
  (`esp32s3` vs `esp32c6`). Same pattern for
  `tools/setup-firmware.sh`.
* **Pin map matches the board silk.** The C6's pads are
  silk-labelled by function (`SDA`, `SCL`, `SCK`, `MO`, `MI`,
  `LP_*`), not by `IOnn`. The firmware pin literals are chosen so
  peripherals wire **pad-to-label** — verified against the
  official `dfrobot_beetle_esp32c6` Arduino variant under
  `espressif/arduino-esp32`. See `HARDWARE-WIRING-DFR1117.md` §1.1.
* **Battery telemetry is carrier-specific in code, identical in
  contract.** Each carrier's `battery.rs` hardcodes the divider
  GPIO (DevKitC: GPIO4; DFR1117: GPIO4 = `LP_RX` pad — same number,
  different chip; XIAO: not yet allocated → `BatterySim`). Module
  doc comments spell out the per-carrier rule
  ([[feedback_no_guessing]]). The §5 divider (100 kΩ / 100 kΩ +
  100 nF) is identical across carriers.

## Consequences

**Positive:**

* **First RISC-V deployment of the R2 stack.** Proves the protocol +
  vendored crates are SoC-family-agnostic. Useful precedent if a
  future deployment wants ESP32-P4 / RP2350 / nRF / other.
* **Plugin abstraction now has a working example.** The lis2dh
  swap demonstrates concretely that a different chip on a different
  bus can stand in behind the same `read_xyz_lsb()` contract — what
  SPEC-R2-WORKSHOP-SENTANTS describes in spec form, now shown to
  work.
* **Test bench for fidelity-versus-cost tradeoffs.** The LIS2DH is
  10-bit (versus the ADXL355's 20-bit); running the same rocker
  experiment on both lets the project measure how much sensor
  fidelity the structural-health classifier actually needs, before
  committing the final deployment hardware. This was a deliberate
  research question the C6 trial was sized to answer.
* **Heterogeneous-fleet matched OTA forced honestly.** Mixing a
  RISC-V sensor into a previously S3-only fleet exposed every
  carrier-blind assumption in the OTA path — `class` + `carrier`
  in the announce (WIRE §3.1 keys 11/12), per-carrier asset
  selection in the dashboard + webapp, manual-upload validation
  gate (#91, pending). Those gaps existed before; the C6 made them
  unavoidable to fix.

**Negative / costs:**

* **Two toolchains, two build paths.** `espup install` covers both,
  but cargo's build cache is per-target so cold builds touch the
  C6 *and* S3 trees. Mitigated by `build-firmware.sh` building
  only the carrier specified.
* **Per-carrier `battery.rs` / `led.rs` divergence widens.** The
  C6's mono-LED `led.rs` and its slightly different battery-pin
  comments are intentionally carrier-specific copies. The
  long-term direction is a shared `led` crate with per-carrier
  backends (project memory has this as a non-blocking follow-up).
* **The C6's 5 V-design SD modules don't survive battery
  operation.** The DFR1117's `VIN` is the USB 5 V rail and is dead
  on battery; the existing DFR0229 / "MicroSD Module V1.0" 5 V
  modules need either a BAT-powered LDO path (works for the
  BL8555-33 inside the DFR0229) or — cleanly — a 3.3 V-native
  microSD breakout powered from `3V3`. Documented in
  `HARDWARE-WIRING-DFR1117.md` §4.

**Neutral:**

* The C6 keeps the same R2-WIRE / R2-TRUST / R2-CBOR / R2-ROUTE
  semantics as the S3 carriers — observers (dashboard, webapp,
  another sensor) cannot tell at the wire layer which SoC family a
  given peer is, except by reading the announced `carrier` slug.
  That's the entire point.

## Alternatives considered

* **Keep the fleet ESP32-S3-only.** Easy, but it postpones every
  question the cross-arch move forces. Picked against because the
  hardware was on hand and the protocol stack was already general.
* **Custom PCB around the ESP32-C6-WROOM module instead of the
  DFR1117 carrier.** Lower BOM in volume, no DFRobot dependency,
  but a custom PCB is not in scope for the v0.3 timeline. The
  DFR1117 is a "buy on Digikey, solder up tonight" option.
* **Use the LIS2DW12 (SEN0405) instead of the LIS2DH (SEN0224).**
  Considered briefly; the LIS2DW12 is reserved for a sibling
  bridge-vibration deployment that needs its wake-on-motion
  feature. The rocker rig is operator-supervised so wake-on-motion
  isn't useful here, and the LIS2DH's Gravity connector is the
  plug-and-play win.

## Implementation status

Released as **v0.3.0** (2026-05-31). Bench-validated end-to-end:

* `firmware/esp32-c6/dfr1117/` builds clean in release mode,
  flashes over USB-Serial-JTAG, joins WiFi, announces with
  `carrier="dfr1117"`, streams real triaxial data from the LIS2DH,
  reads battery telemetry from the GPIO4 divider, and accepts OTA
  pushes from the dashboard's GitHub-Releases path.
* All three carriers ship from the same v0.3.0 tag: clean
  release-mode builds, matched-naming `.bin` + `.meta.json` assets
  on the GitHub Release per the SPEC-R2-WORKSHOP-DASHBOARD §13.3
  convention.

## References

* `HARDWARE-WIRING-DFR1117.md` — the C6 carrier's wiring document
  (pin map, sensor, SD, battery).
* `SPEC-R2-WORKSHOP-SENSOR.md` §1 — SoC family generalisation.
* `SPEC-R2-WORKSHOP-WIRE.md` §3.1 row 12 + the v0.3.1 / v0.3.2 change-log
  entries.
* `SPEC-R2-WORKSHOP-DASHBOARD.md` §13.3 — local-fallback scanner
  generalised to walk every `firmware/<soc-family>/<carrier>/releases/`
  tree.
* `firmware/esp32-c6/dfr1117/build.rs::stamp_sensor_carrier`,
  `…/src/main.rs` — carrier-stamp + pin literals.
* `firmware/esp32-c6/dfr1117/src/lis2dh.rs` — the I²C sensing
  plugin.
* `espressif/arduino-esp32` `dfrobot_beetle_esp32c6` variant —
  authoritative label-to-GPIO map used to verify the firmware pins.
