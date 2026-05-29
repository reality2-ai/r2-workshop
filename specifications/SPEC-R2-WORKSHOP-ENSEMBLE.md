# SPEC-R2-WORKSHOP-ENSEMBLE: r2-workshop as an R2 Ensemble

**Version:** 0.1 Draft
**Date:** 2026-05-28
**Status:** Normative Draft
**Depends on:**
- **Upstream (canonical):** R2-ENSEMBLE, R2-COMPILE, R2-DEF, R2-CAP
  (all at `r2-specifications/specs/r2-core/`)
- **r2-workshop:** SPEC-R2-WORKSHOP-SENSOR, SPEC-R2-WORKSHOP-DASHBOARD,
  SPEC-R2-WORKSHOP-WIRE, SPEC-R2-WORKSHOP-CAPTURE, SPEC-R2-WORKSHOP-SENTANTS

---

## 1. Introduction

This specification declares **what kind of thing r2-workshop is** in
the canonical R2 vocabulary, and pins the identity of the concrete
deployment that lives in this repo (the rocker-rig tyre-wear sensor
fleet).

**r2-workshop is two things at once:**

1. **A template** for building R2-ENSEMBLE-conformant sensor
   deployments — Rust on ESP32-S3 + ADXL355 (or replaceable IC) +
   web dashboard + signed trust group + automatic data sync to a
   laptop. The shared substrate crates (r2-wire, r2-fnv, r2-trust,
   r2-bootstrap, r2-cbor, r2-esp, r2-bootstrap) cover the protocol +
   transport plumbing; the per-deployment specialisation is the
   choice of sentants, plugin selection, and class string.

2. **The "rocker" deployment** at the University of Auckland —
   monitoring structural shear in actuator joints of a tyre-wear
   test rig. Class string: `nz.ac.auckland.rocker`.

The deployment is delivered as a set of **role-ensembles** (a role is
an ensemble; most hives perform one role): `rocker-sensor`,
`rocker-controller`, `rocker-viewer`, `rocker-keyholder`, sharing the
one `nz.ac.auckland.rocker` class + trust group. Each has its own
R2-DEF §7 score under `ensemble/`. See SPEC-R2-WORKSHOP-SENTANTS for the
role catalog and the substrate-vs-deployment boundary. Future
deployments (people-counter, pet-gait, etc.) reuse the Controller +
KeyHolder + Viewer-sentant substrate and ship only a new Sensor
role-ensemble + a Viewer UI skin + a new class string.

This spec defines:

* The ensemble identity (name, class, version) for the rocker
  deployment.
* The class-namespace policy — institutional reverse-DNS by
  default; `ai.reality2.ensemble.*` reserved for Reality2's own
  forks of this template.
* The relationship to the canonical R2-ENSEMBLE concept upstream.
* Where the scores live (`ensemble/{sensor,controller,viewer,keyholder}.yaml`,
  one per role-ensemble) and what they document.
* The roadmap from the current pre-loader pattern to the
  R2-COMPILE-driven world.

### 1.1 Scope

In scope:

* r2-workshop's identity as an R2 ensemble in the canonical sense.
* The rocker deployment's class string + version policy.
* Class-namespace policy for ensembles built from this template.
* The (class, carrier) OTA-matching tuple as the ensemble class
  carried by every sensor + dashboard binary at compile time.

Out of scope:

* The R2-ENSEMBLE concept itself — see upstream `R2-ENSEMBLE.md`.
* Score schema details — see upstream `R2-DEF.md` §7.
* Compilation pipeline for sentant YAML → firmware — see upstream
  `R2-COMPILE.md`.
* Specific sentant + plugin enumeration — see
  `SPEC-R2-WORKSHOP-SENTANTS.md` and the per-role scores under
  `ensemble/`.

### 1.2 Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**,
**SHOULD**, **MAY** are interpreted per RFC 2119.

Defined upstream by R2-ENSEMBLE §1.4:

* **Ensemble** — composite distributed unit of user-meaningful
  functionality (sentants + plugins + UI registrations).
* **Part** — a single component of an ensemble (one sentant, one
  plugin, or one UI registration).
* **Performer** — a hive currently hosting one or more parts.
* **Score** — declarative YAML description of the parts and their
  relationships (R2-DEF §7).

Defined here:

* **Template** — a repository that ships a substrate + reference
  ensemble that other operators may fork and re-class to build their
  own deployment. r2-workshop is one.
* **Deployment** — a concrete instantiation of an ensemble at a
  specific site, with a specific class string. The rocker rig at the
  University of Auckland is one such deployment of r2-workshop.

---

## 2. Ensemble identity

### 2.1 The rocker deployment

