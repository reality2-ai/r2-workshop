# SPEC-R2-WORKSHOP-SENTANTS: Role-ensemble, sentant + plugin catalog

**Version:** 0.3 Draft
**Date:** 2026-05-29
**Status:** Normative Draft
**Depends on:** SPEC-R2-WORKSHOP-ENSEMBLE, SPEC-R2-WORKSHOP-SENSOR, SPEC-R2-WORKSHOP-DASHBOARD, SPEC-R2-WORKSHOP-WIRE, SPEC-R2-WORKSHOP-CAPTURE, SPEC-R2-WORKSHOP-TIMESYNC, SPEC-R2-WORKSHOP-ACCESS, canonical **R2-ENSEMBLE / R2-SENTANT / R2-PLUGIN / R2-CAP / R2-DEF / R2-WEB / R2-COMPILE**
**Scores (normative):** `ensemble/{sensor,controller,viewer,keyholder}.yaml`

---

## 1. Introduction

r2-workshop is delivered as a small set of **role-ensembles**. This
spec is the human-readable companion to the four R2-DEF §7 scores under
`ensemble/`; the scores are the normative part inventory, this document
explains the model, the contracts, and the rules that make the parts
composable and swappable.

### 1.1 A role is an ensemble

The earlier framing of "two hive classes (sensor / dashboard)" is
retired. Canonically (R2-ENSEMBLE), r2-workshop is **not** one ensemble
whose parts are cherry-picked per hive. It is a set of **role-ensembles**
that share one event vocabulary and one trust group (the
`nz.ac.auckland.rocker` class). **A role is an ensemble** — a composite
of many sentants + plugins — and **most hives perform a single role**:
a hive loads the score for its role. They interoperate because R2 event
hashes derive from the event *name* (R2-FNV), independent of class.

| Role | Score | Runs on | Trust group | `compile_target` | Tier |
|---|---|---|---|---|---|
| **Sensor** | `sensor.yaml` | each ESP32 rig device | production | `esp32-s3` (cand. `esp32-c6`) | **deployment-specific** |
| **Controller** | `controller.yaml` | the fixed coordinator laptop | production | `linux` | substrate (100%) |
| **Viewer** | `viewer.yaml` | the browser (WASM hive) | viewing (entangled) | `wasm` | substrate + UI skin |
| **KeyHolder** | `keyholder.yaml` | usually co-loaded w/ Controller; separable | production (owner) | `linux` | substrate |

### 1.2 The end-state: choose → compile → flash

Where this is heading: setting up R2 on a board should be **choose the
board → choose the plugins → choose the sentants → compile → flash**
(§10). The role-ensemble scores *are* that manifest — a score's
`compile_target` is the board, its `plugins:` are the chosen plugins,
its `sentants:` are the chosen sentants. Binding hardware by
**capability** rather than by part number (§4.3) is what turns "choose
the plugins" into a real menu. Today (B0) the firmware/dashboard
hand-implement the chosen score; R2-COMPILE (B2) will consume it
directly.

### 1.3 Substrate vs deployment — the abstraction boundary

The reason to split by role is that **building a sibling deployment is a
small, bounded diff**: ship a new **Sensor** ensemble (a sensing plugin
+ a domain sentant) and a **Viewer skin**; reuse **Controller** and
**KeyHolder** unchanged. The boundary is encoded in the class namespace:

| Namespace | Meaning |
|---|---|
| `ai.reality2.workshop.*` | framework **substrate** — reused unchanged by every sibling deployment |
| `nz.ac.auckland.rocker.*` | **deployment-specific** — the swap points (the rocker domain) |

### 1.4 Scope

In scope: the role-ensemble model; the sentant + plugin contracts as
r2-workshop applies them; the surface-ownership ("anti-short-circuit")
rule; per-role part summaries; the choose→compile→flash flow; per-role
conformance.

Out of scope: the canonical models themselves (see the upstream specs);
the full per-part inventory (that is the four scores); per-event wire
detail (SPEC-R2-WORKSHOP-WIRE).

### 1.5 Terminology

