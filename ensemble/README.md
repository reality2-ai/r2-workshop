# r2-workshop — rocker ensemble scores

R2-ENSEMBLE / R2-DEF §7 scores for the **rocker** deployment of the
r2-workshop template.

## A role is an ensemble

r2-workshop is **not** one ensemble whose parts are cherry-picked per
hive. It is a small set of **role-ensembles** that share one event
vocabulary + one trust group (the `nz.ac.auckland.rocker` class). Most
hives perform a **single role** — and a hive loads the score for its
role. The roles interoperate because R2 event hashes derive from the
event *name* (R2-FNV), independent of class.

| Score | Role | Runs on | Tier |
|---|---|---|---|
| [`sensor.yaml`](sensor.yaml) | **Sensor** | each ESP32 rig device | **deployment-specific** (the swap points) |
| [`controller.yaml`](controller.yaml) | **Controller** | the fixed coordinator laptop | framework substrate (100%) |
| [`viewer.yaml`](viewer.yaml) | **Viewer** | the browser (WASM hive) | substrate sentant + deployment skin |
| [`keyholder.yaml`](keyholder.yaml) | **KeyHolder** | usually co-loaded with Controller; separable | framework substrate |

## Substrate vs deployment — the abstraction boundary

The point of splitting by role is that **building a new application is a
small, bounded diff**: ship a new **Sensor** ensemble (a new sensing
plugin + domain sentant) and a new **Viewer skin**, and reuse the
**Controller** and **KeyHolder** ensembles unchanged.

The boundary is visible in the class namespace inside each score:

- `ai.reality2.workshop.*` — framework substrate, reused unchanged.
- `nz.ac.auckland.rocker.*` — rocker-specific (the swap points).

The sensing element is bound by **capability**
(`ai.reality2.cap.accel.triaxial`), not by chip — so the candidate
sensors from the 2026-05 shipment (LIS2DW12 / LIS2DH / ADXL345; see
`../docs/datasheets/README.md`) are drop-in plugin swaps with no sentant
change. DFR1117 (ESP32-C6) would be a new `compile_target` / carrier.

## How the four scores relate at runtime

- **Controller** hosts the one R2-WEB singleton and serves the
  **Viewer**'s UI bundle (R2-WEB §8.5 hybrid: controller = gateway), or
  GitHub Pages serves it off-network. The browser boots the **Viewer**
  WASM hive and talks to the Controller's sentants over `/r2`.
- **Sensor** ↔ **Controller** is the production-TG event traffic
  (`r2.sensor.*` / `r2.dash.*`).
- **Controller** ↔ **Viewer** is bilateral entanglement between the
  production and viewing trust groups (relay-forwarded off-network).
- **KeyHolder** mints the certs + signs `#wifi_offer`; the Controller's
  Bootstrap binds its `tg-signer` locally (co-loaded) or remotely
  (separate) via trust-group plugin routing (R2-PLUGIN §10).

## Building your own deployment from this template

1. Fork the repo.
2. Set `trust_keys/sensor_class.txt` to your class (e.g.
   `nz.ac.auckland.people-counter`; namespace policy in
   `SPEC-R2-WORKSHOP-ENSEMBLE.md` §2.2). Every score's `class:` + the
   baked firmware/dashboard class follow from this one file.
3. Author a new **`sensor.yaml`**: a sensing plugin providing
   `ai.reality2.cap.accel.*` (or your domain capability) + a domain
   sentant under `nz.<your-org>.<deployment>.*`.
4. Skin **`viewer.yaml`**'s `static_bundle` for your domain.
5. Reuse **`controller.yaml`** and **`keyholder.yaml`** unchanged.

Until the ensemble loader lands (B2/B3) this is a per-fork edit + a
rebuild; afterwards the runtime consumes the scores directly.

## Relationship to the current runtime (B0 pattern)

The Rust dashboard (`../dashboard/`) and the per-carrier firmware
(`../firmware/esp32-s3/{devkitc,xiao}/`) are the **hand-coded** form of
what these scores describe — notekeeper's "B0" pattern (R2-ENSEMBLE §5,
SPEC-R2-WORKSHOP-ENSEMBLE §4):

- **B0** *(current)* — scores written, runtime hand-implemented.
- **B1** — r2-loader / r2-dispatch crate lands.
- **B2** — Sensor firmware moves to `r2-compile build` against
  `sensor.yaml` (R2-COMPILE §7).
- **B3** — Controller moves to the loader; scores replace the
  hand-coded Rust dispatch.

Until B2/B3 these files are documentation in declarative form. The
human-readable companion is
`../specifications/SPEC-R2-WORKSHOP-SENTANTS.md`; per-event wire detail
is in `SPEC-R2-WORKSHOP-WIRE.md`.