| Field | Value |
|---|---|
| **name** | `rocker` |
| **class** | `nz.ac.auckland.rocker` |
| **class hash (FNV-1a-32)** | `0x624c47bc` |
| **ensemble_version** | `0.1` (per R2-DEF §7 schema) |
| **version** (the ensemble's own semver) | tracked at each role score's `ensemble.version` (the four roles version together) |

The class string is the canonical R2-ENSEMBLE class per R2-ENSEMBLE
§2.2 and the value emitted on every `r2.sensor.announce` per
SPEC-R2-WORKSHOP-WIRE §3.1 key 11. Same string, same hash,
everywhere.

### 2.2 Class-namespace policy

The class string follows the upstream R2-CAP §3 convention — a
reverse-DNS string identifying the **ensemble**, not a component
type. Namespace ownership:

| Namespace prefix | Reserved for |
|---|---|
| `ai.reality2.ensemble.*` | Reality2's own ensemble forks of this template (the team that originated R2). Example upstream: `ai.reality2.ensemble.notekeeper` (R2-ENSEMBLE §2.3). |
| `<organisation reverse-DNS>` | Operators forking this template for their own deployments. The rocker deployment uses `nz.ac.auckland.rocker`; a future people-counter ensemble at the same institution would be `nz.ac.auckland.people-counter`. |

There is no central registry. Operators pick a namespace they
control (per the IETF reverse-DNS convention) and the resulting FNV
hash gates protocol traffic between deployments — two ensembles
with different class strings cannot interpret each other's frames
even if they share radio/network space (their event hashes don't
agree).

The trailing leaf names the **ensemble** in flat form (matching
notekeeper). Operators MAY use a deeper hierarchy
(`<org>.<group>.<deployment>`) but the leaf segment SHOULD identify
the deployment singularly — avoid trailing component-type suffixes
like `.sensor`, `.firmware`, or `.dashboard` since the class names
the ensemble as a whole, not one of its parts.

### 2.3 Class compilation

The class string is baked into firmware AND dashboard at compile
time from `trust_keys/sensor_class.txt`. Both binaries derive
their FNV event-hash table from this same string (R2-FNV +
SPEC-R2-WORKSHOP-WIRE), so traffic between them is class-gated by
construction — no runtime check, no mis-class catastrophe at
runtime.

A class-string change requires:

1. Updating `trust_keys/sensor_class.txt` to the new string.
2. Re-compiling the dashboard and every per-carrier firmware
   variant.
3. Re-flashing every sensor (cannot OTA across a class boundary
   because the post-flash binary won't recognise the dashboard's
   announce-frame hash).
4. Re-pairing every viewer (the TG's class hash changes; viewer
   certs minted before the change won't validate post-change).

This is the "class rotation" procedure documented as feedback
memory `feedback_release_build_order` and exercised once already
during the r2-rocker → r2-workshop rename. Treat it as a
wire-breaking schema migration; never do it mid-experiment.

---

## 3. Composition

### 3.1 Parts inventory — by role-ensemble

The parts are enumerated normatively across **four R2-DEF §7 scores**
under `ensemble/` (one per role) and described narratively in
**`SPEC-R2-WORKSHOP-SENTANTS.md`**. This section is informative — a
top-level map. A role *is* an ensemble; a hive performs one role.

| Role-ensemble (score) | Sentants | Notable plugins / registrations | Tier |
|---|---|---|---|
| **`sensor.yaml`** | `Accelerometer` (domain) + substrate (`Identity`, `WifiProv`, `Bootstrap`, `Beacon`, `Battery`, `Status`, `Sync`, `Recorder`, `Uplink`, `Ota`, `Reset`, `Health`, `Capture`, `Presence`) | `adxl355` (sensing — swap point), `sd-card`, `data-tcp`, `ota-tcp`, …; `r2-ble` advertise | **deployment-specific** |
| **`controller.yaml`** | `Fleet`, `Capture`, `Sync`, `TimeSync`, `Bootstrap`, `OTA`, `Reset`, `Identify` | `captures-store`, `sd-relay`, `github-firmware-cache`, `ble-scan`; **R2-WEB** + `r2-ble` registrations | substrate (100%) |
| **`viewer.yaml`** | `Viewer` (substrate) | R2-WEB UI registration (bundle = deployment skin) | substrate + skin |
| **`keyholder.yaml`** | `Access` | `tg-signer` (TG private key, credential store) | substrate |

The sensing element is bound by **capability**
(`ai.reality2.cap.accel.triaxial`), not by chip, so it is a swap point
(R2-PLUGIN §10; SPEC-R2-WORKSHOP-SENTANTS §3.3). Raw transports (WiFi,
BLE radio, TCP, relay) are hive-shared singletons, never ensemble
plugins (R2-ENSEMBLE §2.1.2).

### 3.2 What the dashboard actually is

Per R2-ENSEMBLE §2.1.1 *"User interfaces are a special class of
plugin"*, the dashboard is the rocker ensemble's **Web-UI
registration** with the hive's R2-WEB singleton. Today's
monolithic dashboard Rust binary is the pre-R2-loader-runtime
approximation of that registration; once the BEAM/Rust ensemble
loader lands, the registration will be a declarative entry in the
score that an R2-WEB plugin instance consumes (notekeeper's B3
target, per `r2-notekeeper/ensemble/README.md`).

In the meantime, the dashboard binary serves the same protocol
surface a future loaded ensemble would: HTTP routes, `/r2`
WebSocket, /r2 → cmd dispatch, status broadcasts.

---

## 4. Roadmap

The current state of r2-workshop is "B0" in notekeeper's lifecycle
(R2-ENSEMBLE §5 + `r2-notekeeper/ensemble/README.md`): the
ensemble score is drafted and the runtime is a hand-coded
approximation of what the score describes. The phases ahead:

* **B0** *(this spec)* — ensemble score schema defined; rocker
  scores drafted as the four per-role files under `ensemble/`
  (`sensor`, `controller`, `viewer`, `keyholder`). Class string
  fixed at `nz.ac.auckland.rocker`.
* **B1** — substrate crates (r2-dispatch / r2-loader equivalent)
  landed in the workspace, accepting the score's automation
  format.
* **B2** — sensor firmware migrates from hand-coded Rust to
  `r2-compile build --target <carrier> --definition sensor.yaml`
  output (per R2-COMPILE §7); the choose-board/plugins/sentants
  picker becomes real (SPEC-R2-WORKSHOP-SENTANTS §10). Controller
  remains hand-coded for now — its Linux tier supports a runtime
  loader.
* **B3** — controller migrates to the loader. R2-WEB registration
  becomes the operative path; the Rust binary becomes a thin
  loader that consumes `controller.yaml` at startup.
* **B4** — sibling ensembles (people-counter, pet-gait, …) become
  fork repos OR live alongside in a mono-repo, each with its own
  `ensemble.yaml` + class string. r2-workshop's role shifts from
  "the rocker ensemble" to "the template + a worked example".
* **B5** *(parallel)* — r2-workshop github.io project landing
  page listing all known ensembles built on this template, with
  per-ensemble download / fork / quickstart links.

The (class, carrier) work already landed (SPEC-R2-WORKSHOP-WIRE
§3.1 keys 11+12; tasks #88-91 for code) carries forward unchanged
through every phase — it's the protocol-level identity that
distinguishes ensembles regardless of which runtime hosts them.

---

## 5. Conformance

A repository conforms to "r2-workshop ensemble template" semantics
when:

1. The role-ensemble scores live at
   `ensemble/{sensor,controller,viewer,keyholder}.yaml` and each
   validates against the R2-DEF §7 schema (until the loader lands,
   schema validation is manual). A role is an ensemble; a hive
   performs one role (SPEC-R2-WORKSHOP-SENTANTS §1).
2. Every score's `class` matches the contents of
   `trust_keys/sensor_class.txt` byte-for-byte (the four roles share
   the one deployment class).
3. Every binary the repo builds (sensor firmware × N carriers +
   dashboard) bakes that same class string at compile time, and
   emits it on every `r2.sensor.announce` (firmware) /
   participates in the same FNV event-hash table (dashboard).
4. The class string follows §2.2 namespace policy.
5. The carrier slug for each per-carrier firmware variant is
   declared in `Cargo.toml`'s `[package.metadata.r2-workshop]
   carrier = "…"` field per SPEC-R2-WORKSHOP-SENSOR §3.3.1.

---

## 6. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-28 | 0.1 | Initial draft. r2-workshop declared as both template + the rocker deployment. Rocker class set to `nz.ac.auckland.rocker` (FNV 0x624c47bc). Class-namespace policy: institutional reverse-DNS by default; `ai.reality2.ensemble.*` reserved for Reality2's own forks. Roadmap B0-B5 mapped to notekeeper's lifecycle pattern. |
| 2026-05-29 | 0.2 | **Role-ensemble model.** A role is an ensemble; the single `ensemble/ensemble.yaml` split into four per-role scores `{sensor,controller,viewer,keyholder}.yaml` sharing the one class. §1 + §3.1 reframed by role; §5 conformance points to the four scores. Substrate (Controller/KeyHolder/Viewer-sentant) vs deployment (Sensor + Viewer skin) boundary; sensing bound by capability (swap lever); choose→compile→flash build flow (SPEC-R2-WORKSHOP-SENTANTS §10). |