RFC 2119 key words apply when capitalised. Canonical terms (R2-ENSEMBLE
§1.4, R2-SENTANT §1.1, R2-PLUGIN §1, R2-CAP §1.1): **ensemble, part,
performer, score, sentant, plugin, capability, class, hive**. Defined
here:

* **Role-ensemble** — an ensemble that corresponds to one role a hive
  performs (Sensor / Controller / Viewer / KeyHolder). A hive performing
  that role loads that ensemble's score.
* **Substrate part** — a sentant or plugin reused unchanged across
  deployments (`ai.reality2.workshop.*`).
* **Deployment part** — a sentant or plugin specific to this deployment
  (`nz.ac.auckland.rocker.*`); the swap point for siblings.

---

## 2. The Sentant contract (R2-SENTANT, as applied)

A sentant is an IPUCOD agent: event-driven FSM(s) + vars, identified by
a reverse-DNS **class**, declaring its **public events** (its contract),
its **storage** level, and its **plugin bindings**. Sentants do not
touch hardware or transports directly — they go through plugins (§4).
Everything cross-sentant and cross-hive is an event (R2-SENTANT §1.2).

For r2-workshop every sentant declares, in its score entry:

| Field | Meaning |
|---|---|
| `class` | reverse-DNS; namespace marks substrate vs deployment (§1.3) |
| `storage` | `volatile` / `durable` / `durable-state` (R2-SENTANT §2.2.1) |
| `plugins` | bindings — by `name` or, for swappable hardware, by `capability` (§4.3) |
| (automation comment) | the events it subscribes to / emits; authoritative set in the ensemble `capabilities:` block |

### 2.1 Minimal `Sentant` surface (firmware, pre-loader)

Until the loader lands, sensor-side sentants are realised as Rust
structs implementing this surface; the runtime is whatever `main()`
builds. (Unchanged from v0.2.)

```rust
pub trait Sentant {
    fn name(&self) -> &'static str;
    fn subscribed_events(&self) -> &'static [u32] { &[] }   // FNV-1a-32, compile-time
    fn boot(&mut self, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }
    fn tick(&mut self, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }
    fn on_event(&mut self, _ev: &Event, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }
}
```

`HiveCtx` exposes references to the role's plugins and the local event
bus. A sentant publishes via `ctx.emit(hash, payload)`; the runtime
delivers in-process to other subscribed sentants and, via the `uplink`
sentant, onto the wire. Sentants MUST NOT access peripherals directly —
only through a plugin.

---

## 3. The Plugin contract (R2-PLUGIN, as applied)

Per R2-PLUGIN §2 a plugin is anything that runs on a hive and provides
capabilities; it interacts **only through events / typed method calls**,
never plugin-to-plugin, and is **hive-local always** (R2-PLUGIN §5). For
r2-workshop a plugin is "well-defined" when its score entry declares:

| Field | Meaning (R2-PLUGIN §2 / §12.3) |
|---|---|
| `name` | unique within the role-ensemble |
| `capabilities.provides` | the R2-CAP class(es) it offers (reverse-DNS) |
| `capabilities.requires` | host capabilities it needs (buses, fs, net, radios) |
| `events.handled` / `events.emitted` | its inbound / outbound event interface |
| `compile_target` | tiers it builds for (R2-COMPILE) |
| `credentials` | named secrets from the credential store (never in-repo) |

### 3.1 Transports are hive singletons, not ensemble plugins

Raw transports — WiFi, the BLE radio, TCP sockets, the relay — are
**hive-shared singletons** owned by the hive (R2-ENSEMBLE §2.1.2) and
are **never** listed as ensemble plugins. A plugin that speaks a
*protocol* over a transport (e.g. `data-tcp`, `ota-tcp`, the
`ble-beacon` advertiser) is an ensemble plugin that `requires:` the
transport capability (`r2.net.tcp`, `r2.hw.ble`, …).

### 3.2 User interfaces are plugins; the web UI is a registration

Per R2-ENSEMBLE §2.1.1 a UI is a special class of plugin, and per
R2-PLUGIN §13 the web UI is served by the hive's **R2-WEB singleton**
into which an ensemble **registers** a bundle + WS channels. R2-WEB's
server role is *exhausted by serving the bundle and forwarding `/r2`
frames to a sentant* — it hosts no event handlers and no REST. See §6.

