# SPEC-R2-WORKSHOP-DASHBOARD: Controlling-Device Dashboard Behaviour

**Version:** 0.1 Draft
**Date:** 2026-05-07
**Status:** Normative Draft
**Depends on:** SPEC-R2-WORKSHOP-WIRE, SPEC-R2-WORKSHOP-SENSOR, R2-BLE, R2-BOOTSTRAP, R2-WIFI, R2-TRUST

---

## 1. Introduction

This specification defines the **operational behaviour** of the
**r2-workshop dashboard hive** — the single controller process per
deployment.

In the canonical R2 vocabulary (R2-ENSEMBLE §2.1), the dashboard
is **not a standalone product**. It is the hive that hosts the
ensemble's dashboard-side sentants (Fleet / Capture / Sync /
TimeSync / Access / Bootstrap / OTA / Reset / Identify — see
SPEC-R2-WORKSHOP-SENTANTS §4) PLUS the **R2-WEB plugin
registration** that serves the operator's browser UI (static
bundle + `/r2` WebSocket + `/api/...` HTTP routes — see §5.1).

The current Rust binary (`dashboard/src/main.rs`) is the
pre-loader-era approximation of all of that: the sentants are
realised as struct + free-function clusters, the R2-WEB
registration is built into the binary, and the Controller
role-ensemble score at `ensemble/controller.yaml` is documentation
rather than the operative input. Phase B3 of the
SPEC-R2-WORKSHOP-ENSEMBLE roadmap will move that score from
documentation to operative.

What the dashboard hive currently does, named:

* Hosts a WiFi hotspot for sensors to join.
* Bootstraps sensors via BLE + L2CAP, signing `#wifi_offer` frames with
  the trust-group private key.
* Receives R2-WIRE TCP streams from sensors on port 21042.
* Decodes accelerometer + battery + status frames.
* Maintains per-device calibration matrices and joint-group definitions.
* Computes pairwise differential lateral motion across joints (the
  diagnostic per PLAN D-08).
* Serves the browser UI on the unified R2 port 21042 (HTTP + WebSocket).
* Manages OTA updates and reset commands.
* Auto-syncs sensor captures to a controller-local store
  (SPEC-R2-WORKSHOP-CAPTURE §7.4) and broadcasts in-session event
  marks (§7.5).

There is **exactly one** dashboard per deployment. The dashboard is the
only entity holding the TG private key (see `SECRETS-POLICY.md`).

### 1.1 Scope

In scope:

* Process model, startup, and shutdown.
* On-disk state (peers, calibration, joints).
* Network listeners (TCP, HTTP, BLE).
* Bootstrap engine.
* Per-peer state machine on the dashboard side.
* Time-synchronisation algorithm.
* Server-side calibration math.
* Joint groups and differential analysis.
* Stress indicator and long-term trend.
* Browser UI requirements.
* OTA push and command dispatch.

Out of scope:

* Sensor firmware (`SPEC-R2-WORKSHOP-SENSOR`).
* Wire protocol (`SPEC-R2-WORKSHOP-WIRE`).
* Hotspot driver-level configuration (delegated to the host OS via
  NetworkManager / nmcli).

### 1.2 Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**,
**MAY** are interpreted per RFC 2119.

* **Dashboard** — the single application defined by this spec.
* **Host** — the Linux machine (laptop or Raspberry Pi) running the
  dashboard.
* **Peer** — a connected (or remembered) sensor, identified by its
  `device_pk`.
* **Joint** — a logical grouping of one or more peers that, together,
  diagnose a structural element of the rig.
* **TG** — trust group (see PLAN D-19).
* **Wall clock** — the host's monotonic-clock reading converted to
  milliseconds since dashboard start, used as the reference time for
  cross-sensor alignment.

### 1.3 Notation

UTC-style ISO 8601 timestamps in user-facing UI; integer milliseconds
internally. Path conventions assume Linux; Windows/macOS support is
out of scope for v0.1.

---

## 2. Process model

### 2.1 Single-process architecture

The dashboard is **one Rust process** with multiple Tokio tasks:

| Task | Purpose |
|---|---|
| `tcp_listener` | Accept inbound sensor connections on :21042 |
| `peer_handler` (per peer) | Decode R2-WIRE frames, dispatch to state |
| `http_server` | Serve the browser UI on the unified R2 port (:21042) via hyper-driven axum |
| `websocket_handler` (per browser) | Push live updates |
| `bootstrap_engine` | BLE scan + L2CAP per sensor, on demand |
| `analytics` | Periodic stress indicator computation, trend update |
| `persistence_writer` | Rate-limited disk writes for state |

### 2.2 Startup sequence

1. Parse CLI args / config (§14).
2. Load TG private key from `tg_signer_path`
   (default `~/.config/r2-workshop/tg_signer/tg_priv.bin`); refuse to
   start if missing or unreadable.
3. Load TG public key from `trust_keys/tg_pub.bin`; verify it pairs
   with the private key.
4. Load on-disk state (§3) into memory: peers, calibration, joints,
   high_water.
5. Bind the unified R2 port listener on :21042. If
   fails, exit with non-zero status — *do not* fall back to alternate
   ports.
6. Initialise the BLE adapter (do not yet scan).
7. Start the analytics task.
8. Log "ready" and serve.

### 2.3 Shutdown

On `SIGINT` / `SIGTERM` the dashboard shall:

1. Stop accepting new sensor connections.
2. Send a final `r2.dash.ack` to every connected sensor with the
   highest known `seq`.
3. Flush all pending state writes to disk.
4. Disconnect the WiFi hotspot iff the dashboard created it (§6.4).
5. Exit cleanly within 5 s; force-kill after 10 s.

---

## 3. State persistence

### 3.1 Layout

The dashboard maintains state in a host-local directory, default
`~/.config/r2-workshop/state/` (overridable via CLI; see §14):

```
state/
├─ peers.json            ← known sensors (device_pk → metadata)
├─ calibration.json      ← per-device R matrix
├─ joints.json           ← joint group definitions
├─ high_water.json       ← per-device last-received seq
└─ events.log            ← rolling event log (append-only, rotated daily)
```

These paths are **gitignored** (`SECRETS-POLICY.md`); they are local
runtime state.

### 3.2 `peers.json`

```json
{
  "<device_pk_hex>": {
    "device_pk": "<32-byte hex>",
    "hostname": "rocker-1",
    "first_seen": "2026-05-07T08:00:00Z",
    "last_seen": "2026-05-07T09:30:14Z",
    "mounting_role": "rocker",
    "joint": "front-left-actuator",
    "fw_ver": "0.1.0+abc1234",
    "label": "Front-left rocker pivot",
    "notes": "..."
  }
}
```

A device is added on first valid `r2.sensor.announce`. The dashboard
follows TOFU (trust-on-first-use) for v0.1: any peer whose announce
signature is well-formed (Ed25519 over the canonical CBOR per WIRE §3.1)
is admitted, with `device_pk` becoming the persistent identity.

A peer can be deleted from `peers.json` only via the UI, with confirm.

### 3.3 `calibration.json`

