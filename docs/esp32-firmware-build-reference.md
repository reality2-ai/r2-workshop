# ESP32 firmware + build reference (for r2-hive's general MCU hive)

A reference for r2-hive, which is generalizing an ESP32-S3 (DFR1195) hive
firmware. r2-workshop is the embedded proving-ground; this captures the
toolchain, structure, and OTA/TCP patterns, and — honestly — **what ports
cleanly vs what diverges from the no_std north-star.**

## Headline: read this first

Per the north-star, the general hive firmware is **Path B pure no_std**
(core no_std crates + a thin no_std platform layer). **r2-workshop's firmware
is Path A: std-on-ESP-IDF** (`esp-idf-svc` with the `std` feature, `fn main()`,
`std::net::TcpStream`, NimBLE, ESP-IDF WiFi/TCP/FAT).

BUT workshop already has the **exact seam** the north-star wants:

```
  r2-core / r2-wire / r2-cbor / r2-fnv / r2-trust   ← no_std protocol (alloc), reused as-is
        ▲ wrapped by
  crates/r2-esp                                      ← the per-platform layer
   (beacon, l2cap, wifi_sta, wifi_prov, ota_tcp,        (today: ESP-IDF/std glue)
    data_tcp, reset_tcp, log_tcp, hive_id)
        ▲ orchestrated by
  firmware/esp32-s3/<carrier>/src/main.rs            ← board/app wiring
```

So the **protocol crates and the `r2-esp` module decomposition are the reusable
asset**; only `r2-esp`'s *internals* are std/ESP-IDF and would be reimplemented
against no_std HALs (esp-hal + esp-wifi + embassy + smoltcp) for Path B. The
*API surface* of `r2-esp` is the platform-layer interface hive should generalize.

## Toolchain + build pipeline

- **Toolchain**: Espressif Rust fork via `espup install` → `channel = "esp"`
  (per-carrier `rust-toolchain.toml`). Targets: `xtensa-esp32s3-espidf` (S3),
  `riscv32imac-esp-espidf` (C6). `source ~/export-esp.sh` before building.
- **ESP-IDF**: pinned `ESP_IDF_VERSION = "v5.2.5"` in each carrier's
  `.cargo/config.toml`; fetched/built by `esp-idf-sys` on first build (slow,
  one-time, per build dir).
- **Build script**: `tools/build-firmware.sh <carrier>` →
  `cargo build --release` → `espflash save-image --chip <c>
  --partition-table <carrier>/partitions.csv --flash-size <8mb|4mb> <elf> <bin>`
  → archive `.bin` + emit a `.bin.meta.json` sidecar
  `{class,carrier,version,git,sha256,built}`.
  - **Gotcha 1 (fresh checkout)**: the first `cargo build` fails on a missing
    `partitions.csv` (esp-idf-sys CMake configures before our `build.rs` copies
    the CSV). Fix: `tools/setup-firmware.sh` stages it into the build dir, then
    rebuild. A generic build orchestrator should do build → on that ninja error,
    setup → rebuild.
  - **Gotcha 2 (espflash versions)**: `save-image` MUST get `--partition-table`
    + `--flash-size` or it validates against espflash's defaults and rejects the
    image (3.x: 1 MB app slot; 4.x: 4 MB flash). Carry both flags.
- **Partition layout**: two-slot OTA (`ota_0`/`ota_1` + `otadata`), no factory.
  Per-board flash: 8 MB S3 → 3 MB slots + internal FAT `storage`; 4 MB C6 →
  1.875 MB slots, NO internal FAT (SD card instead). **First flash of a fresh
  board must be USB** (`espflash flash`) to write the table + both slots; OTA
  only works after that.

## Device firmware structure (sensor node)