### 3.3 Bind hardware by capability, not by part number — THE swap lever

A sentant that needs hardware binds the **capability**, not the chip.
The rocker `Accelerometer` sentant binds `ai.reality2.cap.accel.triaxial`;
the `adxl355` plugin provides it. Any plugin providing the same
capability is a drop-in replacement with **no sentant change** — this is
R2-PLUGIN §10 (a sentant references a plugin by capability; placement /
implementation is a trust-group / build concern).

This is the concrete lever for the 2026-05 hardware shipment: LIS2DW12
(SEN0405), LIS2DH (SEN0224), and ADXL345 (SEN0140) would each be a
plugin providing `ai.reality2.cap.accel.triaxial`; choosing one is a
build-time plugin selection (§10), not a code change.
(BMA220/SEN0168 is 6-bit — likely too coarse; see
`docs/datasheets/README.md`.)

---

## 4. The Sensor role-ensemble (`sensor.yaml`)

The **deployment-specific** role: one per ESP32 rig device, compiled
ahead-of-time into firmware (R2-COMPILE). Authoritative inventory is
`ensemble/sensor.yaml`; summary:

**Deployment-specific parts** (the swap points):

| Part | Kind | Role |
|---|---|---|
| `Accelerometer` (`nz.ac.auckland.rocker.accelerometer`) | sentant | reads triaxial accel via the bound capability, calibrates, stamps, emits `r2.sensor.acceleration`; sim-fallback on chip failure |
| `adxl355` | plugin | provides `ai.reality2.cap.accel.triaxial` over SPI — **the swap point** (§3.3) |

**Substrate parts** (`ai.reality2.workshop.sensor.*`): `Identity`,
`WifiProv`, `Bootstrap`, `Beacon`, `Battery`, `Status`, `Sync`,
`Recorder`, `Uplink`, `Ota`, `Reset`, `Health`, `Capture`, `Presence`;
plugins `sd-card`, `battery-adc`, `led`, `nvs`, `clock`, `data-tcp`,
`ota-tcp`, `reset-tcp`, `log-tcp`, `ble-beacon`, `ble-l2cap`. The
`Capture` FSM and `Health` heuristic are substrate, with
domain-parameterised detail (calibration content, stuck-detection).

Boot order and the per-sentant FSM detail are in SPEC-R2-WORKSHOP-SENSOR
§4 (unchanged); the score lists every plugin's capability + event
interface.

---

## 5. The Controller role-ensemble (`controller.yaml`)

**100% framework substrate** — the fixed per-experiment coordinator
laptop; reused unchanged by every sibling deployment. Authoritative
inventory is `ensemble/controller.yaml`; summary
(`ai.reality2.workshop.*`):

| Sentant | Role |
|---|---|
| `Fleet` | peer metadata + aliases + online/stale state; replays cached announce to late viewers; **owns hive identity** → emits `r2.dash.hive.announce` (§9) |
| `Capture` | fleet-wide Calibrate→Record→Stop FSM + `mark_id` |
| `Sync` | captures-store + auto-sync engine (Recording→Idle trigger, 60 s recon) |
| `TimeSync` | Cristian's-algorithm `sync_pulse`/`set_clock_offset` |
| `Bootstrap` | BLE scan + L2CAP `#wifi_offer`; binds `tg-signer` local-or-remote (§7) |
| `OTA` | firmware push + progress; `github-firmware-cache` |
| `Reset` | remote soft reset |
| `Identify` | identify-LED toggle |

Plugins: `captures-store`, `sd-relay`, `github-firmware-cache`,
`ble-scan`. Registrations: `r2-web` (hosts the singleton; serves the
Viewer bundle — §6), `r2-ble` (scan subscription).

---

## 6. The Viewer role-ensemble (`viewer.yaml`)

The **browser WASM hive**. Per R2-WEB §1.1 a browser is a real R2
device; per §8.4/§8.5 it runs the R2 stack as a WASM hive. The running
dashboard a user sees **is this browser hive** rendering from the `/r2`
event stream — the controller is ~a headless event source + bundle/blob
server.