```json
{
  "<device_pk_hex>": {
    "g_a":   [gx, gy, gz],
    "g_b":   [gx, gy, gz],
    "R":     [[r11, r12, r13],
              [r21, r22, r23],
              [r31, r32, r33]],
    "main_axis":     [mx, my, mz],
    "vertical_axis": [vx, vy, vz],
    "sideways_axis": [sx, sy, sz],
    "calibrated_at": "2026-05-07T09:00:00Z",
    "ms": 1000,
    "n_samples": 99,
    "valid": true
  }
}
```

Computed from `g_a` and `g_b` per PLAN D-17. See §9 for math.

### 3.4 `joints.json`

```json
{
  "front-left-actuator": {
    "label": "Front left actuator joint",
    "members": ["<device_pk_hex_1>", "<device_pk_hex_2>"],
    "topology": "pair",
    "baseline_separation_mm": 350,
    "stress_threshold_g_rms": 0.25,
    "created_at": "2026-05-07T..."
  },
  "front-right-actuator": {
    "label": "Front right actuator joint",
    "members": ["<device_pk_hex_3>"],
    "topology": "single",
    "stress_threshold_g_rms": 0.25
  }
}
```

`topology` values: `"single"` (one sensor; absolute lateral motion), or
`"pair"` (two sensors; differential lateral motion). Future:
`"reference"` for bed-mounted reference channel.

### 3.5 `high_water.json`

```json
{
  "<device_pk_hex>": { "high_seq": 1234567, "as_of": "2026-05-07T..." }
}
```

Updated whenever the dashboard records a sample with `seq > high_seq`.
Used to resume after dashboard restart: on receipt of
`r2.sensor.announce {last_seq: N}`, the dashboard sends
`r2.dash.ack {through_seq: max(high_seq, N - 1)}` so the sensor knows
where to resume.

### 3.6 Persistence rate-limiting

State writes are **debounced** at 1 Hz per file. The
`persistence_writer` task collects dirty markers from other tasks and
flushes affected files at most once per second. On clean shutdown, all
dirty state is flushed unconditionally.

---

## 4. R2 port listener (port 21042)

The dashboard binds a **single TCP listener on the canonical R2-WIRE
port 21042** (per R2-WIRE §13.5 — port 21042 carries R2-WIRE events in
both raw-TCP and WebSocket form; R2-TRANSPORT §3.5 — WebSocket is just
an alternative encoding of the same Internet transport). The accept
loop peek-dispatches each connection:

* First byte in `[A-Z]` (0x41-0x5A) → HTTP request line. The
  connection is driven by hyper with the axum router as its service;
  WebSocket upgrade requests (`/r2`, `/ws/logs/{addr}`) are
  handled inline via `serve_connection.with_upgrades()`. The bulk of
  this section's §4.x subsections describe that path (HTTP routes,
  WebSocket message shape, etc.).
* Otherwise → raw R2-WIRE TCP from a sensor. Sensor frames are
  length-prefixed per WIRE §13.4 — the first byte is the high byte
  of a u16 BE length, always `0x00` for our compact frames (< 256
  bytes total). The connection is handed to `handle_sensor_connection`
  with TCP keepalive enabled (15 s probe / 5 s interval); see §4.2.

This unification replaces the pre-v0.2 split (TCP/21042 for sensors,
HTTP/8080 for browsers). The two never collide because HTTP requests
always start with an ASCII method name and R2-WIRE frames always
start with the length-prefix high byte.

### 4.1 Accept loop

`run_unified_listener` accepts on `0.0.0.0:21042`, peeks the first
byte (5 s timeout — generous; both sensors and browsers send their
first byte well under that), and forks per the rule above. Each
connection is spawned into its own task so a slow peek doesn't
block accept.

### 4.2 Frame decoding

Each TCP byte stream is parsed as a sequence of length-prefixed
R2-WIRE frames (per WIRE §1.4). The dashboard shall:

* Read a u16 big-endian length.
* Read `length` bytes as one R2-WIRE compact frame.
* Decode the event hash, dispatch by name.

Malformed length prefixes or oversize frames (> 4 KiB) shall close the
connection.

### 4.3 Announce verification

The first frame received MUST be `r2.sensor.announce`. The dashboard
shall:

1. Decode the CBOR payload.
2. Reconstruct the canonical CBOR over keys 0..5 (WIRE §3.1).
3. Verify `sig` (key 6) as Ed25519 over the canonical bytes, using
   `device_pk` (key 0) as the verifying key.
4. (TOFU policy) Accept any well-formed signature in v0.1; record
   `device_pk` to `peers.json` if new.

Future versions SHALL replace TOFU with TG-cert-chain verification per
R2-TRUST: the device presents a cert chain rooted at `TG_PUB_KEY`, and
the announce signature is over a fresh nonce signed by the cert's
device key.

### 4.4 Per-peer dispatch

After a successful announce:

* The dashboard sends `r2.dash.ack {through_seq: high_water[device_pk]
  or last_seq}` to confirm resume point.
* The dashboard MAY immediately send a `r2.dash.config.set` if the peer
  has been renamed/reroled in the UI since last contact.
* Subsequent frames are dispatched by event hash:

| Event hash | Handler |
|---|---|
| `r2.sensor.acceleration` | sample buffer + WS push (§5.2) |
| `r2.sensor.acceleration.batch` | sample buffer + WS push, ack-eligible |
| `r2.sensor.battery` | battery widget update |
| `r2.sensor.status` | peer status display |
| `r2.sensor.cal.sample.resp` | calibration handler (§9) |
| `r2.sensor.sync_pong` | time-sync handler (§8) |
| `r2.sensor.event.log` | event log panel |
| (unknown) | log + ignore (per WIRE §2) |

### 4.5 ACK cadence

The peer handler shall transmit `r2.dash.ack {through_seq: N}` where
`N` is the highest received `seq`, at the rate of:

* every **200 ms**, OR
* every **100 received samples**,

whichever occurs first (per WIRE §4.1).

---

## 5. HTTP + WebSocket (unified R2 port 21042)

### 5.1 HTTP routes

