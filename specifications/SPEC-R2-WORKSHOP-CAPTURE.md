# SPEC-R2-WORKSHOP-CAPTURE: Named experimental captures

**Version:** 0.1 Draft
**Date:** 2026-05-18
**Status:** Normative Draft
**Depends on:** SPEC-R2-WORKSHOP-SENSOR (§6 SD ring), SPEC-R2-WORKSHOP-WIRE, SPEC-R2-WORKSHOP-TIMESYNC, SPEC-R2-WORKSHOP-SENSOR-HEALTH

---

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**,
**SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**,
and **OPTIONAL** in this document are to be interpreted as
described in [RFC 2119](https://www.rfc-editor.org/info/rfc2119),
when they appear in capitals.

---

## 1. Introduction

The rolling SD ring (`/sdcard/logNNNN.csv` per
SPEC-R2-WORKSHOP-SENSOR §6) is a continuous backstop for the live
stream. It is **not** the right format for deliberate experimental
runs — there is no operator-given name, no per-run calibration,
no notion of "this run starts here, ends there".

This specification defines the **capture** workflow: discrete,
named, calibration-zeroed CSV files created on every sensor's SD
card in lockstep with a controller-driven Start → Mark → Stop
sequence. Captures live alongside the rolling ring, not in place
of it.

### 1.1 Scope

In scope:

* The three-state capture lifecycle (Idle → Calibrating →
  Recording → Idle).
* Four R2-WIRE events that drive it.
* On-disk layout under `/sdcard/captures/`.
* Calibration semantics (fixed-window baseline, additive
  per-axis offset applied to the row values).
* Sensor-side TCP listener `data_tcp` on port 21047 that
  enumerates, downloads, and deletes capture files for the
  dashboard.

Out of scope:

* Calibration that compensates for orientation or temperature.
  v0.1 captures a static per-axis additive offset and stops
  there; finer-grained calibration is a future extension.
* Crash-safety guarantees beyond fsync-on-Stop. A loss of power
  mid-Recording **MAY** leave a partially-written file; that
  partial file is still valid CSV up to its last fsync.
* Re-arming the calibration offset across boots. Each capture
  session re-calibrates from scratch — a sensor reboot between
  Mark and Stop **MUST** result in the file being closed by the
  next dashboard Stop or by file-list cleanup.

### 1.2 Terminology

* **Capture session** — one Start → Mark → Stop triple.
* **Calibration window** — the `CAL_WINDOW_MS` milliseconds
  immediately after Start during which the sensor accumulates a
  baseline mean per axis.
* **Capture offset** — the locked-in mean computed at Mark,
  applied as `output_axis = raw_axis - offset_axis` to every row
  written during Recording.
* **Capture file** — a CSV at
  `/sdcard/captures/<ts>-<name>.csv` written exclusively during
  Recording.
* **Run name** — the operator-supplied label, sent by the
  controller in `r2.dash.capture.mark`.

---

## 2. State machine

```
                       Start            Mark
              Idle ─────────► Calibrating ─────► Recording
                ▲                  │                │
                │                  │ Start (re-arm) │ Stop
                │                  ▼                │
                └───────── Stop ◄──────────────────┘
```

Transitions:

| From | Event | To | Action |
|---|---|---|---|
| `Idle` | `r2.dash.capture.start` | `Calibrating` | Reset the cal accumulator. Stamp `cal_start_ms = clock.ts_ms_i64()`. LED `Calibrating` (purple). |
| `Calibrating` | every sample during the window | `Calibrating` | Add the raw sample to the running sum. |
| `Calibrating` | `cal_start_ms + CAL_WINDOW_MS` elapsed (a sample arrives after that) | `Calibrating` (locked) | Mean of the accumulated samples becomes the candidate offset. Further samples are dropped from the accumulator. |
| `Calibrating` | `r2.dash.capture.mark` | `Recording` | Lock the candidate offset as `capture_offset`. Open `/sdcard/captures/<ts>-<name>.csv` for write (filename built from the payload's `ts_ms` + `name`). Begin writing **calibrated** rows. LED returns to `StreamingLive` / `StreamingDegradedSim`. |
| `Recording` | `r2.dash.capture.stop` | `Idle` | `sync_all()` the file, close it, drop the `capture_offset`. |
| `Recording` | `r2.dash.capture.start` | `Calibrating` | Equivalent to Stop then Start in one event. File is closed via fsync; the new cal window begins. |
| any | `r2.dash.capture.stop` while `Idle` | `Idle` | No-op. **MUST** acknowledge silently. |

Behaviour while `Calibrating` but before `CAL_WINDOW_MS` has
elapsed and a `r2.dash.capture.mark` arrives early: the firmware
**MUST** clamp the accumulated mean to the samples received so
far and proceed to `Recording`. Operators **SHOULD NOT** Mark
inside the window — the controller's UI **SHOULD** disable the
Mark button until the window has elapsed.

`CAL_WINDOW_MS` **SHALL** default to **2000 ms**. Carriers
**MAY** override via an NVS key (a future spec extension).

---

## 3. Wire events

All four events ride the existing R2-WIRE compact frame on the
streaming TCP session (port 21042). CBOR payloads use the
integer-key + smallest-encoding convention from R2-WIRE / R2-CBOR.

| Event name | Hash (FNV-1a-32) | Direction | Payload |
|---|---|---|---|
| `r2.dash.capture.start` | computed at compile time | dash → sensor | `{}` (empty CBOR map) |
| `r2.dash.capture.mark`  | computed at compile time | dash → sensor | `{0: i64 ts_ms, 1: str name, 2: str prefix?}` (key 2 optional) |
| `r2.dash.capture.stop`  | computed at compile time | dash → sensor | `{}` |
| `r2.sensor.capture.state` | computed at compile time | sensor → dash | `{0: u8 state, 1: str file_opt}` where `state ∈ {0=idle, 1=calibrating, 2=recording}` and `file` is the open filename when `state=2`, omitted otherwise |
| `r2.dash.capture.event_mark` | computed at compile time | dash → sensor | `{1: u64 ts_ms, 2: str label, 3: u32 mark_id}` — operator annotation injected into the active capture. Sensors **MUST** apply only while in `Recording`; silently ignored otherwise. Writes one row to the sidecar `<stem>.marks.csv` (§4.1) — main capture file is untouched. See §7.5 for the controller-side fan-out. |

Sensors **MUST** emit `r2.sensor.capture.state` on every state
transition. The controller uses these to update the webapp.

The `name` field on `r2.dash.capture.mark`:

* **SHALL** be UTF-8.
* **SHALL** be no longer than 32 bytes.
* **SHALL** match `[A-Za-z0-9_-]+`. Any character outside that
  charset **MUST** cause the sensor to refuse the Mark, remain in
  `Calibrating`, and emit a `r2.sensor.event.log` with code
  `CAPTURE_BAD_NAME`.

The `ts_ms` field is supplied by the dashboard so every sensor in
the fleet builds the **same** filename. Sensors **MUST NOT**
substitute their local clock at file-open time.

The optional `prefix` field carries a pre-formatted local-time stem
(typically `YYYY-MM-DD_HH-MM-SS`) used as the date portion of the
filename in place of the zero-padded `ts_ms`. The dashboard's
webapp formats this from the operator's browser timezone so the
file on disk is human-dated in local time instead of UTC epoch ms.
The `prefix` charset is restricted to `[0-9_-]` (length 1..32);
sensors **MUST** refuse a Mark whose prefix violates the charset
(same handling as `CAPTURE_BAD_NAME`). When `prefix` is absent,
sensors **MUST** fall back to the legacy `<ts16>` convention so
older dashboards continue to work.

---

## 4. Filesystem layout

Capture files live under a sub-directory of the SD mount root:

```
/sdcard/
├─ log0001.csv               ← rolling-ring segment, untouched
├─ log0002.csv
├─ …
└─ captures/
   ├─ 0001779000000000-run-01-asphaltA.csv
   ├─ 0001779000003000-run-02-asphaltA.csv
   └─ …
```

Filename convention: `<prefix>-<name>.csv` where `<prefix>` is one of:

* **local-time stem** (preferred) — `YYYY-MM-DD_HH-MM-SS` carried in
  payload key 2 of `r2.dash.capture.mark` (§3). Example:
  `2026-05-18_13-35-00-run-01-asphaltA.csv`. Human-readable in the
  operator's timezone; lex-sortable for that timezone's wall clock.
* **`<ts16>`** (fallback) — the dashboard-supplied `ts_ms` rendered
  as a **16-digit zero-padded decimal**. Used when the dashboard
  omits key 2 (older builds, or no browser to source the local-time
  stem). Lex-sortable as UTC epoch ms.

`<name>` is the validated run name in both cases.

This filename is **longer than 8.3** and therefore requires FATFS
Long-Filename support to be enabled in the firmware build. ESP-IDF
disables LFN by default. Conforming sensor builds **SHALL** set:

```
CONFIG_FATFS_LFN_HEAP=y
CONFIG_FATFS_MAX_LFN=255
```

(or `CONFIG_FATFS_LFN_STACK=y`) in `sdkconfig.defaults`. Without
this, every `File::create` for a capture filename fails with
`EINVAL`/`ENOENT` and the capture state machine can never leave
`Calibrating`.

Sensors **MUST** create the `captures/` sub-directory if absent
(via `fs::create_dir_all`). If `create_dir_all` fails (e.g. due
to the ESP-IDF FATFS quirk noted in
SPEC-R2-WORKSHOP-SENSOR §6.1), the sensor **MAY** fall back to
placing capture files at the SD root with a `cap-` prefix
(`cap-<ts16>-<name>.csv`). The `data_tcp` LIST command **SHALL**
return either layout transparently.

Row format: **identical** to the rolling ring (62-byte
fixed-width CSV per SPEC-R2-WORKSHOP-SENSOR §6.2 v0.2) **except**
the x, y, z columns carry calibrated values:

```
output_x = raw_x − capture_offset.x
output_y = raw_y − capture_offset.y
output_z = raw_z − capture_offset.z
```

The `seq` and `ts_ms` columns are unchanged.

### 4.1 Event-mark sidecar (`<stem>.marks.csv`)

Per-capture sidecar file co-located with the main capture in
`/sdcard/captures/`. Created lazily on the first
`r2.dash.capture.event_mark` for a given session, and only while
the sensor is in `Recording` (the `CurrentRecording` lock holds
the active stem — see §6.4). If no event marks are ever issued
for a session, no sidecar exists for that session.

Stem convention: same `<prefix>-<name>` as the main capture, with
the `.marks.csv` suffix replacing `.csv`. Example for a Mark named
`run-01-asphaltA`:

```
captures/2026-05-18_13-35-00-run-01-asphaltA.csv         ← main capture
captures/2026-05-18_13-35-00-run-01-asphaltA.marks.csv   ← sidecar
```

File format — plain UTF-8 CSV (text, not the fixed-width binary
layout of the main file). Sensors **SHALL** emit on file create:

```
# r2-workshop event marks v1
ts_ms,mark_id,label
```

then one data row per `event_mark` received while Recording:

```
<ts_ms>,<mark_id>,<label_escaped>
```

where `<ts_ms>` and `<mark_id>` are the values from the wire
event's payload (controller-stamped, not local clock), and
`<label_escaped>` is the label with `"` doubled and the whole
field wrapped in `"` if it contains comma, quote, or newline (RFC
4180 minimal escaping). Sensors **MUST** `fsync` after each
append so power-loss can lose at most the in-flight row.

The sidecar is **session-level**, not per-device — every sensor
in the fleet that was Recording at the time receives the same
event and writes the same row to its local sidecar. The auto-sync
engine (§7.4) treats the sidecar as a normal capture artefact and
fetches it like any other file.

---

## 5. Calibration semantics

The capture offset is a static per-axis additive value computed
once per session:

```
offset.x = mean(raw_x_i)  for samples i in the cal window
offset.y = mean(raw_y_i)
offset.z = mean(raw_z_i)
```

The mean is integer division over signed `i32` accumulators
(saturating add). Sample sources during calibration are the same
as during normal sampling — real ADXL355 or sim per
SPEC-R2-WORKSHOP-SENSOR-HEALTH; sim-fallback samples **MAY** be
included in the calibration mean (operators wanting a clean
baseline should ensure no sim-fallback before Mark).

`CAL_WINDOW_MS = 2000` at 100 Hz yields ≈ 200 samples per axis,
which is sufficient to drive the per-axis standard error below
1 LSB at ±2 g for a stationary mount.

The rolling ring **MUST** continue writing **raw** (uncalibrated)
samples regardless of capture state. The durable backstop never
depends on a per-session calibration value.

---

## 6. `data_tcp` listener (port 21047)

A dedicated TCP listener on the sensor enumerates, fetches, and
deletes capture files for the dashboard. Mirrors the ergonomics
of `ota_tcp` (port 21043) and `reset_tcp` (port 21044).

### 6.1 Framing

Plain binary framing — no CBOR — chosen for `xxd`/`nc`-readable
wire vectors and a tight implementation on a small heap. Every
command begins with a single-byte opcode; bodies use big-endian
length-prefixed strings and big-endian integers.

```
client → sensor : [opcode u8][body…]
sensor → client : [status u8][body…]
```

Status bytes:
* `0x00 OK`
* `0x01 ERROR` — body is `[u16 BE msg_len][msg utf-8]`
* `0x02 BUSY` — capture is `Recording`; the requested file is the
  one currently open. Body is `[u16 BE msg_len][msg utf-8]`.
  Client **SHOULD** retry after a Stop.

### 6.2 Opcodes

| Opcode | Name | Request body | Response on OK |
|---|---|---|---|
| `0x01` | `LIST` | (none) | `[u32 BE count]` then `count` × `[u16 BE name_len][name utf-8][u64 BE size][i64 BE mtime_ms]` |
| `0x02` | `GET`  | `[u16 BE name_len][name utf-8]` | `[u64 BE size][size bytes raw file content]` |
| `0x03` | `DEL`  | `[u16 BE name_len][name utf-8]` | (empty) |
| `0x04` | `DEL_ALL` | (none) | `[u32 BE deleted_count]` |

The sensor **SHALL** refuse `GET` and `DEL` on the
currently-recording file with `BUSY`. `DEL_ALL` **SHALL** skip
the currently-recording file and report the surviving count
correctly.

The sensor **SHALL** reject any `name` that doesn't match the
basename charset `[A-Za-z0-9_.-]{1,64}` — guards against path
traversal. The webapp never composes a name itself; it passes
back the basenames it received from a prior `LIST`.

### 6.3 Resource budget

* Listener thread stack: 8 KiB.
* Per-client name buffer: 64 B; per-client streaming buffer: 4 KiB.
* The listener **MUST** accept exactly one client at a time;
  further `accept()`s wait. This keeps the sensor's SD bandwidth
  exclusive to one consumer.

### 6.4 Capture-state sharing

The capture sentant and the `data_tcp` listener run in different
threads. The capture sentant **MUST** publish the
currently-recording filename (or `None`) into a shared handle
that the listener reads on every `GET` / `DEL` / `DEL_ALL`. The
reference implementation uses
`Arc<Mutex<Option<String>>>`; see
`r2_esp::data_tcp::CurrentRecording`.

### 6.4 Port choice

21047 is the first port above the rocker block (21042..21046).
Canonical R2 has not claimed it. See
`audits/2026-05-18-post-v0.1.0-conformance.md` Finding F for the
prior precedent that motivated avoiding the canonical 21042..21045.

---

## 7. Dashboard responsibilities

### 7.1 Forced sync_pulse on Start

Before sending `r2.dash.capture.start` to the fleet, the
dashboard **SHALL** issue one immediate `r2.dash.sync_pulse`
round to every connected peer. The smoothed clock-offset deltas
from the subsequent `r2.sensor.sync_pong` responses flow back to
the sensors via `r2.dash.set_clock_offset` through the existing
Cristian's-algorithm path (per SPEC-R2-WORKSHOP-TIMESYNC §2.3) so
the `ts_ms` values appearing in all sensors' subsequent capture
files share a freshly-tightened baseline.

The dashboard **SHALL** issue `r2.dash.capture.start`
immediately after kicking the sync round — it does **NOT** await
the pongs. Each pong, when it arrives, refines the offset
asynchronously and applies to subsequent samples; the period
between `start` and `mark` (≥ `CAL_WINDOW_MS` ≈ 2000 ms) is
more than enough for the refinement to land in practice.

### 7.2 Filename consistency

The dashboard **SHALL** generate the `ts_ms` value once on
`Start` (not on `Mark`) and pass the same value to every sensor
when sending `r2.dash.capture.mark`. This guarantees the same
filename across the fleet for one capture session.

### 7.3 Operator-plane events and HTTP routes

The capture lifecycle (start / mark / stop) is operator-initiated
and rides as R2-WIRE cmd events on `/r2` per WIRE §2.1; the per-
sensor data export uses HTTP GET helpers mounted by the dashboard
(see SPEC-R2-WORKSHOP-DASHBOARD §5.1).

**Operator-plane cmd events** (viewer → controller on `/r2`,
correlated by `req_id` in CBOR key 0; see WIRE rows 29–31):

| Event | Payload | Effect |
|---|---|---|
| `r2.dash.cmd.capture.start` | `{0: req_id (u32)}` | Controller emits a sync_pulse to align fleet clocks, then fans out row-17 `r2.dash.capture.start` to every connected sensor. Response confirms scheduling. |
| `r2.dash.cmd.capture.mark`  | `{0: req_id, 1: name (text), 2: prefix (text, optional)}` | Controller stamps an authoritative `ts_ms`, derives the canonical filename `<ts16>-<name>.csv` (with optional `prefix`), and fans out row-18 `r2.dash.capture.mark` to every sensor. |
| `r2.dash.cmd.capture.stop`  | `{0: req_id}` | Controller fans out row-19 `r2.dash.capture.stop` to close the active capture file on every sensor. |

**Data-export HTTP routes** (per SPEC-R2-WORKSHOP-DASHBOARD §5.1):

| Route | Method | Body | Purpose |
|---|---|---|---|
| `/api/data/{addr}/list` | GET | — | `data_tcp` `LIST` to one sensor; returns the JSON-mapped CBOR response. |
| `/api/data/{addr}/file/{name}` | GET | — | `data_tcp` `GET`; prepends a `seq,ts_ms,<dev>_x,<dev>_y,<dev>_z\n` header line where `<dev>` is the operator-assigned alias (or IP-with-underscores fallback), then streams the raw fixed-width rows. The Content-Disposition filename becomes `<original-stem>__<dev>.csv`. The on-disk file itself has no header and no device suffix — the dashboard splices both on for the browser download so multi-sensor exports stay distinguishable in a directory listing and when concatenated in pandas. |
| `/api/data/{addr}/file/{name}` | DELETE | — | `data_tcp` `DEL`. |
| `/api/data/{addr}/all` | DELETE | — | `data_tcp` `DEL_ALL`. |
| `/api/data/merged` | GET `?file=<basename>` | — | Wide-format merge of the named capture from every connected sensor. The header is `ts_ms` followed by three columns per sensor (`<ip>_x, <ip>_y, <ip>_z`, IP dots → underscores, sensors in sorted-IP order). One row per unique `ts_ms` across the fleet, ascending. Cells are **blank** where that sensor has no sample at that `ts_ms` — coincident timestamps fill both sensors' columns; offsets-by-jitter (typically 1–3 ms apart in practice) produce single-sensor rows. |

The per-sensor zip route mooted in earlier drafts is deferred —
operators wanting all files from one sensor can iterate `LIST`
then `GET name` per file. The webapp's "Download merged" button
passes the most-recent `<ts16>-<name>.csv` from the current
session as `?file=`.

### 7.4 Auto-sync to controller

Target workflow: *"run experiments, pack and go home without
having to worry about synchronising the data because it is
already synched to the laptop."* The dashboard fetches each
capture file (main + sidecar) from every sensor as soon as it is
finalised on the sensor's SD, with a periodic reconciliation pass
covering anything missed.

**Trigger.** The dashboard **SHALL** maintain per-peer
last-known capture state. When the dashboard observes a peer
transition `Recording → Idle` (`r2.sensor.capture.state` row 17,
state `2 → 0`) it **SHALL** spawn a fetch for the filename that
peer last reported in payload key 1 of row 17. Per
`firmware/.../capture.rs::stop()`, the file is fsync'd and the
`CurrentRecording` lock cleared before that transition is
emitted, so the file is safe to read immediately.

**Storage path.** Captures **SHALL** land under
`$XDG_DATA_HOME/r2-workshop/captures/` (fallback
`~/.local/share/r2-workshop/captures/`). Filename convention
matches the existing single-file download path:
`<stem>__<dev>.csv` where `<stem>` is the sensor-side filename
without extension and `<dev>` is the operator-assigned device
alias (or sanitised IP fallback). Sidecars land as
`<stem>__<dev>.marks.csv` using the same `<dev>` convention.

**CSV-header splicing.** For the main file the dashboard
**SHALL** prepend the `seq,ts_ms,<dev>_x,<dev>_y,<dev>_z\n`
header at write time, identical to the existing
`/api/data/{addr}/file/{name}` GET handler. Sidecars are copied
byte-for-byte from the sensor — the v1 sidecar header (§4.1) is
already self-describing.

**Reconciliation poll.** The dashboard **SHALL** run a
reconciliation pass every 60 s for every connected peer:
`data_tcp` `LIST`, diff against the controller-side index, fetch
anything missing. `ST_BUSY` (the live file) is skipped — the
next pass picks it up after the Recording→Idle transition.

**Sensor SD is the canonical safety net.** The dashboard
**MUST NOT** auto-delete files from the sensor after sync. The
controller-local copy is additive. Operators clear sensor SD
manually via the existing per-file Delete or `DEL_ALL` actions.

**Status event.** After a successful local-write the controller
**SHALL** emit `r2.dash.capture.synced` on `/r2` (see
SPEC-R2-WORKSHOP-WIRE) so every connected viewer can update its
session-row sync badge in real time.

### 7.5 In-session event marks

Operator annotation injected into the active capture window —
distinct from `r2.dash.cmd.capture.mark` which **closes** a file
and opens a new one named with the operator's text. Event marks
**add a row** to a sidecar file without disturbing the active
capture.

**Operator-plane cmd** (viewer → controller on `/r2`, row
to be added in WIRE):
`r2.dash.cmd.capture.event_mark { 0: req_id (u32), 1: label
(text, ≤64 chars) }`. If the label is empty, the controller
**SHALL** substitute `"mark"`.

**Controller behaviour.** On receipt of the cmd the controller:

1. **SHALL** stamp `ts_ms` from the controller clock at the
   moment of receipt — webapp-supplied timestamps are not
   trusted (clock-drift between operator browser and controller
   is real).
2. **SHALL** assign a `mark_id` from a monotonic atomic counter
   that survives the controller process lifetime (resets only on
   controller restart; collisions across restarts are tolerated
   — the `(ts_ms, label)` pair disambiguates downstream).
3. **SHALL** fan out `r2.dash.capture.event_mark { 1: ts_ms,
   2: label, 3: mark_id }` to every connected peer.
4. **SHALL** emit `r2.dash.capture.event_marked { 1: ts_ms,
   2: label, 3: mark_id, 4: session_stem (text) }` on `/r2` so
   every viewer renders the mark immediately — independent of
   whether any specific sensor's sidecar has synced yet.

**Sensor behaviour.** Per §3 row, sensors check
`CurrentRecording`: if `None` (not Recording), the event is
silently ignored. If `Some(stem)`, the sensor appends one row to
`<stem>.marks.csv` per §4.1 and fsyncs.

**Auto-sync.** Sidecars are picked up by the §7.4 sync engine
exactly like main files — same Recording→Idle trigger covers
the final state, and the 60 s reconciliation pass covers any
in-flight marks added after the last sync but before Stop.

---

## 8. Conformance

A firmware build conforms to this spec when ALL of the following
hold:

1. The CaptureMgr (or equivalent) **MUST** implement the three
   states + transitions in §2.
2. The five wire events in §3 **MUST** be present with the
   payload shapes shown.
3. Capture files **MUST** be written to `/sdcard/captures/` (or
   the fallback `/sdcard/cap-*` per §4).
4. The `seq` and `ts_ms` columns in capture rows **MUST** match
   the rolling ring; x, y, z **MUST** be `raw − offset`.
5. The `data_tcp` listener on port 21047 **MUST** implement
   `LIST`, `GET`, `DEL`, `DEL_ALL` per §6.
6. Names violating the `[A-Za-z0-9_-]{1,32}` charset **MUST**
   cause the Mark to be refused per §3.
7. `r2.dash.capture.event_mark` **MUST** be ignored when not in
   `Recording`. When in `Recording`, the sidecar
   `<stem>.marks.csv` **MUST** be created on first event with
   the header from §4.1, each subsequent event **MUST** append
   one RFC-4180-escaped row, and each append **MUST** be
   fsync'd before the next event is accepted.

A dashboard build conforms when:

1. `Start` triggers the sync_pulse round per §7.1.
2. The same `ts_ms` is sent to every sensor on `Mark` per §7.2.
3. All HTTP routes in §7.3 are present and proxy to the
   sensor's `data_tcp` listener as specified.
4. Per-peer `Recording → Idle` transitions **MUST** trigger a
   fetch of the just-finalised file per §7.4, and the resulting
   `r2.dash.capture.synced` event **MUST** be emitted on `/r2`
   after the local-write succeeds.
5. The 60-second reconciliation poll per §7.4 **MUST** be
   running for every connected peer.
6. Sensor-side files **MUST NOT** be auto-deleted after sync.
7. `r2.dash.cmd.capture.event_mark` **MUST** assign a
   controller-stamped `ts_ms` and a monotonic `mark_id`, fan
   out `r2.dash.capture.event_mark` to every connected peer,
   and emit `r2.dash.capture.event_marked` on `/r2` per §7.5.

A webapp build conforms when:

1. The Data tab disables the **Record** button (which emits
   `r2.dash.cmd.capture.mark`) while any peer reports
   `capture_state = 1 (calibrating)` and that peer's
   `cal_start_ms + CAL_WINDOW_MS` has not yet elapsed.
2. Per-card "delete" + fleet-wide "delete all" actions fan out
   via `Promise.allSettled` over the per-sensor DELETE routes.
3. The **Mark** run-control action (which emits
   `r2.dash.cmd.capture.event_mark` per §7.5) is reachable only
   while at least one peer reports `capture_state = 2 (recording)`.

**Run-control button labels.** v0.2 webapp labels rotate one slot
to match the state machine and free "Mark" for the in-session
annotation. Wire-event names are unchanged — this is a UI label
remap only:

| Operator label | Wire event |
|---|---|
| Calibrate | `r2.dash.cmd.capture.start` |
| Record    | `r2.dash.cmd.capture.mark`  |
| Mark      | `r2.dash.cmd.capture.event_mark` |
| Stop      | `r2.dash.cmd.capture.stop`  |

---

## 9. Versioning

| Date       | Ver | Change                                                     |
|------------|-----|------------------------------------------------------------|
| 2026-05-18 | 0.1 | Initial draft.                                             |
| 2026-05-26 | 0.2 | Add §4.1 event-mark sidecar, §7.4 auto-sync to controller, §7.5 in-session event marks. Adds row 5 `r2.dash.capture.event_mark` to §3 and three conformance clauses (firmware #7, dashboard #4–7, webapp #3). |