| Part | Tier | Role |
|---|---|---|
| `Viewer` (`ai.reality2.workshop.viewer`) | substrate | mirrors the event stream into UI-facing state; emits operator-plane commands; holds no authority |
| UI bundle (`../webapp/`) | **deployment skin** | the rocker charts/sessions/live view — the deployment-specific part |

**Where the bundle is served (the one cross-role reference):** the
bundle is *authored by* the Viewer ensemble but *served by* the
Controller's R2-WEB singleton on the LAN (R2-WEB §8.5 hybrid: controller
= gateway), or by GitHub Pages off-network. Either way the browser loads
the bundle, boots the Viewer WASM hive, and talks to the Controller's
sentants over `/r2` (LAN) or the relay (off-network — the relay forwards
all `/r2` frames, so the experience is identical). A sibling deployment
reuses the `Viewer` sentant and ships a different bundle skin.

---

## 7. The KeyHolder role-ensemble (`keyholder.yaml`)

Holds the trust-group private key and is the sole authority that mints
DeviceCertificates, signs `#wifi_offer`, and approves/revokes viewer
access. **Usually co-loaded with the Controller**, but a separate
ensemble because it is *separable* onto a more-trusted hive.

| Part | Role |
|---|---|
| `Access` (`ai.reality2.workshop.access`) | enrolment + viewer access (ACCESS v0.3) |
| `tg-signer` plugin | wraps the Ed25519 TG keypair; `credentials: [tg_priv]` from the credential store — **never in-repo** (SECRETS-POLICY.md) |

**Separability (R2-PLUGIN §10):** the Controller's `Bootstrap` sentant
binds `tg-signer` by name. Co-loaded → resolves locally; separate → the
signing invocation routes to the KeyHolder hive as a trust-group plugin
call and the signature routes back. The `Bootstrap` sentant is identical
either way; plugin placement is a trust-group concern.

---

## 8. Surfaces and the anti-short-circuit rule

A **plugin may expose a surface** (an HTTP route, a TCP port, a WS
channel) — but a surface MUST NOT be a back door around the sentant
model. The governing rule:

> **Sentant-to-sentant exchange travels as `/r2` events.** A surface is
> a legitimate plugin surface only if it serves (a) static assets, (b) a
> bulk binary blob, or (c) a local binary diagnostic. All **bounded
> structured state** and all **operator commands** are `/r2` events
> owned by a sentant (cached + replayed on connect). A surface that
> serves or mutates hive state with **no sentant behind it** is a
> short-circuit and is **non-conformant**.

The deciding question is the **consumer**: if a *sentant* needs the data
to drive behaviour → `/r2` event; if the consumer is page-bootstrap
chrome, an operator with curl, or a plugin's own served content → a
plugin surface is fine.

### 8.1 Surface → Sentant → Plugin → Capability (Controller / R2-WEB)

| Surface (R2-WEB plugin) | Owning sentant | Verdict |
|---|---|---|
| `/` static bundle | — | pure plugin (assets) |
| `/r2` WebSocket | the hive bus (all sentants) | the event channel itself |
| `/api/version` | — | local diagnostic, no hive state |
| `/api/data/.../file`, `/zip`, `/merged`, `/api/firmware/{carrier}/binary` | `Sync` / `OTA` (via plugins) | **bulk blob** → HTTP surface (justified exception) |
| hive identity, fleet/aliases, firmware-available, capture state, marks, all commands | `Fleet` / `Capture` / `Sync` / `OTA` | **state/commands → `/r2` events**, never a route |

`r2-workshop` extends the canonical R2-WEB payload with a `blob_routes:`
list (in `controller.yaml`) for case (b) — the only non-`/r2`, non-asset
HTTP surface. Identity (`r2.dash.hive.announce`, owned by `Fleet`) is the
worked example: the `/api/ensemble` route, if kept, is a read-through to
the sentant, never a const-reading orphan.

---

## 9. Hive identity as a worked example of the rule