| Route | Method | Purpose |
|---|---|---|
| `/` | GET | WASM viewer bundle — `webapp/` static tree (HTML + JS + `pkg/` WASM); the canonical browser frontend (the pre-Phase-5d server-decoded `/` HTML was removed once `/v/` reached parity and got promoted to root) |
| `/api/peers` | GET | Current peers (snapshot) |
| `/api/peers/:pk` | PATCH | Update label/role/joint assignment |
| `/api/joints` | GET, POST, PATCH, DELETE | Manage joint definitions |
| `/api/calibration/:pk` | GET, DELETE | Inspect / clear calibration |
| `/api/calibration/:pk/sample` | POST `{position: "A"\|"B", ms: 1000}` | Trigger cal sample request |
| `/api/calibration/:pk/compute` | POST | Compute R from stored g_A/g_B |
| `/api/stream/:pk/start` | POST `{rate_hz, range}` | Forward to sensor |
| `/api/stream/:pk/stop` | POST | Forward to sensor |
| `/api/ota/{addr}` | POST `<firmware .bin bytes>` | Phase 9-light OTA push — multi-MB binary upload. Deliberately not migrated to a cmd event since per-frame WS is the wrong shape; progress streams on `r2.dash.ota.progress` (WIRE row 23). |
| `/api/firmware/available` | GET | Latest release per carrier (GitHub Releases primary, local `releases/` fallback). 5-min cache. |
| `/api/firmware/{carrier}/binary` | GET | Stream the cached firmware blob for OTA selection. |
| `/api/data/{addr}/list` | GET | Open a TCP connection to `<addr>:21047` and issue `LIST`. Returns a JSON-mapped CBOR response of `[{name, size, mtime}]`. |
| `/api/data/{addr}/file/{name}` | GET, DELETE | `GET` opens TCP 21047 and streams the raw fixed-width rows with a synthesised CSV header `seq,ts_ms,<dev>_x,<dev>_y,<dev>_z\n` where `<dev>` is the operator-assigned alias (or IP-with-underscores fallback) — same name resolution as `/api/data/merged`. Content-Disposition filename becomes `<original-stem>__<dev>.csv` so multi-sensor downloads don't collide when collected later. `DELETE` issues `DEL`. |
| `/api/data/{addr}/all` | DELETE | `DEL_ALL` against the named sensor. |
| `/api/data/merged` | GET `?file=<basename>` | Wide-format merge across the fleet: header is `ts_ms` followed by three columns per sensor (`<ip>_x, <ip>_y, <ip>_z`, IP dots → underscores, sensors in sorted-IP order). One row per unique `ts_ms`, ascending; cells are blank where that sensor has no sample at that `ts_ms`. |
| `/api/data/local/list` | GET | Controller-local capture index. Returns the `CapturesStore` contents (SPEC-R2-WORKSHOP-CAPTURE §7.4) as JSON, grouped by session-stem. Each session row carries `{stem, ts_ms, files: [{device_pk, alias, sensor_filename, controller_path, size, mtime_ms, kind: "data"|"marks"}], marks: [{ts_ms, mark_id, label}]}`. Reflects what has actually synced to the laptop — works even when no sensor is currently connected. |
| `/api/data/local/file/{name}` | GET | Stream a single file from the controller-local captures dir. `{name}` is the `<stem>__<dev>.csv` (or `.marks.csv`) filename as it appears in `local/list`. Content-Disposition matches the filename. |
| `/api/data/zip` | GET | End-of-day "grab everything" bundle. v0.2 sources from the controller-local store (was: per-sensor round-trip in v0.1) — much faster and works while sensors are offline. Sensors that haven't synced their latest file yet are simply absent from the zip; operator can trigger a fresh sync round and re-zip. |
| `/api/devices/aliases` | GET | Bulk-fetch the operator-assigned alias map on webapp boot. Per-sensor mutations ride `r2.dash.cmd.device.alias.set` on `/r2`. |
| `/api/version` | GET | `{name, version, git_sha, built}` introspection. |
| `/api/ensemble` | GET | The running deployment's R2-ENSEMBLE identity (SPEC-R2-WORKSHOP-ENSEMBLE §2.1): `{ensemble, class, class_hash, ensemble_version, build, built_at}`. `ensemble` is the class-string leaf (`nz.ac.auckland.rocker` → `rocker`); `class_hash` is the compile-time FNV-1a-32 of the class (`0x624c47bc`); `build` is `<semver>+<git_sha>`. The webapp reads this to render its identity footer. |
| `/api/keyholder/tg-pub` | GET | TG public key bytes for QR-code generation. |
| `/api/access/onboard` | GET | KeyHolder-only operator UI helper — pair of QR payloads for the "Onboard a visitor" modal (ACCESS v0.3 §8). |
| `/api/access/whoami/{device_pk}` | GET | Self-heal — a paired viewer calls this on every load to confirm membership; 404 → stale cert → webapp wipes IndexedDB. |
| `/api/enrol-init`, `/api/enrol-complete` | POST | v0.1 stubs returning 501; reserved for a future enrolment-token flow. |
| `/r2` | WebSocket | **Binary** R2-WIRE frames in both directions: sensor-emitted events forwarded verbatim outbound, viewer-emitted operator commands inbound (WIRE §2.1). Per R2-WIRE §13.5, R2-WIRE events ride port 21042 — `/r2` is the WebSocket path on that port (R2-INTERNET §5). v0.1 anonymous per the additive auth model in SPEC-R2-WORKSHOP-ACCESS §5.1; v1 target is a cert-handshake variant. |
| `/ws/logs/{addr}` | WebSocket | **Text** per-sensor live log tail — proxies a TCP connection on the sensor's log port (21046) and pipes lines back as WS text frames. Non-R2-WIRE rocker plugin transport. |

Routes that existed pre-v0.2 and were retired in v0.2 (every operator-
plane action migrated to a `r2.dash.cmd.*` event on `/r2`): see §5.3.

Auth: v0.1 has no auth — the dashboard is bound to localhost-or-LAN
on a private hotspot. Future versions SHOULD add a session cookie or
token if exposed beyond the lab network.

### 5.2 `/r2` — binary frame transport

The canonical Phase-5d browser data path. Each WS message is a single
binary envelope wrapping one verbatim R2-WIRE frame from one sensor:

```
[u16 BE  src_addr_len ][src_addr  utf-8       ]
[u32 BE  ts_ms_low32  ][u16 BE  frame_len    ][frame  raw bytes]
```

* `src_addr` is the sensor's TCP socket address (`ip:port`) as observed
  by the dashboard — used by the browser to key per-peer state.
* `ts_ms_low32` is the dashboard wall-clock time of receipt, low 32 bits
  of `unix_epoch_ms`. The full upper bits are reconstructed
  client-side from local clock context.
* `frame` is the unmodified R2-WIRE compact frame as defined in
  `SPEC-R2-WORKSHOP-WIRE` §3 — header + signed CBOR payload.

Frames are forwarded **without decode, signature verification, or
filtering** at the server, with one exception: acceleration
(`r2.sensor.acceleration`) is **decimated by `ACCEL_DECIMATION`**
(currently 10) before broadcast — only every 10th sample per peer
reaches `/r2`. The same gate previously applied to the now-removed `/ws/status` JSON path
so viewers see consistent per-peer rates regardless of transport.
The browser's vendored `r2-wasm` performs decode + signature
verification on the frames it does receive. Full fidelity remains on
the SD ring (`/api/data/*` retrieval). The decimation became
server-side after Pi5 deployment saturated WebSocket + browser
processing at the full 100 Hz × N-sensors firmware rate.

Browser → server: viewer hives emit `r2.dash.cmd.*` events on
`/r2` per SPEC-R2-WORKSHOP-WIRE §2.1 (operator-plane request /
response framework). Frames are bare R2-WIRE compact bodies (12-byte
header + CBOR payload — no length prefix; WebSocket provides the
message boundary). Unknown event hashes are logged and dropped per
WIRE §2's "non-actionable" rule.

### 5.3 Removed legacy endpoints

The pre-v0.2 dashboard exposed several transitional channels that have
since been retired:

* `GET /` (server-decoded SPA bundle, embedded HTML) — pre-Phase-5d.
  Replaced by the static WASM viewer served at `/`.
* `GET /ws` (bidirectional JSON WebSocket where the server decoded
  R2-WIRE frames) — pre-Phase-5d. Replaced by `/r2`.
