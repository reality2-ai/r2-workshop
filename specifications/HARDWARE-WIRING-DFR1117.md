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
| Broken-out GPIO | 4, 5, 6, 7, 16, 17, 19, 20, 21, 22, 23 (+ 3V3, GND, VUSB, BAT). GPIO0 = BOOT; GPIO8/9 = strapping (avoid). |
| Deep-sleep wake pins | LP-domain GPIO0–GPIO7 (relevant for the bridge wake-MCU sibling, not the rocker) |

## 2. Pin assignments

| Function | C6 GPIO | Bus / notes |
|---|---|---|
| Status LED (mono) | **GPIO15** | on-board LED1, LEDC PWM (see `src/led.rs`) |
| Battery ADC | **GPIO4** | ADC1-capable, broken out |
| SPI SCK (SD) | **GPIO19** | shared SPI bus |
| SPI MOSI (SD) | **GPIO20** | |
| SPI MISO (SD) | **GPIO21** | |
| SD chip-select | **GPIO23** | |
| I²C SDA (accel) | **GPIO5** | new I²C bus (see §3) |
| I²C SCL (accel) | **GPIO6** | |
| accel INT (optional) | GPIO7 | not on the 4-pin Gravity lead; only if jumpered from the board's INT header. Firmware polls. |
| spare | GPIO22, 16, 17 | GPIO22 freed (was the SPI accel-CS) |

> **Two buses on this carrier.** Unlike the S3 builds (ADXL355 + SD
> share one SPI2 bus), the chosen accelerometer here (SEN0224 / LIS2DH,
> Gravity) is **I²C**, so the accelerometer is on I²C (GPIO5/6) and the
> SD stays on SPI (GPIO19–23).

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

| Gravity pin | → DFR1117 | Note |
|---|---|---|
| VCC | **3V3** | 3.3–5 V; on-board LDO + bidirectional level shifters → clean at 3.3 V (no divider/voltage issue) |
| GND | GND | |
| SCL | **GPIO6** | I²C clock |
| SDA | **GPIO5** | I²C data |

* **I²C address: 0x18 / 0x19** (LIS2DH; SA0 is strapped on the board, so
  fixed — the driver can probe both).
* **10-bit** acceleration data (high-res mode), ±2/4/8/16 g, up to ~5.3 kHz ODR.
* INT1/INT2 are on the board's *separate* header, not on the 4-pin
  Gravity lead; unused for now (the firmware polls).

> **⚠ Firmware driver needed.** The current firmware's sensing path is
> the **ADXL355 over SPI** — it will NOT read a LIS2DH, so until a
> `lis2dh` I²C driver exists (a new sensing plugin providing
> `ai.reality2.cap.accel.triaxial`), the sensor check fails and the
> firmware streams **simulator** data (the on-board LED keeps its gentle
> "degraded-sim" breathe rather than switching to the live heartbeat).
> Writing that driver + adding the I²C bus is the remaining work for
> this build.

> **Not wired here: wake-on-motion.** The LIS2DW12 (SEN0405) and its
> MCU-wake feature are reserved for the separate **bridge-vibration**
> sibling deployment (sleep → wake-on-traffic → record → sleep), not the
> operator-supervised rocker.

## 4. microSD — DFR0229 (Fermion MicroSD module), SPI

| DFR0229 pin | → DFR1117 | Note |
|---|---|---|
| +5 (VCC) | **VUSB** (5 V) | module's on-board 3.3 V LDO needs 5 V in |
| GND | GND | |
| SCK | **GPIO19** | |
| MOSI | **GPIO20** | |
| MISO | **GPIO21** | |
| SS (CS) | **GPIO23** | |

> **⚠ 5 V-logic module on a 3.3 V MCU.** Per its schematic, the DFR0229's
> inputs (SCK/MOSI/CS) pass through 2.2 kΩ/1 kΩ dividers tuned for a 5 V
> Arduino; driven from the C6's 3.3 V they reach the card at **~2.3 V** —
> just above the card's ~2.06 V threshold, so it will likely work but
> with little margin and may be flaky at high SPI clock. If the SD
> misbehaves: lower the SPI clock, or use a 3.3 V-native microSD
> breakout. (MISO from the card is already 3.3 V — fine.) Keep the shared
> SCK/MOSI/MISO stubs short.

## 5. Status LED + battery

* **Status LED:** the on-board **LED1 (GPIO15)** is a plain single-colour
  LED, driven via **LEDC PWM** with the *same* state machine + animation
  timing as the WS2812 carriers — colour dropped, pattern/brightness
  carries the state (`src/led.rs`). LED4 (green) is the TP4057 charge
  indicator and is not under firmware control.
* **Battery:** on-board LiPo charge over USB-C; battery voltage sensed on
  **GPIO4** (ADC1) as on the other carriers.

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

* Carrier **built + flashed + bootstrapped + streaming** (sim data; no
  sensor wired yet) — 2026-05-29.
* TODO: `lis2dh` I²C sensing driver + I²C bus init; an **ADR-003** for
  this carrier (RISC-V SoC family — per the "Adding a new carrier"
  guidance in `HARDWARE-WIRING.md`, a different SoC family warrants a
  documented decision); and the (class, carrier) matched-OTA safety work
  (#88–91) before OTA is safe on this mixed xtensa/RISC-V fleet.

## See also

* `HARDWARE-WIRING.md` — carrier index
* `decisions/ADR-001-xiao-esp32-s3-carrier.md` — carrier-choice rationale + ESP32-C6 discussion
* `docs/datasheets/` — DFR1117 / DFR0229 / SEN0168 datasheets + schematics
* `SPEC-R2-WORKSHOP-SENSOR.md` — carrier-agnostic firmware behaviour