The dashboard's R2-ENSEMBLE identity (name/class/class-hash/version) is
hive **state**, so by §8 it is owned by a sentant (`Fleet`) and
announced on `/r2` as `r2.dash.hive.announce`, cached + replayed to
late-joining viewers exactly like `r2.sensor.announce`. The viewer's
footer renders it from that event — which is why it works identically on
the LAN and over the relay (off-network), where no `/api/*` route is
reachable. A read-only `/api/ensemble` convenience view MAY exist, but
the event is the source of truth. (Tracked: WIRE row for
`r2.dash.hive.announce`; Fleet emit + replay; viewer consume.)

---

## 10. Choose → compile → flash

The target build flow (R2-COMPILE): **choose the board → choose the
plugins → choose the sentants → compile → flash.** The role-ensemble
scores are exactly that manifest:

| Choice | Where it lives in the score |
|---|---|
| **board** | `compile_target` (`esp32-s3`, `esp32-c6`, `linux`, `wasm`) |
| **plugins** | `plugins:` — incl. the sensing element, selected by the capability it must provide (§3.3) |
| **sentants** | `sentants:` — substrate set + the deployment domain sentant |

A new sensor build is then: pick a carrier (`esp32-s3` / the new
`esp32-c6` DFR1117), pick the sensing plugin that provides
`ai.reality2.cap.accel.triaxial` (adxl355 / lis2dw12 / lis2dh / adxl345),
keep the substrate sentants, `r2-compile build --target <carrier>
--definition sensor.yaml`, flash. No sentant edits.

Maturity (R2-ENSEMBLE §5 / SPEC-R2-WORKSHOP-ENSEMBLE §4):

* **B0** *(now)* — scores written; firmware/dashboard hand-implement the
  chosen score.
* **B1** — loader / dispatch crate consumes the score's automation form.
* **B2** — sensor firmware built via `r2-compile` from `sensor.yaml`;
  the picker (choose board/plugins/sentants) becomes real.
* **B3** — controller moves to the loader; scores replace hand-coded
  dispatch.

---

## 11. Conformance

A **role-ensemble score** conforms when:

1. It validates against R2-DEF §7 and declares `name`, `class`
   (= `nz.ac.auckland.rocker`, byte-for-byte with
   `trust_keys/sensor_class.txt`), `version`, `ensemble_version`,
   `compile_target`, `trust_group.roles_allowed`.
2. Every sentant declares `class` (correctly namespaced per §1.3) and
   `storage`; every plugin declares `capabilities.{provides,requires}`
   and `events.{handled,emitted}` (§3).
3. Hardware-bearing sentants bind by **capability** where a swap is
   intended (§3.3), not by chip name.
4. No raw transport is listed as an ensemble plugin (§3.1); UIs are
   R2-WEB registrations (§3.2).
5. **Every surface maps to an owning sentant** except pure
   assets/blobs/diagnostics (§8). No state-or-command surface lacks a
   sentant behind it.

A **build** (firmware or dashboard) conforms when its hand-coded
(B0) or compiled (B2+) form realises exactly the chosen role-ensemble
score's sentant + plugin set and event interface, and bakes the class
string at compile time.

---

## 12. Versioning

| Date | Ver | Change |
|------|-----|--------|
| 2026-05-18 | 0.1 | Initial draft — firmware modules framed as sentants + plugins. |
| 2026-05-28 | 0.2 | Added §4 dashboard-hive sentants + plugins; reframed to cover both hive classes; cross-ref SPEC-R2-WORKSHOP-ENSEMBLE + the single `ensemble/ensemble.yaml`. |
| 2026-05-29 | 0.3 | **Restructured around role-ensembles.** A role is an ensemble; `ensemble.yaml` split into per-role scores `{sensor,controller,viewer,keyholder}.yaml`. Added the R2-PLUGIN plugin contract (capabilities + event interface), bind-by-capability swap lever (§3.3), transports-as-singletons (§3.1), web-UI-as-registration (§3.2), the anti-short-circuit surface rule + Surface→Sentant→Plugin→Capability map (§8), hive-identity worked example (§9), and the choose→compile→flash build flow (§10). Substrate-vs-deployment boundary encoded in the class namespace. |