* `GET /ws/status` (text JSON status channel — bootstrap progress,
  peer disconnects, OTA / reset / capture / access / device-alias
  events) — pre-v0.2. Every event it carried is now an R2-WIRE event
  on `/r2`: see SPEC-R2-WORKSHOP-WIRE §2 rows 22-28 + 32. The webapp
  consumes these directly through its rocker viewer hive.
* `POST /api/capture/{start,mark,stop}` — replaced by
  `r2.dash.cmd.capture.{start,mark,stop}` on `/r2` (WIRE rows 29-31).
* `POST /api/sensor/{addr}/reset` — replaced by `r2.dash.cmd.reset`
  (row 33).
* `POST /api/sensor/{addr}/identify` — replaced by
  `r2.dash.cmd.identify` (row 34).
* `POST /api/bootstrap` + `GET /api/bootstrap/status` — replaced by
  `r2.dash.cmd.bootstrap` (row 35); progress streams on
  `r2.dash.bootstrap.progress` (row 27).
* `POST /api/devices/alias` — replaced by
  `r2.dash.cmd.device.alias.set` (row 36).
* The seven `/api/access/*` operator routes — replaced by the
  `r2.dash.cmd.access.*` set (rows 37-43).

New consumers MUST use `/r2` and the static WASM viewer at `/`.

---

## 6. Bootstrap engine

### 6.1 Trigger

Bootstrap runs on demand: the user clicks "Connect Sensors" in the UI,
which emits `r2.dash.cmd.bootstrap` on `/r2` (WIRE row 35). The dashboard
shall NOT auto-discover on start; discovery is operator-controlled. Re-
pressing the button while a bootstrap loop is active SHALL cancel the
in-flight task and start a fresh one (a `Reset` event is emitted as
`r2.dash.bootstrap.progress` on `/r2` to signal this to viewers).

### 6.2 WiFi hotspot

The dashboard SHALL ensure a WiFi hotspot is active before starting
discovery:

1. **Cycle the existing hotspot, if any.** When the bootstrap loop
   is (re)started, the dashboard sets
   `BootstrapConfig.cycle_hotspot = true`, which tells the engine to
   tear down the active `r2-workshop-*` AP-mode connection on the
   spare adapter before bringing one up. Any sensor still latched
   to the old hotspot loses WiFi and falls back to BLE — the only
   path through which `run_bootstrap` can re-push credentials to a
   sensor that was already streaming. Without this, pressing
   "Connect Sensors" while a sensor is already happily connected
   is a no-op for that sensor.
2. Check NetworkManager for a hotspot connection on the spare
   adapter (the dashboard refuses to host the hotspot on the
   internet-carrying adapter — see §14 / `find_free_wifi_interface`).
3. If active and matches the desired SSID, reuse it.
4. If not, create one via `nmcli dev wifi hotspot ifname …` with
   the fixed SSID `R2-workshop` and `HOTSPOT_PSK`.
5. Wait for the hotspot to be ready (carrier up, IP assigned).

The hotspot's gateway IP (typically 10.42.0.1 with NetworkManager) is
the value embedded in `#wifi_offer.gateway_ip`.

### 6.3 BLE scan

The dashboard shall scan for BLE advertisements matching R2-BEACON
(per R2-BEACON spec). The canonical r2-workshop sensor class string is:

```
nz.ac.auckland.rocker   →   FNV-1a-32 hash 0x624C47BC
```

(Reverse-DNS per R2-BEACON §4 recommendation; institutional +
deployment scope so a stray r2-notekeeper or sibling-ensemble
device on the air doesn't get pulled into the rocker's bootstrap
loop.) The class identifies this deployment of the r2-workshop
template per SPEC-R2-WORKSHOP-ENSEMBLE §2.1. For each unique RBID
found, spawn a per-sensor bootstrap task.

Scan duration: 10 s; if zero matches, the loop sleeps 20 s and retries.

### 6.4 Per-sensor bootstrap

Per the R2-BOOTSTRAP flow (with the deviation that the dashboard
initiates per the M10 demo):

1. Connect L2CAP CoC (PSM 0x00D2) — retry up to 5× with 3 s delay.
2. Compose `#wifi_offer {ssid, psk, gateway_ip}`.
3. **Sign** the offer with the TG private key (Ed25519 over canonical
   CBOR per R2-WIFI).
4. Send the signed offer over L2CAP.
5. Disconnect L2CAP.
6. Wait for the sensor to appear at `gateway_ip:21042` (TCP connect
   inbound), with a 60 s presence timeout.
7. On TCP connect, the announce verification (§4.3) closes the loop;
   the bootstrap task reports success to the UI.

