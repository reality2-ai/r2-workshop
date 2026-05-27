# r2-workshop — rocker ensemble

This directory holds the R2-ENSEMBLE score for the rocker
deployment of r2-workshop. Pattern is borrowed directly from
`r2-notekeeper/ensemble/README.md`.

## Files

- `ensemble.yaml` — the normative ensemble score (R2-DEF §7 schema).
  Declares identity (`name: rocker`, `class: nz.ac.auckland.rocker`,
  `version: 0.2.0`), the 7 sentants, ensemble-owned plugins, and
  the single R2-WEB singleton registration.
- (this README)

## Relationship to the current runtime

The Rust dashboard (`dashboard/`) and the per-carrier firmware
(`firmware/esp32-s3/devkitc/`, `firmware/esp32-s3/xiao/`) are the
**hand-coded** form of what the score above describes. This is
notekeeper's "B0" pattern (R2-ENSEMBLE §5, repeated in
SPEC-R2-WORKSHOP-ENSEMBLE §4):

* B0 *(current)* — Score written, runtime hand-implemented.
* B1 — r2-loader / r2-dispatch crate landed.
* B2 — Sensor firmware moves to `r2-compile build` against this
  YAML (R2-COMPILE §7).
* B3 — Dashboard moves to the loader; the score replaces the
  hand-coded Rust dispatch.

Until B2/B3, this file is documentation in declarative form.
Conformance is verified by reading R2-ENSEMBLE.md + R2-DEF.md and
checking the score field-by-field; no automated validator yet.

## Quick reference

* **Identity** — see `ensemble.yaml` `ensemble:` block.
* **Sentant inventory** — `sentants:` array; narrative descriptions
  in `SPEC-R2-WORKSHOP-SENTANTS.md`.
* **Wire events the ensemble emits / consumes** — `capabilities:`
  block at the bottom of `ensemble.yaml`; per-event details in
  `SPEC-R2-WORKSHOP-WIRE.md`.
* **R2-WEB registration** (the dashboard UI) — `registrations.r2-web`
  block in `ensemble.yaml`; HTTP-route details in
  `SPEC-R2-WORKSHOP-DASHBOARD.md` §5.1.

## Building your own deployment from this template

To fork r2-workshop as a starting point for a new ensemble (e.g.
people-counter, pet-gait):

1. Fork the repo.
2. Edit `trust_keys/sensor_class.txt` to your new class string
   (e.g. `nz.ac.auckland.people-counter`). Class-namespace policy
   in `SPEC-R2-WORKSHOP-ENSEMBLE.md` §2.2.
3. Edit `ensemble/ensemble.yaml`: change `ensemble.name`,
   `ensemble.class`, `ensemble.version`, and the sentant /
   plugin set to match your deployment.
4. Swap the sensor-side automation logic (firmware-level) to suit
   your sensor IC + analysis. Substrate crates (r2-wire, r2-fnv,
   r2-trust, r2-bootstrap, r2-cbor, r2-esp, r2-bootstrap) stay
   put.
5. Rebuild the dashboard + per-carrier firmware. Both bake the
   new class string at compile time, so the FNV event-hash table
   rotates and your fleet won't accidentally talk to ours.

Per-deployment customisation is currently a per-fork concern.
Once the BEAM/Rust ensemble loader lands (B2/B3), the runtime
consumes `ensemble.yaml` directly and the fork becomes a
configuration-only change for many ensembles.
