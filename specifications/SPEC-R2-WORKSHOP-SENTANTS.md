# SPEC-R2-WORKSHOP-SENTANTS: Ensemble sentant + plugin catalog

**Version:** 0.2 Draft
**Date:** 2026-05-28
**Status:** Normative Draft
**Depends on:** SPEC-R2-WORKSHOP-ENSEMBLE, SPEC-R2-WORKSHOP-SENSOR, SPEC-R2-WORKSHOP-DASHBOARD, SPEC-R2-WORKSHOP-WIRE, SPEC-R2-WORKSHOP-CAPTURE, SPEC-R2-WORKSHOP-TIMESYNC, SPEC-R2-WORKSHOP-ACCESS, SPEC-R2-WORKSHOP-SENSOR-HEALTH, SPEC-R2-WORKSHOP-SENSOR-REMOTE-RESET, SPEC-R2-WORKSHOP-SENSOR-LIVE-LOGS, canonical R2-ENSEMBLE / R2-SENTANT / R2-CAP / R2-DEF

---

## 1. Introduction

The r2-workshop ensemble's sentants live across **two hive
classes**:

* **Sensor hive** — one per ESP32-S3 device. Hand-coded Rust
  monolith at `firmware/esp32-s3/<carrier>/src/main.rs`. The
  Sensor sentant + supporting plugins documented in §3.
* **Dashboard hive** — one per controller laptop. Hand-coded Rust
  binary at `dashboard/src/main.rs`. The Fleet / Capture / Sync /
  TimeSync / Access / Bootstrap sentants + supporting plugins
  documented in §4.

This spec re-frames both monoliths in the canonical R2 vocabulary
— **sentants** (event-driven logic) and **plugins** (hardware /
platform shims) — so the firmware and dashboard match the same
architectural model as r2-notekeeper and so individual building
blocks can be reused or swapped (e.g. ADXL355 → BNO055; rocker
analysis → people-counter detection) without restructuring the
rest of the ensemble.

The catalogue below is **what the runtime currently is**, named
declaratively. The normative score is at
[`ensemble/ensemble.yaml`](../ensemble/ensemble.yaml) per
SPEC-R2-WORKSHOP-ENSEMBLE; this document is the human-readable
companion.

### 1.1 Scope

In scope:

* The fixed sentant ensemble that ships in every r2-workshop sensor
  firmware build, with the events each sentant produces and
  consumes.
* The plugin set each sentant depends on (hardware shims +
  cross-cutting platform services).
* The minimal `Sentant` trait — surface only, not the full R2-HIVE
  runtime.

Out of scope:

* Dynamic sentant loading. The ensemble is fixed at build time,
  AOT-compiled into the firmware image. Operators reconfigure by
  re-flashing.
* Multi-hive cohabitation on one MCU. Each sensor board hosts
  exactly one hive (`rocker-<mac>`).
* The dashboard- and webapp-side hives. Those are R2-HIVE-conformant
  in their own right and are not catalogued here.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**,
**SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**,
and **OPTIONAL** in this document are to be interpreted as
described in [RFC 2119](https://www.rfc-editor.org/info/rfc2119),
when they appear in capitals.

### 1.2 Terminology

* **Sentant** — a self-contained piece of event-driven logic. Has
  a name, declares the event hashes it consumes and produces, and
  exposes a small lifecycle: `boot`, `tick` (called from the main
  loop), `on_event`. Sentants do not talk to hardware directly —
  they hold references to plugins.
* **Plugin** — a capability shim that abstracts a hardware
  peripheral or platform service. Has no event surface of its own;
  exposes typed methods to sentants. Examples: `adxl355`, `nvs`,
  `led`, `wifi_sta`. (R2-CAP §3 terminology — closely related to
  a Capability, but rendered in the firmware as a Rust struct
  rather than an opaque token.)
* **Ensemble** — the ordered list of sentants + plugins composed by
  `main()`. The ensemble for r2-workshop is fixed and defined in §3.
* **Event hash** — FNV-1a-32 over the lowercase event name (per
  R2-WIRE / R2-FNV). Sentants subscribe by hash, not by string.

---

## 2. `Sentant` trait (minimal surface)

Every sentant in a conforming firmware **SHALL** implement the
following surface. The runtime **MAY** be implemented as whatever
`main()` builds — this spec does not mandate a separate scheduler
crate.

```rust
pub trait Sentant {
    /// Canonical name, e.g. `"r2.sensor.accelerometer"`. MUST be
    /// stable across builds. Used as the log target and as the
    /// future R2-HIVE introspection key.
    fn name(&self) -> &'static str;

    /// Event hashes this sentant wants to receive. Hashes MUST
    /// be FNV-1a-32 of the lowercase event name per R2-WIRE /
    /// R2-FNV, computed at compile time.
    fn subscribed_events(&self) -> &'static [u32] { &[] }

    /// One-shot boot. The runtime SHALL call this exactly once
    /// after every plugin in the ensemble has been constructed
    /// and SHALL deliver no `on_event` callbacks before `boot`
    /// has returned.
    fn boot(&mut self, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }

    /// Cooperative tick. The runtime SHALL call this at least as
    /// frequently as the sentant's declared cadence (see §3.2);
    /// implementations SHOULD return promptly. A sentant that
    /// needs its own thread (e.g. a long-running TCP listener)
    /// MAY spawn one in `boot` and leave `tick` as a no-op.
    fn tick(&mut self, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }

    /// Inbound event. The runtime SHALL deliver only events whose
    /// hash is in `subscribed_events()`. A sentant MUST NOT
    /// assume any ordering across event types.
    fn on_event(&mut self, _ev: &Event, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }
}
```

`HiveCtx` **SHALL** expose references to every plugin in §3.1 and a
small event bus the runtime feeds back into `on_event`. A sentant
publishes by calling `ctx.emit(hash, payload)`; the runtime
**SHALL** then deliver the event to other subscribed sentants
in-process and **MAY** additionally forward it onto the wire via
the `uplink` sentant (see §3.2). Sentants **MUST NOT** access
ESP-IDF peripherals directly; they **MUST** go through a plugin.

---

## 3. The r2-workshop sensor ensemble

### 3.1 Plugins (hardware + platform shims)

| Plugin | Owns / wraps | Used by |
|---|---|---|
| `nvs` | ESP-IDF NVS partition. Reads/writes WiFi creds, RBID, clock offset, last-acked seq. | `clock`, `identity`, `wifi-prov`, `recorder` |
| `led` | WS2812 / RGB driver + state machine (`LedState`). | `health`, `uplink`, `wifi-sta`, `beacon`, `ota` |
| `adxl355` | ADXL355 over SPI2, shared bus. | `accelerometer` |
| `sd-card` | Mounted FATFS on `/sdcard`. | `recorder` |
| `battery-adc` | Single-channel ADC + divider for the LiPo cell. | `battery` |
| `wifi-sta` | esp-idf-svc WiFi station + reconnect machinery. | `uplink`, `clock`, all listeners |
| `ble-beacon` | R2-BEACON legacy 28-byte AD advertiser. | `beacon` (sentant of the same name) |
| `ble-l2cap` | L2CAP CoC server on PSM 0x00D2. | `bootstrap` |
| `ota-tcp` | TCP listener on port 21043; receives a firmware image, stages to the inactive OTA partition, restarts. | `ota` |
| `reset-tcp` | TCP listener on port 21044; accepts a single `CMD_RESET` byte. | `reset` |
| `log-tcp` | TCP fan-out on port 21046 of the wrapping logger's records (SPEC-R2-WORKSHOP-SENSOR-LIVE-LOGS). | every sentant, transparently via `log::info!` etc. |
| `data-tcp` | TCP listener on port 21047; LIST / GET / DEL / DEL_ALL over the captures sub-directory (SPEC-R2-WORKSHOP-CAPTURE §6). | external — dashboard's `/api/data/...` handlers. |
| `clock` | Monotonic + offset clock. Reads/writes the NVS-persisted `clock_offset_ms`. | `accelerometer`, `uplink`, `recorder`, `health`, `sync` |

### 3.2 Sentants (event-driven logic)

Hashes shown in hex are FNV-1a-32 over the lowercase event name
per R2-WIRE / R2-FNV.

| Sentant | Subscribes to | Emits | Role |
|---|---|---|---|
| `r2.sensor.identity` | (none) | (none — populated into `HiveCtx` at boot) | One-shot. Loads device keypair from NVS (creates one if absent), loads the persistent RBID, exposes both via the context for other sentants. Mirrors R2-HIVE §4 device-identity contract. |
| `r2.sensor.wifi-prov` | (none) | (none — drives the `wifi-sta` plugin) | One-shot at boot. Reads WiFi credentials from NVS / `wifi_config.toml` / env per SPEC-R2-WORKSHOP-SENSOR §2.1.1, and tells `wifi-sta` to associate. On association failure flips `led` to `Advertising` (blue) and yields to `bootstrap`. |
| `r2.sensor.bootstrap` | (none — listens on the `ble-l2cap` plugin) | (none) | Owns the `#wifi_offer` listener over BLE L2CAP CoC. On a valid signed offer, writes credentials to NVS via the `nvs` plugin and triggers `esp_restart()`. Per R2-BOOTSTRAP §4 + SPEC-R2-WORKSHOP-SENSOR §2.2. |
| `r2.sensor.beacon` | (none) | (none) | Drives the `ble-beacon` plugin with the rocker class hash + RBID + provisioning flag from `identity`. Always running once `identity` has booted. |
| `r2.sensor.accelerometer` | (none) | `r2.sensor.acceleration` 0x94fef38f at 100 Hz | Reads x/y/z via `adxl355` plugin, stamps with `clock.ts_ms_i64()`, emits onto the bus. Falls back to a built-in simulator if the IC fails to enumerate (per SPEC-R2-WORKSHOP-SENSOR-HEALTH). |
| `r2.sensor.battery` | (none) | `r2.sensor.battery` 0xa2751318 every 30 s | Polls `battery-adc`, emits voltage / percent / charging flag. |
| `r2.sensor.status` | (none) | `r2.sensor.status` 0x70bd64a5 every 2 s | Emits FSM state + `data_source` + `seq` watermark + uptime. Drives the dashboard's virtual LED. |
| `r2.sensor.sync` | `r2.dash.sync_pulse` 0x80a7… `r2.dash.set_clock_offset` 0xae40… | `r2.sensor.sync_pong` 0xccae4ebb | Implements SPEC-R2-WORKSHOP-TIMESYNC §2 (Cristian's algorithm). Applies `set_clock_offset` deltas to the `clock` plugin and persists via `nvs`. |
| `r2.sensor.recorder` | `r2.sensor.acceleration`, `r2.dash.ack` 0xab… | (none) | Writes every acceleration record to the SD ring (CSV per SPEC-R2-WORKSHOP-SENSOR §6.2 v0.2) with periodic fsync; frees segments whose `last_seq ≤ through_seq` on each ack. |
| `r2.sensor.uplink` | every event the dashboard cares about | (none — TCP egress) | Single TCP session to the gateway (port 21042). Sends the announce frame on connect, then forwards subscribed events as R2-WIRE compact frames. On session error, reconnects with exponential backoff; flips `led` between `WifiConnecting` and `StreamingLive`/`StreamingDegradedSim` per session state. |
| `r2.sensor.ota` | (driven by the `ota-tcp` plugin) | (none) | TCP listener that accepts firmware via SPEC-R2-WORKSHOP-SENSOR §12. Verifies SHA-256, swaps OTA partitions, reboots. Calls `esp_ota_mark_app_valid_cancel_rollback()` after the first frame round-trips via `uplink`. |
| `r2.sensor.reset` | (driven by the `reset-tcp` plugin) | (none) | TCP listener implementing SPEC-R2-WORKSHOP-SENSOR-REMOTE-RESET. Calls `esp_restart()`. |
| `r2.sensor.health` | `r2.sensor.acceleration` | (none) | Watches for a stuck data source (SPEC-R2-WORKSHOP-SENSOR-HEALTH §6) and surfaces `data_source = sim` on the next `r2.sensor.status` emission. |
| `r2.sensor.capture` | `r2.sensor.acceleration`, `r2.dash.capture.start`, `r2.dash.capture.mark`, `r2.dash.capture.stop` | `r2.sensor.capture.state` | Owns `CaptureMgr`. Implements the Idle / Calibrating / Recording state machine per SPEC-R2-WORKSHOP-CAPTURE §2. Writes calibrated CSV rows to `/sdcard/captures/<ts16>-<name>.csv` via the `sd-card` plugin while in Recording. Emits a state event on every transition. |
| `r2.sensor.presence` | (none) | UDP burst | One-shot at boot: 5× UDP packets to `255.255.255.255:21044` carrying the persistent RBID + own IP. Drives the dashboard's RBID-based bootstrap reconciliation. |

### 3.3 Required boot order

The firmware **SHALL** boot the ensemble in the following order.
Steps marked OPTIONAL are conditional on the firmware build.

1. `identity` (**REQUIRED**) — populates the device keypair + RBID
   into the context.
2. Plugins (**REQUIRED**) — `nvs`, `led`, `adxl355`, `sd-card`,
   `battery-adc`, `clock`, `wifi-sta`, `ble-beacon`, `ble-l2cap`,
   `ota-tcp`, `reset-tcp`, `log-tcp` constructed and registered on
   `HiveCtx`. A plugin's construction failure **MUST NOT** be
   fatal if the plugin's spec allows graceful degradation
   (e.g. `sd-card.try_mount` returning `None` per
   SPEC-R2-WORKSHOP-SENSOR §6).
3. `wifi-prov` (**REQUIRED**) — either `wifi-sta` succeeds or
   control yields to `bootstrap` per
   SPEC-R2-WORKSHOP-SENSOR §2.1.1.
4. `beacon` (**REQUIRED**) — starts unconditionally once
   `identity` is populated.
5. `presence` (**REQUIRED**) — one UDP burst once `wifi-sta` is
   associated.
6. `clock` (**REQUIRED**) — offset loaded from NVS.
7. All remaining sentants in §3.2 (**REQUIRED**) — `boot()`-ed in
   any order; the runtime **MUST NOT** deliver events between
   them until every `boot()` returns.
8. Main loop (**REQUIRED**) — the runtime **SHALL** call each
   sentant's `tick()` at least as frequently as its declared
   cadence in §3.2.

### 3.4 Implementation note (non-normative)

For v0.2, sentants **SHOULD** be realised as Rust structs in
`firmware/esp32-s3/<carrier>/src/sentants/*.rs` and plugins as
Rust structs in `firmware/esp32-s3/<carrier>/src/plugins/*.rs` (or
the shared `crates/r2-esp/`). The ensemble composer **SHOULD**
live in `main()`. There is no dynamic registry — adding a sentant
is a source-tree edit.

A future v0.3 **MAY** move to a build-time descriptor (YAML / TOML
listing sentants and plugins) compiled to the same Rust ensemble.
That is consistent with the "devise the sentants and plugins, then
compile them to working code" workflow noted in the README.

---

## 4. The r2-workshop dashboard ensemble

The dashboard hive runs on the controller laptop (Linux, std).
Hand-coded today at `dashboard/src/main.rs`; the score documents
the canonical decomposition.

### 4.1 Plugins (platform shims)

| Plugin | Owns / wraps | Used by |
|---|---|---|
| `relay-tunnel` | WebSocket session to the r2-hive relay (SPEC-R2-WORKSHOP-ACCESS §5.2). Forwards R2-WIRE frames bidirectionally between LAN viewers and off-network viewers. | `access` |
| `sd-relay` | data_tcp client. Dials sensor port 21047 to LIST / GET / DEL files on the SD ring (SPEC-R2-WORKSHOP-CAPTURE §6). | `sync`, HTTP route handlers under `/api/data/{addr}/...` |
| `github-firmware-cache` | Periodic poll of `reality2-ai/r2-workshop` Releases + local `firmware/esp32-s3/<carrier>/releases/` fallback (DASHBOARD §13.3). | HTTP route `/api/firmware/available` |
| `tg-signer` | Loads the KeyHolder Ed25519 keypair from `~/.config/r2-workshop/tg_signer/tg_priv.bin` (per `SECRETS-POLICY.md`); signs DeviceCertificates + `#wifi_offer` payloads. | `access`, `bootstrap` |
| `captures-store` | XDG-rooted persistent index over `~/.local/share/r2-workshop/captures/`; provides `has`, `write_data`, `write_marks`, `list_sessions`, `clear_all` (CAPTURE §7.4). | `sync`, HTTP route handlers under `/api/data/local/...` and `/api/data/zip` |
| `ble-scan` | bluez / btleplug scanner subscription for the workshop class hash on the R2-BEACON legacy AD payload. | `bootstrap` |

### 4.2 Sentants (event-driven logic)

| Sentant | Subscribes to | Emits | Role |
|---|---|---|---|
| `r2.dash.fleet` | `r2.sensor.announce`, `r2.peer.disconnected`, `r2.dash.cmd.device.alias.set` | `r2.dash.device.alias.changed` | Tracks every peer's `device_pk` + operator alias + last-known online/stale/offline state. Caches the most-recent announce frame so `/r2` viewer-connects can replay the metadata to late-joining viewers without waiting for the next sensor reboot. |
| `r2.dash.capture` | `r2.dash.cmd.capture.start`, `r2.dash.cmd.capture.mark`, `r2.dash.cmd.capture.stop`, `r2.dash.cmd.capture.event_mark` | `r2.dash.capture.start`, `r2.dash.capture.mark`, `r2.dash.capture.stop`, `r2.dash.capture.event_mark` (to sensors); `r2.dash.capture.progress`, `r2.dash.capture.event_marked`, `r2.dash.cmd.response` (to viewers) | Owns the fleet-wide Calibrate → Record → Stop state machine + the monotonic `mark_id` counter. Stamps authoritative `ts_ms` on Record and on every event_mark. Per SPEC-R2-WORKSHOP-CAPTURE §2 + §7.5. |
| `r2.dash.sync` | `r2.sensor.capture.state` (observes Recording → Idle transitions) | `r2.dash.capture.sync_started`, `r2.dash.capture.synced` | Per-peer transition watcher + 60-second reconciliation poll + immediate single-peer recon on each announce (closes the 0-60 s blind window after a mid-experiment sensor reset). Drives the `captures-store` plugin; replays the full index to every `/r2` viewer on connect. Per SPEC-R2-WORKSHOP-CAPTURE §7.4. |
| `r2.dash.timesync` | `r2.sensor.sync_pong` | `r2.dash.sync_pulse`, `r2.dash.set_clock_offset` | Cristian's-algorithm round per peer. 1 Hz cadence for the first 30 s after each TCP connect, then 30 s thereafter. Exponentially smooths the offset estimate; pushes `set_clock_offset` when the estimate stabilises or drifts past threshold. Per SPEC-R2-WORKSHOP-TIMESYNC §3. |
| `r2.dash.access` | `r2.dash.cmd.access.members.query`, `…pending.query`, `…check`, `…approve`, `…deny`, `…revoke`, `…request` | `r2.dash.access.event`, `r2.dash.enrol` (to sensors), `r2.dash.cmd.response` | KeyHolder-side viewer + device enrolment. Mints DeviceCertificates via the `tg-signer` plugin; manages the pending + approved member lists; emits status events viewers consume to update their Link tab. Per SPEC-R2-WORKSHOP-ACCESS. |
| `r2.dash.bootstrap` | `r2.dash.cmd.bootstrap` | `r2.dash.bootstrap.progress`, `r2.dash.enrol` (over L2CAP) | BLE-scan + L2CAP CoC handshake. Discovers sensors advertising the workshop class hash, signs `#wifi_offer` with the TG private key via `tg-signer`, delivers WiFi credentials so the sensor can join the hotspot. Per SPEC-R2-WORKSHOP-SENSOR §3.5 + DASHBOARD §6. |
| `r2.dash.ota` | (driven by `POST /api/ota/{addr}` HTTP route + `r2.dash.cmd.access.approve` for bulk cases) | `r2.dash.fw.update` (to sensors), `r2.dash.ota.progress` (to viewers) | Streams firmware blobs to peers, surfaces progress. Manual OTA validation per DASHBOARD §13.4 lives client-side; this sentant is the transport. |
| `r2.dash.reset` | `r2.dash.cmd.reset` | `r2.dash.reset.progress` | Opens a TCP session to the sensor's reset port (21044) and writes the `CMD_RESET` byte. Surfaces success/failure as a progress event. Per SPEC-R2-WORKSHOP-SENSOR-REMOTE-RESET. |
| `r2.dash.identify` | `r2.dash.cmd.identify` | `r2.dash.identify_set` (to sensors) | Toggles the sensor's identify-LED via a single-frame fire-and-forget command. Per SPEC-R2-WORKSHOP-SENSOR-IDENTIFY. |

### 4.3 R2-WEB registration

The dashboard's webapp surface is **not a sentant**. Per
R2-ENSEMBLE §2.1.1 it's a registration with the hive-shared
R2-WEB singleton plugin — a static bundle + a `/r2` WebSocket
channel + a set of `/api/...` HTTP routes. See
`ensemble/ensemble.yaml` `registrations.r2-web` for the
authoritative shape; SPEC-R2-WORKSHOP-DASHBOARD §5.1 for the
per-route detail.

### 4.4 Implementation note (non-normative)

The dashboard binary today is a single Rust monolith; the sentants
above are realised as struct + free-function clusters within
`dashboard/src/main.rs` (plus the `dashboard/src/captures.rs`,
`dashboard/src/access.rs`, `dashboard/src/relay.rs` modules). The
sentant decomposition is the canonical mental model + the target
shape for the future R2 ensemble loader; until that loader lands
(SPEC-R2-WORKSHOP-ENSEMBLE §4 phase B3), the binary is the
operative form.

---

## 5. Conformance

A **sensor firmware build** conforms to this spec when ALL of the
following hold:

1. Every sentant listed in §3.2 **MUST** be present in the
   firmware image.
2. Each sentant's emitted-event hashes **MUST** equal the FNV-1a-32
   of the lowercase event name listed in §3.2.
3. Every plugin in §3.1 **MUST** be reachable from sentants via
   the `HiveCtx` (or equivalent ownership pattern). No sentant
   **SHALL** access an ESP-IDF peripheral directly.
4. The boot order in §3.3 **MUST** be respected.
5. A sentant or plugin **SHOULD** be portable to a sibling
   ensemble (e.g. people-counter) by porting the file plus its
   declared plugin dependencies — i.e. no deployment-specific
   globals.

A **dashboard build** conforms to this spec when ALL of the
following hold:

1. Every sentant listed in §4.2 **MUST** be reachable as part of
   the dashboard process. The decomposition need not be a literal
   1-Rust-struct-per-sentant (pre-loader era) but the event
   surface MUST match.
2. Every plugin in §4.1 **MUST** be the sole code path to the
   relevant capability (e.g. only `tg-signer` holds the
   KeyHolder private key; only `captures-store` mutates the
   captures dir).
3. The R2-WEB registration's static bundle (§4.3) **MUST** be
   served at `/` and **MUST** expose the `/r2` WebSocket per
   SPEC-R2-WORKSHOP-DASHBOARD §5.2.

---

## 6. Versioning

| Date       | Ver | Change                                                 |
|------------|-----|--------------------------------------------------------|
| 2026-05-18 | 0.1 | Initial draft — catalog of the existing firmware modules, framed as sentants + plugins. No code change yet. |
| 2026-05-28 | 0.2 | Add §4 — dashboard-hive sentants (Fleet, Capture, Sync, TimeSync, Access, Bootstrap, OTA, Reset, Identify) + their plugins (relay-tunnel, sd-relay, github-firmware-cache, tg-signer, captures-store, ble-scan). Title + intro reframed to cover the whole ensemble across both hive classes; cross-refs the new SPEC-R2-WORKSHOP-ENSEMBLE.md + `ensemble/ensemble.yaml`. |