Per-sensor tasks run in parallel — sensors are not queued (per the M10
demo's lesson; each sensor filters by its own RBID).

### 6.5 Bootstrap log

Every bootstrap step emits a `r2.dash.bootstrap.progress` event on `/r2` (WIRE row 27)
so the operator can watch progress. The event's inner `event` field
serialises `r2_bootstrap::BootstrapEvent` as `{kind: <variant>, data: <payload>}`:

| `kind` | `data` | When |
|---|---|---|
| `Log` | `"<free-text status line>"` | Generic progress (BLE adapter init, retry counters, hotspot up, etc.) |
| `SensorFound` | `{addr: str, name: str}` | BLE beacon matched; about to connect L2CAP |
| `SensorConnected` | `{addr: str, name: str, ip: str}` | Sensor passed TCP ping/pong validation |
| `Done` | `{count: int}` | Scan window closed; loop continues with next retry interval |
| `Error` | `"<message>"` | Recoverable per-sensor failure |

Additionally the dashboard emits a synthetic `{Reset: null}` event (note:
not in the `{kind, data}` shape) when the operator re-presses *Connect
Sensors*; viewers SHOULD treat this as a signal to clear their bootstrap
log panel before further events arrive.

---

## 7. Per-peer state machine (server view)

| State | Description |
|---|---|
| `KNOWN_OFFLINE` | In `peers.json`, no current TCP session |
| `CONNECTING` | TCP up, awaiting announce |
| `ONLINE_LIVE` | Live frames arriving, latency healthy |
| `ONLINE_CATCHUP` | Receiving batched frames |
| `STALE` | No frame for > 10 s but TCP still open |
| `OFFLINE` | TCP closed; reverts to `KNOWN_OFFLINE` after 30 s grace |

Transitions are observable to the UI via `peer_state` WS events.

---

## 8. Time synchronisation

Per WIRE §7. The dashboard maintains a per-peer offset:

* On TCP connect, send `r2.dash.sync_pulse {req_id, dash_ts_ms}` at
  1 Hz for the first 30 s.
* Thereafter, every 30 s.
* On each `r2.sensor.sync_pong` reply:
  ```
  T1 = dash_ts_ms_at_send
  T3 = dash_ts_ms_at_recv
  T2 = sensor_ts_ms_in_pong
  rtt = T3 − T1
  offset_estimate = T1 + (rtt / 2) − T2
  offset_smoothed = α · offset_estimate + (1 − α) · offset_smoothed
                    where α = 0.2
  ```

To convert a sensor `ts_ms` to dashboard wall clock:

```
ts_local = ts_ms + offset_smoothed[device_pk]
```

The dashboard SHALL store the smoothed offset alongside the peer
metadata (in-memory; recomputed on each session — not persisted).

---

## 9. Server-side calibration

### 9.1 Sample collection

On `POST /api/calibration/:pk/sample {position, ms}`, the dashboard
shall:

1. Send `r2.dash.cal.sample.req {req_id, position, ms}` to the sensor.
2. Await `r2.sensor.cal.sample.resp {req_id, position, gx, gy, gz,
   n_samples}` (10 s timeout).
3. Store `(gx, gy, gz)` in the calibration entry's `g_a` or `g_b`
   field per `position`.
4. Return success to the UI.

### 9.2 Computation

On `POST /api/calibration/:pk/compute` (or automatically once both
positions exist):

```rust
let g_a = vec3(g_a_x, g_a_y, g_a_z);
let g_b = vec3(g_b_x, g_b_y, g_b_z);

let delta = g_b - g_a;
let separation_g = delta.norm() / LSB_PER_G[range];
if separation_g < 0.30 {
    return Err("Calibration swing too small (need ≥ 0.3 g separation)");
}

let main     = delta.normalize();
let vertical = ((g_a + g_b) * 0.5).normalize();
let sideways = main.cross(&vertical).normalize();

// R rotates body-frame samples to (main, sideways, vertical) frame:
let r = Mat3::from_rows(&main, &sideways, &vertical);

calibration.r            = r;
calibration.main_axis    = main;
calibration.sideways_axis = sideways;
calibration.vertical_axis = vertical;
calibration.valid        = true;
```

Where `LSB_PER_G` is `[256000, 128000, 64000]` indexed by range.

The dashboard SHALL refuse calibration when `|g_b − g_a| < 0.30 g`
(PLAN D-18) and surface the error in the UI.

### 9.3 Application

For every received `r2.sensor.acceleration{x,y,z}` from a calibrated
peer, the dashboard computes:

```
(a_main, a_sideways, a_vertical) = R · (x, y, z)  / LSB_PER_G[range]
```

These rotated values are emitted in the WS `acceleration` push (§5.2).
For un-calibrated peers, only `(x_g, y_g, z_g)` raw axes are emitted;
the UI shows a "Calibrate" badge.

---

## 10. Joint groups & differential analysis

### 10.1 Joint definitions

Joints are defined via the UI (`POST /api/joints`); each joint has a
list of member device-pks and a topology (§3.4). For v0.1, the
default joint configuration is created automatically on first run with
each rocker-mounted peer assigned to its own single-sensor joint;
operators promote pairs as additional sensors join.

### 10.2 Per-joint metric (single-sensor)

For a joint with `topology = "single"` containing peer `P`:

```
metric(t) = a_sideways[P, t]
```

Reported as instantaneous magnitude on the WS stream.

### 10.3 Per-joint metric (pair)

For a joint with `topology = "pair"` containing peers `A` and `B`:

```
Δa(t) = a_sideways[A, t_aligned] − a_sideways[B, t_aligned]
```

`t_aligned` requires interpolating both peers' samples to a common
time grid using each peer's smoothed offset (§8). The dashboard
maintains a per-peer ring buffer of the last 10 s of rotated samples
and resamples to a 1 ms grid for differencing.

### 10.4 Reference channel (informative, future)

For joints with a member of `mounting_role = "bed"` declared as a
reference, the dashboard MAY subtract:

```
Δjoint_clean(t) = Δa(t) − a_sideways[bed_ref, t_aligned]
```

This isolates joint-local motion from environmental vibration. Not
required in v0.1.

---

## 11. Stress indicator & trend

### 11.1 Sliding-window RMS

The analytics task shall, at 1 Hz per joint, compute:

```
rms(t) = sqrt( mean( Δa(t − W..t)² ) )      where W = 10 s
peak(t) = max( |Δa(t − W..t)| )
```

Emitted as a `joint_metric` WS event each second.

### 11.2 Long-term trend

The analytics task shall persist daily summaries to disk under
`state/trends/<joint_id>/<YYYY-MM-DD>.json`:

```json
{
  "joint_id": "front-left-actuator",
  "date": "2026-05-07",
  "samples_n": 86400,
  "rms_p50": 0.04, "rms_p95": 0.08, "rms_max": 0.18,
  "peak_p50": 0.10, "peak_p95": 0.22, "peak_max": 0.45,
  "first_ts": "...", "last_ts": "..."
}
```

The trend chart in the UI plots `rms_p50` and `rms_max` against date
for each joint.

### 11.3 Threshold alerting

When `rms(t) > stress_threshold_g_rms` (joint config) for a continuous
2 s window, the dashboard shall:

1. Emit a `joint_alert` WS event.
2. Append to `events.log`.
3. Surface a banner in the UI until acknowledged.

Thresholds are configured per joint via the UI; default 0.25 g RMS,
adjustable empirically once baseline data is collected.

---

## 12. Browser UI requirements

### 12.1 Layout

The SPA shall use a **CSS grid auto-fit** container with `min-width:
320px` per card so the layout reflows from 1 column (mobile) up to
~6 columns at 1920 px width.

A persistent header at the top shows:

* Total peers (online / known).
* Total joints with current stress: green / yellow / red counts.
* Connection-state widget for the dashboard itself.
* "Discover" button (BLE bootstrap trigger).

### 12.2 Cards

Three card types arranged in the grid:

1. **Peer card** — one per online sensor:
   * Hostname / label / mounting role.
   * Live state (LIVE / CATCHUP / STALE / etc.).
   * Battery widget (voltage + percent + charging icon).
   * Mini-chart (canvas, 200 × 60 px) showing `a_sideways` for the
     last 30 s (or `a_z` if uncalibrated).
   * Status line: `seq`, `rate_hz`, `sd_pct_used`.
   * Actions: Calibrate / Stream Start / Stream Stop / OTA / Reset
     (collapsed into a menu).

2. **Joint card** — one per defined joint:
   * Joint label.
   * Member peer hostnames.
   * Stress gauge: current RMS + threshold, colour-coded.
   * Trend sparkline (7 days).
   * Actions: edit threshold, edit members.

3. **Bootstrap log card** — appears during discovery:
   * Per-sensor stage progress.
   * Auto-dismisses 30 s after discovery completes.

### 12.3 Charts

For sensor count > 8, the per-peer mini-charts shall render via
**HTML5 Canvas** (not SVG / Plotly) to avoid jank. Update rate ≤
30 Hz; coalesce multiple samples per frame. Larger detail charts
(opened by clicking a peer) MAY use richer libraries.

### 12.4 Calibration flow

A modal wizard with three steps:

1. "Place rig at position A. Hold steady. Click Capture."
2. "Move rig to position B. Hold steady. Click Capture."
3. "Calibration computed. Δ = X.XX g across (mx, my, mz). Save?"

If `|g_b − g_a| < 0.3 g`, the wizard refuses to advance past step 2
with a clear error.

### 12.5 Accessibility

The UI shall be usable with keyboard navigation, screen-reader labels
on all interactive elements, and prefers-reduced-motion respected for
chart animations. Colour-only signalling is forbidden — every red /
yellow / green state has a textual or iconographic counterpart.

---

## 13. OTA push

### 13.1 Trigger

Operator submits a firmware push via `POST /api/ota/{addr}` with the
binary as the request body (the one operator-plane HTTP route that
survived v0.2 — multi-MB binary uploads are the wrong shape for per-
frame WS, per §5.1). The dashboard shall:

1. Verify the host serving `url` is reachable (HEAD request, 5 s
   timeout).
2. Compute and verify SHA-256 of the binary by streaming a HEAD-then-GET
   if possible, or trust-but-warn.
3. (v1.0) Sign the `(url || sha256)` tuple with the TG private key.
4. Send `r2.dash.fw.update {url, sha256, tg_sig}` to the peer.
5. Monitor `r2.sensor.event.log` for `OTA_*` codes; surface to UI.
6. After the sensor reboots and reconnects, log success.

### 13.2 Bulk OTA

The dashboard exposes a fleet-wide "Update All Firmware" affordance
in the Devices view. v0.2 implementation:

* The browser file-picker accepts a single `.bin`; the filename SHOULD
  follow the `r2-workshop-firmware-<fw_ver>.bin` convention used by
  `tools/build-firmware.sh`.
* If the filename's `<fw_ver>` segment parses, the webapp compares
  it against each peer's announced `fw_ver` and skips peers already on
  that version. Operator sees a confirmation dialog reporting
  `<targets>` to push and `<skipped>` already matching.
* If the filename does NOT parse (operator renamed it), the dialog
  warns and offers to push to every peer.
* Pushes run in **parallel** via `Promise.allSettled` over the
  existing per-sensor `POST /api/ota/{addr}` route — the dashboard has
  no fleet-level endpoint. Failure of one peer does not abort the
  others; each peer's error is logged client-side and shown via the
  existing OTA `r2.dash.ota.progress` event stream on `/r2`.
* Concurrency is uncapped for the v0.1 fleet size (≤ ~4 sensors). If
  the fleet grows the webapp SHOULD cap concurrency at 2 to keep the
  hotspot uplink from saturating; the original spec's "sequential by
  default, rolling parallel later" guidance still applies in spirit.

Fleet-wide reset works the same way but rides as `r2.dash.cmd.reset`
events on `/r2` (WIRE row 33): "Reset All Sensors" button on the
Devices view, confirmation dialog, `Promise.allSettled` over per-peer
`sendCmd("r2.dash.cmd.reset", {1: addr})` calls.

### 13.3 Firmware availability + "needs update" dot

The dashboard exposes a snapshot of the latest available firmware so
the webapp can flag any device whose announced `fw_ver` is out of
date. **The match is by (class, carrier) tuple, version is the
delta** — a sensor only "needs update" against a binary built for
the same class+carrier; everything else gets filtered out.

**Release filename convention** (canonical, both GitHub Releases +
the local fallback dir):

```
r2-workshop-firmware-<class-slug>-<carrier>-<version>+<git>.bin
```

* `<class-slug>` is the reverse-DNS class string with dots replaced
  by hyphens (`nz.ac.auckland.rocker` → `nz-ac-auckland-rocker`).
* `<carrier>` matches the announce's CBOR key 12 (`devkitc`, `xiao`, …).
* `<version>` is the firmware version per release-mode rules below.
* `<git>` is the short git sha at build time.

Each released `.bin` is accompanied by a **meta sidecar**
`r2-workshop-firmware-<class-slug>-<carrier>-<version>+<git>.bin.meta.json`:

```json
{
  "class":   "nz.ac.auckland.rocker",
  "carrier": "xiao",
  "version": "0.3.0",
  "git":     "a1b2c3d",
  "sha256":  "<hex>",
  "built":   "2026-05-28T07:00:00Z"
}
```

The sidecar is authoritative; if it differs from the filename the
dashboard SHALL trust the JSON and surface a warning. This decouples
the OTA-matching logic from filename string-parsing and lets
operators rename binaries (e.g. for archive organisation) without
breaking the match.

**Sources (in order of preference):**

1. **GitHub Releases** on `reality2-ai/r2-workshop` — queried via the
   public REST API. The dashboard filters the asset list down to
   binaries matching its own `(class, carrier)` per the filename
   convention above, then fetches the sidecar for each survivor
   (or parses the filename when the sidecar is missing, for
   pre-v0.3 releases).
2. **Local `firmware/<soc-family>/<carrier>/releases/` directories** —
   the dashboard walks every `firmware/<soc-family>/<carrier>/releases/`
   tree under the repo (as of 2026-05-31 those are `esp32-s3/devkitc`,
   `esp32-s3/xiao`, and `esp32-c6/dfr1117`) and picks the highest-mtime
   `.bin` per `(class-slug, carrier)` tuple. The **carrier comes from
   the directory** (not the filename); **class + version + sha256 come
   from the `<bin>.meta.json` sidecar** when present (authoritative),
   falling back to the dashboard's own class + a filename-parsed version
   for pre-v0.3 archives that shipped no sidecar. A sidecar whose `class`
   is foreign is skipped. The sensor's announced `carrier` (WIRE §3.1
   row 12) selects which entry it's matched against. Used when the
   GitHub query fails (no internet, repo private, rate-limit) so the
   operator is never stuck without the dot working.

   > **Local filename note.** Unlike GitHub release assets, the local
   > archive keeps the timestamped dev-build name
   > (`r2-workshop-firmware-<fw_ver>.bin`, where `fw_ver` embeds a build
   > timestamp) rather than the canonical `<class-slug>-<carrier>`
   > filename — so repeated dev builds at the same semver don't clobber
   > each other. The sidecar carries the authoritative `(class, carrier,
   > version, sha256)` tuple, so matching never depends on the local
   > filename. `tools/build-firmware.sh` emits the sidecar alongside
   > each archive.

