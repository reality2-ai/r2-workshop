---
title: r2-workshop — Hardware wiring (DFRobot Beetle ESP32-C6 / DFR1117)
status: Supported alternative carrier — RISC-V (ESP32-C6)
date: 2026-05-29
---

# r2-workshop — DFR1117 (Beetle ESP32-C6) wiring

Parallel carrier wiring guide, peer to `HARDWARE-WIRING-DEVKITC.md` and
`HARDWARE-WIRING-XIAO.md`. The protocol + firmware-spec layer is
unchanged (`SPEC-R2-WORKSHOP-SENSOR/WIRE/DASHBOARD`); only the board,
pin assignments, and — for this carrier — the **SoC family** differ.

## 1. Board overview

DFRobot **Beetle ESP32-C6 (DFR1117)** — a coin-sized board around the
**ESP32-C6** (single-core 160 MHz **RISC-V**, Wi-Fi 6 / BLE 5 /
802.15.4). Distinct from the two ESP32-S3 carriers, which are xtensa.

| Property | Value |
|---|---|
| SoC | ESP32-C6-FH4 (RISC-V, single-core) |
| Flash / PSRAM | **4 MB / none** |
| USB | USB-C, native USB-Serial-JTAG (flash + console on one cable) |
| Power | USB-C 5 V; **on-board LiPo charge** (TP4057) + 3.3 V regulator |
| On-board LEDs | **LED1 = user LED, GPIO15** (plain blue, single-colour); **LED4 = LiPo charge status** (TP4057-driven, *not* software-controllable) |
| Broken-out GPIO | 4, 5, 6, 7, 16, 17, 19, 20, 21, 22, 23 (+ `3V3`, `GND`, `VIN` (5 V), `BAT`). GPIO0 = BOOT; GPIO8/9 = strapping (avoid). |
| Deep-sleep wake pins | LP-domain GPIO0–GPIO7 (relevant for the bridge wake-MCU sibling, not the rocker) |

## 1.1 Wire by the printed silk label

Every pad on the board's edge headers is **silk-labelled** — power names
(`3V3`, `GND`, `BAT`, `VIN`) and `IOnn` GPIO numbers. **Wire by the
printed label**; you don't need a physical position map, and this doc
deliberately doesn't assert one (see the official pinout diagram for
physical layout: <https://wiki.dfrobot.com/dfr1117/>).

The board's signal pads carry **function-name silk** (`SDA`, `SCL`,
`SCK`, `MO`, `MI`, `RX`, `TX`, `LP_*`), not raw `IOnn`. The label → GPIO
map below is **verified** against the official `dfrobot_beetle_esp32c6`
Arduino variant (`espressif/arduino-esp32`); the firmware pins (§2) are
chosen to match it, so you wire **pad-to-label**:

| Silk label | GPIO | | Silk label | GPIO |
|---|---|---|---|---|
| `SDA` | 19 | | `SCK` | 23 |
| `SCL` | 20 | | `MO` (MOSI) | 22 |
| `RX` | 17 | | `MI` (MISO) | 21 |
| `TX` | 16 | | `LP_SCL` | 7 |
| `LED` (on-board) | 15 | | `LP_RX` | 4 |

Other verified facts:

* `VIN` = the **5 V input** rail (USB / external 5 V). The SD module's
  +5 goes here. (The schematic net name is `VUSB`; the board silk reads
  `VIN`.)
* Power pads: `3V3`, `GND`, `BAT` (on-board LiPo connector), `VIN`.
* Onboard user LED = `IO15` (not on a header pad); BOOT button = `IO9`.
* Confirmed against the board (2026-05-29): left edge top→bottom
  `GND, 3V3, LP_RX, LP_TX, SCK, MO, MI, LP_SCL`; right edge top→bottom
  `BAT, GND, VIN, RX, TX, SDA, SCL, LP_SDA`.

## 2. Pin assignments

Firmware pins (`src/main.rs`) match the board silk — wire each
peripheral lead to the pad with the same label.

| Function | GPIO | Solder to pad | Bus / notes |
|---|---|---|---|
| Status LED (mono) | **GPIO15** | (on-board) | LED1, LEDC PWM (`src/led.rs`) — not on a header pad |
| Battery ADC | **GPIO4** | `LP_RX` | ADC1; needs the §5 divider to read the cell |
| SPI SCK (SD) | **GPIO23** | `SCK` | shared SPI bus |
| SPI MOSI (SD) | **GPIO22** | `MO` | |
| SPI MISO (SD) | **GPIO21** | `MI` | |
| SD chip-select | **GPIO7** | `LP_SCL` | free pad next to `MI` (no dedicated `CS` pad on this board) |
| I²C SDA (accel) | **GPIO19** | `SDA` | accel I²C bus (see §3) |
| I²C SCL (accel) | **GPIO20** | `SCL` | |
| spare | GPIO5, 6, 16, 17 | `LP_TX`,`LP_SDA`,`TX`,`RX` | available |