`firmware/esp32-s3/<carrier>/src/`: `main.rs` (orchestration), `identity.rs`
(NVS-persistent Ed25519 device key + KeyHolder cert), `sender.rs` (the
R2-WIRE-over-TCP client), `ring.rs` + `sd.rs` (durable SD ring buffer +
replay), `capture.rs`, `clock.rs` (time-sync), `battery.rs`, `led.rs`
(status FSM), `<sensor>.rs` (driver: `adxl355` SPI / `lis2dh` I²C), `wire.rs`
(compact-frame encode), `sim.rs` (synthetic data fallback).

Boot flow: **BLE bootstrap** (R2-BEACON advert + L2CAP receive of a signed
`#wifi_offer`, persisted to NVS) → **WiFi join** → **TCP connect to
`gateway:21042`** → signed `r2.sensor.announce` → stream `r2.sensor.acceleration`
@100 Hz + `r2.sensor.battery`; samples land in the SD ring first (durable),
the network task drains it and frees on `r2.dash.ack {through_seq}`.

## Device-side OTA + TCP-receiver patterns (`crates/r2-esp`)

These are the most directly portable pieces — small, single-purpose TCP
servers/clients on the device, each wrapping a no_std protocol:
- **`ota_tcp`** — OTA receiver: reads `[0x01][size u32 LE][sha256 32 raw][fw…]`
  + half-close, streams into the ESP-IDF OTA partition pair, verifies, sets the
  boot slot, replies `[status u8][len u16 LE][utf8 msg]`, reboots. Exposes
  `ota_in_progress()` (the LED FSM polls it). *(Wire matches R2-UPDATE
  §3.1.2.2.)*
- **`data_tcp`** — pulls SD-ring capture files off the device over TCP.
- **`reset_tcp`** — remote soft/factory reset. **`log_tcp`** — log streaming.
- **`beacon` / `l2cap`** — BLE provisioning substrate (NimBLE), wrapping
  `r2_core::beacon` build/parse.
- **`wifi_sta` / `wifi_prov`** — station + provisioning.

## Ports cleanly vs workshop-specific vs needs-no_std-rewrite

**Ports cleanly (take as-is / as template):**
- The no_std protocol crates (r2-wire/cbor/fnv/core/trust) — already MCU-proven.
- The `r2-esp` **module decomposition + API shape** (beacon, l2cap, wifi,
  ota_tcp, data_tcp, reset_tcp, log_tcp, hive_id) — this is the per-platform
  layer interface to generalize.
- The OTA wire + receiver state machine; the SD-ring-with-ack-replay durability
  model; the signed-announce identity (NVS Ed25519 + cert); the two-slot OTA
  partition strategy; the whole `build-firmware.sh`/`setup-firmware.sh`/sidecar
  pipeline (incl. both gotchas above).

**Workshop-specific (don't generalize):**
- Sensor drivers (adxl355/lis2dh), the rocker-rig sampling cadence/calibration,
  the `nz.ac.auckland.rocker` class, battery divider per carrier, capture/CSV.

**Needs rewrite for Path B no_std (the real work):**
- Everything inside `r2-esp` is `esp-idf-svc`/std. Pure no_std means swapping
  ESP-IDF for `esp-hal` + `esp-wifi` + `embassy` + `smoltcp`, and NimBLE for a
  no_std BLE stack. The protocol crates and the *module API* survive; the
  platform internals are reimplemented. `fn main()` → `#![no_std] #![no_main]`
  + embassy executor. `std::net::TcpStream` → smoltcp sockets.

## Pointers
- `crates/r2-esp/src/` — the platform-layer modules (the API to generalize).
- `firmware/esp32-s3/devkitc/src/{main,sender,identity,ring}.rs` — app wiring.
- `tools/build-firmware.sh`, `tools/setup-firmware.sh` — build pipeline.
- `firmware/*/*/partitions.csv`, `.cargo/config.toml` (ESP_IDF_VERSION) — board layer.
- Boards: workshop has devkitc/xiao (S3) + dfr1117 (**C6**); hive targets
  DFR1195 (**S3**) — the devkitc/xiao S3 path is the closest precedent.