**Endpoint:**

```
GET /api/firmware/available
→ {
    "source":  "github" | "local" | "none",
    "class":   "<dashboard's configured class>",
    "version": "<tag or fw_ver>",
    "assets":  [{ "class":   "<class>",
                  "carrier": "<carrier>",
                  "version": "<fw_ver>",
                  "url":     "<https url for github, fs path for local>",
                  "sha256":  "<from sidecar when available>",
                  "size":    <bytes> }],
    "note":    "<optional note when GitHub query fell back to local>",
    "fetched_at_ms": <unix ms>
  }
```

Assets returned MUST have `class` equal to the dashboard's own
class — the operator's dashboard never offers them a foreign-class
binary as an "available update". (A foreign-class binary is still
*usable* via manual OTA per §13.4 — operator-confirmed re-deploy —
but it's not auto-surfaced.)

Cached for **5 minutes** to stay under GitHub's 60/hr/IP
unauthenticated rate limit. Webapp polls every 60 s.

**Binary fetch:**

```
GET /api/firmware/{carrier}/binary
→ application/octet-stream stream of the .bin bytes
```

The dashboard always **proxies** the bytes (rather than 302-ing the
browser to GitHub directly) — see v0.2's CORS rationale.

**Release-mode firmware build:**

For the GitHub-side comparison to make sense, the released `.bin`
must bake an `fw_ver` string equal to the tag name. The firmware's
`build.rs` checks `R2_RELEASE=1` and `git describe --tags
--exact-match`: if both pass, `fw_ver` is just the tag (`v0.3.0`);
if either fails (dirty tree, no tag), the build panics with a clear
error.

**Webapp UX:**

On each device card the webapp shows a small pink dot next to the
firmware line when:

* `availableFirmware.version` is known, AND
* there's an asset whose `(class, carrier)` matches the device's
  announce, AND
* the device's announced `fw_ver` is older than that asset's
  version.

Foreign-class or foreign-carrier devices in the same radio range
(reported via the bootstrap-scan path, [[feedback_check_r2_specs]])
do NOT receive a "needs update" dot — there's no valid update for
them in this dashboard's release stream.

### 13.4 Manual OTA push — (class, carrier) validation

When the operator submits a `.bin` via the "Update Firmware…"
file-picker (single device) or "Update All Firmware…" (fleet),
the webapp SHALL inspect the binary's identity before pushing.
Identity resolution order:

1. **Companion meta sidecar.** The webapp checks for
   `<basename>.meta.json` alongside the picked file. If present
   and valid, the `class` + `carrier` + `version` fields are
   authoritative.
2. **Filename parse.** If no sidecar, the webapp parses the
   filename against the §13.3 convention. Successful parse →
   identity recovered with a "filename-parsed, sidecar absent"
   note in the confirmation dialog.
3. **Unknown.** Neither sidecar nor filename matches → the
   webapp warns ("identity unknown; cannot verify match") and
   defaults to **cancel**; operator may override with an
   explicit confirm tick.

For each target sensor in the push, the webapp compares the
recovered `(class, carrier)` against the target's announce:

| Case | UX |
|---|---|
| `class` matches, `carrier` matches, version newer | Push silently — this is the normal update path. |
| `class` matches, `carrier` matches, version same | Skip with note "already on version X". |
| `class` matches, `carrier` matches, version older | Confirm "downgrade from Y to X — proceed?"; default cancel. |
| `class` **differs**, `carrier` matches | Yellow warn: "Different class (`<their>` → `<file>`). Continuing will re-deploy this sensor into a different TG. Proceed?"; default cancel. |
| `carrier` **differs** (any class) | Red warn: "Different carrier (`<their>` → `<file>`). This is very likely to brick the sensor. Proceed?"; default cancel. Carrier-mismatch is on a separate confirmation step to make the override deliberate. |

Bulk pushes accumulate the per-target verdicts and present the
operator with a single dialog summarising counts in each bucket
before any HTTP request fires. After confirmation the dashboard
runs the per-target pushes in parallel (`Promise.allSettled`)
exactly as v0.2 did — the validation gate is purely client-side
preflight; the per-target `POST /api/ota/{addr}` HTTP route is
unchanged.

**Sensor-side fail-safe.** The sensor's OTA receiver
(`r2-esp::ota_tcp`) MUST also reject a binary whose embedded
`carrier` constant differs from its own — the dashboard's gate is
primary UX, but the sensor is the last line of defence against a
wrong-carrier flash. Implementation: the OTA preamble carries the
incoming binary's class+carrier (parsed from the same `esp_app_desc_t`-
adjacent metadata block the build pipeline writes; see SENSOR §12);
sensor refuses to write the partition on carrier mismatch and emits
`r2.sensor.event.log {code: OTA_CARRIER_MISMATCH}`.