> **Two buses on this carrier.** Unlike the S3 builds (ADXL355 + SD
> share one SPI2 bus), the chosen accelerometer here (SEN0224 / LIS2DH,
> Gravity) is **I²C**, so the accelerometer is on I²C (`SDA`/`SCL`) and
> the SD stays on SPI (`SCK`/`MO`/`MI` + `LP_SCL` for CS).

## 3. Accelerometer — SEN0224 (ST LIS2DH), Gravity I²C

Chosen for the rocker C6 build (2026-05-29). It ships with a **4-pin
Gravity I²C connector + flying lead** (plug-and-play, no soldering on
the sensor side), and at **10-bit** it sits well below the rig's
ADXL355 (20-bit): wiring it lets the project test *whether the ADXL355's
high sensitivity is actually required* to catch joint-failure
precursors, or whether coarser data suffices. (For an even coarser
low-end, the SEN0168 / BMA220 is 6-bit — but it lacks the Gravity
connector and needs soldering.)

Wire the SEN0224's 4-pin Gravity lead to:

| Gravity pin | → board pad | GPIO | Note |
|---|---|---|---|
| VCC | `3V3` | — | 3.3–5 V; on-board LDO + level shifters → clean at 3.3 V |
| GND | `GND` | — | |
| SCL | `SCL` | GPIO20 | I²C clock |
| SDA | `SDA` | GPIO19 | I²C data |

* **I²C address: 0x18 / 0x19** (LIS2DH; SA0 is strapped on the board, so
  fixed — the driver can probe both).
* **10-bit** acceleration data (high-res mode), ±2/4/8/16 g, up to ~5.3 kHz ODR.
* INT1/INT2 are on the board's *separate* header, not on the 4-pin
  Gravity lead; unused for now (the firmware polls).

> **Firmware driver:** the `lis2dh` I²C driver (`src/lis2dh.rs`, a
> sensing plugin providing `ai.reality2.cap.accel.triaxial`) is
> implemented. With no SEN0224 wired the I²C probe fails gracefully and
> the firmware streams **simulator** data (LED holds the gentle
> "degraded-sim" breathe); plug the sensor in and reset and it switches
> to the live heartbeat.

> **Not wired here: wake-on-motion.** The LIS2DW12 (SEN0405) and its
> MCU-wake feature are reserved for the separate **bridge-vibration**
> sibling deployment (sleep → wake-on-traffic → record → sleep), not the
> operator-supervised rocker.

## 4. microSD (SPI)

**Decision: use a 3.3 V-native microSD board, powered from `3V3`.** It is
the only option that works in *every* power mode, because of which rails
are live when:

| Rail | Live when… |
|---|---|
| `VIN` | USB plugged **only** (it's the USB 5 V) |
| `BAT` | a battery is **connected** (USB + no battery → just the charger's unloaded output, not a dependable rail) |
| `3V3` | **always** — regulated buck output that runs the MCU, fed from USB *or* battery automatically |

A 3.3 V-native board runs off `3V3`, so it works tethered, on battery,
or both. A 5 V module can't: its BL8555 LDO needs **≥~3.5 V**, so it can
*only* take `VIN` (USB-only) or `BAT` (battery-only) — there is no single
rail that powers it in all modes.

> **⚠ Powering the SD on battery.** The `VIN` pad is the **USB 5 V rail**
> (Type-C VBUS) — **dead on battery** (the C6 runs off the LiPo via its
> on-board 3.3 V buck, but nothing re-creates 5 V). So never power a
> battery-build SD module from `VIN`. Two workable options:
>
> 1. **Cleanest — a 3.3 V-native microSD breakout** (bare socket +
>    pull-ups, no LDO, no level-shifter) powered from the **`3V3`** pad
>    (regulated, present on USB *and* battery). Avoids both gotchas below.
> 2. **The 5 V DFR0229 / "MicroSD Module V1.0" can run on battery via
>    `BAT`.** Its regulator is a **BL8555-33** (datasheet-verified):
>    dropout **0.22 V typ / 0.35 V max @ 120 mA**, so from a LiPo on `BAT`
>    (4.2→~3.5 V) it holds a clean 3.3 V to the card across essentially
>    the whole discharge (sagging gracefully, still in the card's
>    2.7–3.6 V spec, near empty). Wire `VCC`→**`BAT`** (not `VIN`). Two
>    caveats remain: the BL8555 is only a **150 mA** LDO (SD write spikes
>    can brush that → brownout/write-error risk; add a bulk cap + modest
>    SPI clock), and the input **dividers** still drop SCK/MOSI/CS to
>    ~2.27 V (marginal). Confirm the module's regulator really is a
>    BL8555 (an AMS1117 needs ~4.5 V and won't hold 3.3 V from a LiPo).
>
> The SD is the on-device ring/capture store only — the sensor streams
> over Wi-Fi regardless, so it is **not** required for live data.

The SPI logic pins are the same for any microSD module. A typical
breakout labels its data pins `SO` (= card data-out = MISO) and `SI`
(= card data-in = MOSI):

| SD module pin | → board pad | GPIO | Note |
|---|---|---|---|
| `VCC` | **`3V3`** | — | 3.3 V-native module (see warning above) |
| `GND` | `GND` | — | |
| `SCK` | `SCK` | GPIO23 | |
| `SI` (MOSI) | `MO` | GPIO22 | |
| `SO` (MISO) | `MI` | GPIO21 | |
| `CS` | `LP_SCL` | GPIO7 | free pad adjacent to `MI` |

Keep the `SCK`/`MO`/`MI` stubs short.

> **Where to feed VCC — depends on the module:**
> * **5 V module *with* its own LDO (DFR0229 / "MicroSD Module V1.0"):**
>   `VCC`→**`BAT`** (raw LiPo) for battery, or `VIN` on USB. Its BL8555-33
>   LDO needs **≥~3.5 V in** to make 3.3 V, so a flat `3V3` supply would
>   under-volt the card — feed it the raw battery, not `3V3`.
> * **3.3 V-native breakout (no regulator):** `VCC`→**`3V3`** only.
>   **Never** feed it raw `BAT` (4.2 V over-volts and can kill the card).
>
> **Schematic detail (DFR0229 / V1.0)**, verified from
> `docs/datasheets/DFR0229-microsd-module-schematics.pdf` (titled
> *"MicroSD Module V1.0"*): `VCC` feeds the **BL8555-33 LDO** (card supply
> is internal, not exposed); `SCK`/`MOSI`/`CS` each pass through a
> **1 kΩ series + 2.2 kΩ-to-GND divider** (×0.69). From 5 V that's 3.44 V
> at the card; from the C6's 3.3 V logic it's **~2.27 V** — just over the
> ~2.06 V threshold (marginal; lower the SPI clock if flaky). This divider
> behaviour is independent of how `VCC` is powered. `MISO` passes straight
> through. (So third-party "3.3 V–5 V" listings mean *via the LDO* — it
> still needs ≥~3.5 V in, which a LiPo on `BAT` provides but a flat 3.3 V
> does not.)

## 5. Status LED + battery

* **Status LED:** the on-board **LED1 (GPIO15)** is a plain single-colour
  LED, driven via **LEDC PWM** with the *same* state machine + animation
  timing as the WS2812 carriers — colour dropped, pattern/brightness
  carries the state (`src/led.rs`). LED4 (green) is the TP4057 charge
  indicator and is not under firmware control.
* **Battery:** the board has an **on-board BAT connector** — plug a
  single-cell LiPo straight in (no soldering; mind the connector keying /
  polarity). The on-board **TP4057 charges it from USB-C** automatically,
  and the board runs from the cell when the cable is out. LED4 (green) is
  the charge indicator.
* **Battery sense (optional).** The `BAT` rail is **not** internally wired
  to an ADC, so without a divider GPIO4 floats and the firmware reports
  *simulated* battery (`battery.rs` rejects the implausible/noisy reading
  and falls back to `BatterySim`). To get a real cell reading, fit a
  **0.5 divider into GPIO4** — the firmware does `v_cell = adc_mv × 2`
  (ADC1_CH3, 12-bit, 11 dB atten):

  ```
   BAT ──[100kΩ]──┬──[100kΩ]── GND
                  │
              `LP_RX` pad (GPIO4)   ← also 100nF from this node to GND
  ```

  The **100 nF cap on the GPIO4 midpoint is required** — without it the
  ADC sample-and-hold can't settle on the high-impedance divider, the
  per-reading spread exceeds the plausibility gate, and the firmware
  stays on `BatterySim`.

## 6. Firmware / toolchain

* Crate: `firmware/esp32-c6/dfr1117/` — target **`riscv32imac-esp-espidf`**,
  MCU `esp32c6`, ESP-IDF 5.2.5, `esp` rust-toolchain.
* Build: **`tools/build-firmware.sh dfr1117`** (carrier-aware).
* Partition table: 4 MB two-OTA (1.875 MB slots, no internal FAT storage —
  captures go to the SD). The **first install must be a full USB flash**
  (`espflash flash`) and **must pass the partition table explicitly**
  (`--partition-table partitions.csv --bootloader <built bootloader.bin>`)
  — espflash 4.x otherwise writes a default single-`factory` table that
  won't boot. OTA only works after the first full flash.

## 7. Status / follow-ups

* Carrier **built + flashed + bootstrapped + streaming**; `lis2dh` I²C
  driver done; pins re-mapped to the board silk; **OTA verified
  end-to-end** (2026-05-29). Streams sim data until a SEN0224 is wired.
* TODO: an **ADR-003** for this carrier (RISC-V SoC family — per the
  "Adding a new carrier" guidance in `HARDWARE-WIRING.md`, a different
  SoC family warrants a documented decision); and the (class, carrier)
  matched-OTA safety work (#88–91) before OTA is safe on this mixed
  xtensa/RISC-V fleet.

## See also

* `HARDWARE-WIRING.md` — carrier index
* `decisions/ADR-001-xiao-esp32-s3-carrier.md` — carrier-choice rationale + ESP32-C6 discussion
* `docs/datasheets/` — DFR1117 / DFR0229 / SEN0168 datasheets + schematics
* `SPEC-R2-WORKSHOP-SENSOR.md` — carrier-agnostic firmware behaviour