### 13.5 Server bundle distribution (Hive + webapp)

> **Not an OTA path.** Firmware updates push wirelessly to sensors
> (§13.1–§13.4); the controller Hive itself is installed/updated by an
> operator on the controller host. This section defines how that
> server artefact is built and published, reusing the §13.3 release-asset
> + meta-sidecar convention so the two distribution streams stay
> symmetric.

The controller is published as a self-contained per-architecture
tarball alongside the firmware assets on the same GitHub Release. Of
the three things that compile (the WASM hive `crates/r2-wasm`, the
firmware, and the core Hive `r2-dashboard`), only the **core Hive
binary is architecture-specific**. The **WASM hive + webapp are
architecture-independent** — built once and dropped into every bundle.

**Bundle filename convention** (mirrors §13.3):

```
r2-workshop-server-<class-slug>-<version>-linux-<arch>.tar.gz
```

* `<class-slug>` — reverse-DNS class with dots → hyphens, as §13.3.
* `<version>` — the release tag (e.g. `v0.3.0`); the binary's baked
  `DASHBOARD_VERSION` (= `CARGO_PKG_VERSION+<git>`) MUST come from a
  clean, tagged checkout so it has no `-dirty` suffix.
* `<arch>` — `uname -m` of the target: `x86_64` (Intel/AMD) or
  `aarch64` (ARM, e.g. Raspberry Pi). Both run 64-bit Linux.

**Bundle contents** (extract-and-run, no system install required):

```
r2-dashboard                 # core Hive binary for <arch>
webapp/                      # static UI + webapp/pkg/ WASM hive (arch-independent)
tools/start-server.sh        # one-command launcher (build step skipped — binary present)
tools/install-launcher.sh    # optional desktop icon + `r2-workshop` PATH command
RUN.md                       # extract → ./tools/start-server.sh → http://localhost:21042/
```

Each tarball ships a **meta sidecar**
`r2-workshop-server-<class-slug>-<version>-linux-<arch>.tar.gz.meta.json`:

```json
{
  "class":   "nz.ac.auckland.rocker",
  "kind":    "server",
  "arch":    "aarch64",
  "version": "0.3.0",
  "git":     "a1b2c3d",
  "sha256":  "<hex of the .tar.gz>",
  "built":   "2026-06-07T00:00:00Z"
}
```

As in §13.3 the sidecar is authoritative over the filename.

**Build (`tools/build-server.sh`):**

1. Build the WASM hive once on the build host:
   `wasm-pack build crates/r2-wasm --target web --release --out-dir webapp/pkg`.
2. Build `r2-dashboard --release` for **x86_64** natively on the build host.
3. Build `r2-dashboard --release` for **aarch64** natively on a real
   ARM host (the project uses a Raspberry Pi 5 over Tailscale SSH) from
   a **clean `git` checkout of the release tag** — not a cross-compile —
   to avoid the `openssl` (native-tls) cross-toolchain burden and to
   keep the version string clean. The binary is copied back to the
   build host for packaging.
4. Assemble the per-arch tarball + sidecar above.

Native-on-target (rather than cross-compilation) is the chosen
mechanism while the Hive links `openssl` via `tokio-tungstenite`'s
`native-tls` feature; should that move to `rustls`, static
`*-musl` cross-builds become viable and MAY replace the remote build.

### 14.1 CLI / config file

The dashboard reads config in this precedence (highest first):

1. CLI flags (e.g. `--state-dir`, `--http-port`, `--tcp-port`, `--tg-priv`).
2. Environment variables (`R2_WORKSHOP_STATE_DIR`, etc.).
3. `~/.config/r2-workshop/dashboard.toml`.
4. Built-in defaults.

### 14.2 Required keys

| Key | Default | Description |
|---|---|---|
| `state_dir` | `~/.config/r2-workshop/state/` | Where peers/calibration/joints live |
| `tg_priv_path` | `~/.config/r2-workshop/tg_signer/tg_priv.bin` | TG signing key (off-tree) |
| `tg_pub_path` | `<repo>/trust_keys/tg_pub.bin` | TG verifier key (in-tree) |
| `http_port` | 8080 | UI |
| `tcp_port` | 21042 | Sensor inbound |
| `wifi_iface` | `wlan0` (linux) | Hotspot adapter |
| `hotspot_ssid_prefix` | `r2-workshop-` | Generated SSID = prefix + 6 hex |

The TG private key path must be **outside** the repo working tree
(see SECRETS-POLICY).

---

## 15. Conformance

A dashboard build conforms to this specification when the following
acceptance tests pass against the reference firmware (or a simulator
implementing `SPEC-R2-WORKSHOP-WIRE`):

### 15.1 Listener acceptance

1. TCP :21042 accepts a connection, decodes a valid announce, admits
   the peer to `peers.json`, replies with an ACK within 100 ms.
2. Malformed length prefix closes the connection; the dashboard does
   not crash.
3. Concurrent connections from 30 simulated peers complete announce
   within 2 s of each peer's connect.

### 15.2 UI acceptance

1. Browser at `http://localhost:21042/` loads the SPA in < 1 s on a
   modern laptop.
2. Per-peer cards update at ≥ 10 Hz; with 30 simulated peers
   transmitting at 100 Hz, the browser's main thread frame rate stays
   ≥ 30 fps.
3. Calibration wizard refuses `|g_b − g_a| < 0.3 g`.
4. Stream start / stop / OTA / reset commands round-trip to a
   connected peer and reflect back as state changes within 2 s.

### 15.3 Bootstrap acceptance

1. With a fresh sensor advertising R2-BEACON, the "Discover" flow
   completes (BLE → L2CAP → wifi_offer → WiFi join → TCP announce)
   within 60 s.
2. The signed `#wifi_offer` verifies on the sensor (announce arrives).
3. Aborting discovery (UI button) cleanly tears down in-flight L2CAP
   sessions and leaves the WiFi hotspot intact.

### 15.4 Analytics acceptance

1. With a single calibrated peer, the dashboard's `a_sideways` matches
   a hand-computed value (via captured raw samples and `R`) to within
   1% over a 60 s window.
2. With a paired joint, a synthetic correlated motion injected into
   both peers produces near-zero `Δa`; a synthetic differential
   produces the expected magnitude.
3. Threshold alerting fires within 2.5 s of a sustained over-threshold
   condition; clears within 5 s of the condition ending.

### 15.5 Persistence acceptance

1. Dashboard restart preserves peers, calibration, joints, and
   high_water; on reconnect, sensors resume from `last_acked` without
   data loss.
2. Killing the dashboard during write does not corrupt JSON files
   (atomic replace via tempfile + rename).

### 15.6 Capture auto-sync acceptance

1. A sensor `Recording → Idle` transition triggers a fetch of the
   just-finalised file (main + sidecar if present) within 5 s of
   the transition arriving on `/r2`. The local file lands under
   `$XDG_DATA_HOME/r2-workshop/captures/` (fallback
   `~/.local/share/...`) with the spliced CSV header for the main
   file and byte-for-byte sidecar.
2. Each successful local-write emits `r2.dash.capture.synced`
   (WIRE row 44) on `/r2`.
3. The 60-second reconciliation poll fetches anything missed
   (sensor reconnected after Stop, dashboard restarted mid-run)
   without operator action.
4. Files already present in the captures dir at startup are
   indexed and not re-fetched.
5. Sensor-side files remain on the SD after sync; the only way
   to delete them is the existing per-file `DELETE` route or
   `/api/data/{addr}/all`.

### 15.7 Event-mark acceptance

1. `r2.dash.cmd.capture.event_mark` with non-empty `label`:
   controller stamps `ts_ms` from its own clock, assigns a
   monotonic `mark_id`, fans out `r2.dash.capture.event_mark`
   (WIRE row 45) to every connected peer, and emits
   `r2.dash.capture.event_marked` (row 46) on `/r2` within 200 ms.
2. With empty `label`, the controller substitutes `"mark"`.
3. With at least one peer Recording, the sidecar
   `<stem>.marks.csv` for that session appears on the sensor's SD
   with one row per event; after Stop the sidecar reaches the
   captures dir via §15.6 sync, with `kind: "marks"` on the
   accompanying `r2.dash.capture.synced` event.
4. The `r2.dash.cmd.response` for the cmd echoes `req_id` and
   carries `kind: "capture.event_mark"`.

### 15.8 (class, carrier) matched OTA acceptance

1. `GET /api/firmware/available` returns only assets whose
   `class` matches the dashboard's own configured class. A
   foreign-class binary on the same GH release MUST be excluded
   from the response.
2. Per-sensor "needs update" dot lights only when an asset's
   `(class, carrier)` matches the sensor's announce AND the
   asset's version is newer than the announced `fw_ver`.
3. Manual OTA upload of a `.bin` whose carrier matches the
   target proceeds without extra confirmation when class also
   matches and version is newer.
4. Manual OTA upload of a `.bin` whose **class** differs from the
   target triggers a yellow confirmation dialog defaulting to
   cancel; explicit confirm proceeds with the push.
5. Manual OTA upload of a `.bin` whose **carrier** differs from
   the target triggers a red confirmation dialog defaulting to
   cancel and requiring a deliberate override tick. Confirmation
   alone is insufficient — operator must also tick "I understand
   this may brick the sensor".
6. Binaries without a meta sidecar AND a non-conforming filename
   are accepted only after the operator explicitly waives identity
   verification. Default: cancel.
7. The OTA push HTTP route (`POST /api/ota/{addr}`) MUST NOT
   short-circuit any of the above; the gate is webapp-side.
   Sensor-side carrier-mismatch fail-safe (§13.4) is the
   independent backstop.

---

## 16. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-07 | 0.1 | Initial draft. Process model, listeners, bootstrap, calibration, joints, analytics, UI, OTA, conformance. |
| 2026-05-26 | 0.2 | §5.1 adds `/api/data/local/list`, `/api/data/local/file/{name}`, and folds in `/api/data/zip` (now sources from controller-local store). §15.6 + §15.7 add capture auto-sync + event-mark acceptance tests per SPEC-R2-WORKSHOP-CAPTURE §7.4 + §7.5. |
| 2026-05-28 | 0.3 | Heterogeneous-fleet OTA: §13.3 rewritten to filter `/api/firmware/available` by `(class, carrier)` against the dashboard's own class. New §13.4 specifies the manual OTA validation gate (class-mismatch → yellow warn; carrier-mismatch → red double-confirm). §15.8 adds the matching acceptance tests. Sensor-side fail-safe documented in §13.4 (last paragraph) — cross-ref into SENSOR §12 OTA. |
| 2026-05-28 | 0.3.1 | §1 introduction reframed: the dashboard is the r2-workshop ensemble's controller hive — hosting the dashboard-side sentants per SPEC-R2-WORKSHOP-SENTANTS §4 + the R2-WEB plugin registration that delivers the operator UI. Cross-references the new SPEC-R2-WORKSHOP-ENSEMBLE. |
| 2026-06-07 | 0.3.2 | New §13.5 specifies the **server bundle distribution** convention — per-arch (`x86_64`, `aarch64`) `r2-workshop-server-<class>-<version>-linux-<arch>.tar.gz` tarballs (core Hive binary + arch-independent webapp/WASM hive) + meta sidecars, published alongside firmware on the same GitHub Release. Built by `tools/build-server.sh` (native x86_64 + native aarch64 on a Pi over SSH, from a clean tagged checkout; not cross-compiled while the Hive links openssl). |
