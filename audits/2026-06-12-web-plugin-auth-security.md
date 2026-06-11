---
title: Web-plugin (HTTP) auth surface — security findings + fix
date: 2026-06-12
status: open
severity: high (conditional on exposure beyond the trusted AP)
author: workshop-worker (Alfred build/release session)
---

# Web-plugin auth surface — security findings + fix

## Scope

The controller Hive's **HTTP surface** (the R2-WEB plugin: `blob_routes` +
`diagnostics` + bootstrap routes in `dashboard/src/main.rs`), as distinct from
the operator/control plane, which already rides **R2 events** on `/r2`
(`r2.dash.cmd.*` → `r2.dash.*.progress`, migrated in v0.2).

## TL;DR / verdict

The control plane is event-based and (intended to be) cert-gated. The
**surviving HTTP routes are not** — they are a *weaker auth path than the
event plane*. Today the whole system's security rests on **network isolation**
(the closed, non-internet-facing sensor AP) **+ operator trust**, NOT on the
TG role model. That is acceptable for an isolated bench rig, but every HTTP
mutating/blob route is reachable, unauthenticated, by **anyone on the AP** —
and the moment the surface is reachable beyond a trusted LAN (relay/remote
viewers, a shared network), the gaps below are live holes.

## Findings (grounded in `dashboard/src/main.rs` @ cd9609f)

1. **`/api/ota/{addr}` — ungated AND unsigned (highest risk).**
   `POST ota_push_handler` (no `require_access`); Phase **9-light** OTA has **no
   on-device TG-signature verification** — the sensor flashes whatever bytes
   arrive. ⇒ Any client on the AP can push **arbitrary firmware** to a sensor
   = full device compromise. (Sensor-side `r2_esp::ota_tcp` likewise trusts the
   stream.)

2. **`/api/data/*` — ungated, including destructive routes.**
   `GET …/file` + **`DELETE …/file`**, **`DELETE …/{addr}/all`**,
   **`DELETE …/local/all`**, `…/merged`, `…/zip` — all with **0**
   `require_access` calls. ⇒ Any AP client can **exfiltrate or wipe** all
   capture data (per-sensor and controller-local).

3. **Role model exists but is barely enforced.**
   `require_access` (the cert/role gate) is invoked in **exactly two**
   handlers (the access/identity endpoints). The `DeviceCertificate` + three
   roles (Owner/KeyHolder, Member, viewer-variant) are defined
   (SPEC-R2-WORKSHOP-ACCESS) but **not applied** to the OTA/data/blob surface.

4. **"Remote = read-only" is operator-discretion, not cert-gated.**
   PLAN 5d (verbatim): *"today's enforcement is operator-discretion, not
   cert-role-gated."* There is no cryptographic role enforcement on the HTTP
   plane; `/r2` itself is v0.1 anonymous (ACCESS §5.1 additive model; cert-
   handshake is the v1 target).

## Risk

- **Current deployment (isolated rocker rig, closed USB-AP, operator-driven):**
  acceptable — the AP is the trust boundary and it is not internet-facing.
- **Any exposure beyond a trusted LAN** (relay/remote path, shared/campus
  network, a compromised browser or device already on the AP): findings 1–2
  are directly exploitable — arbitrary firmware push + data wipe, no creds
  needed.

## Fix

In priority order. (1) is load-bearing for OTA; (2)–(3) close the
HTTP-as-bypass gap; (4) is the durable, composer-aligned form.

1. **Phase 9-secure — TG-signed OTA images, verified on-device.**
   `r2-build` produces TG-signed `.bin`s; the sensor verifies the signature
   under its embedded TG public key **before committing** the OTA slot
   (rollback via the SD boot-recovery hook). Until this lands, OTA integrity =
   "is the AP truly trusted?" and nothing else. (Already roadmapped: PLAN
   9-secure ⏳; the future `r2.dash.fw.update {url, sha256, tg_sig}` event is
   the signed control path.)

2. **Gate the *actuation* at a sentant state machine, not the route
   (preferred — makes transport irrelevant).**
   Per-handler `require_access` checks work but are bolt-ons that can be
   forgotten (exactly what happened here). The R2-native fix: a destructive
   operation is a **sentant state-machine transition that only fires on a
   TG-authenticated message** (verified signature + cert role). The HTTP route,
   if kept, becomes a dumb ingress — at most it *stages bytes* (e.g. the OTA
   image blob) but **cannot itself commit**; the commit is a sentant transition
   triggered by a TG-signed command. So an unauthenticated HTTP call produces
   no effect, because the sentant ignores any actuation that isn't carried by a
   valid TG-authenticated frame. Auth becomes **intrinsic to the action**, not
   a property of which port it arrived on.
   - This already has a specced shape: `r2.dash.fw.update {url, sha256,
     tg_sig}` (SPEC-DASHBOARD §13, the v1 signed-OTA control event) is exactly
     such a TG-signed command; pair it with 9-secure (on-device verify) and the
     `/api/ota` blob is inert without the matching signed commit.
   - Same pattern for data deletes: a `DataStore` sentant only executes a
     delete/wipe on a TG-signed `r2.dash.cmd.data.delete*`; the `DELETE`
     HTTP verb routes to "emit the command", never to direct filesystem action.
   - Falls straight out of R2-PLUGIN §13 (the server role hosts no handlers;
     the sentant owns the logic) — the destructive routes were the one place
     that principle wasn't yet applied.

3. **Cert-role enforcement of remote read-only** (ACCESS v1 cert-handshake on
   `/r2` + the HTTP surface), replacing UI/operator discretion. Remote =
   read-only **by cert role**, not convention.

4. **Composer-shaped principle (the durable answer).**
   Since workshop is heading toward being an *output of r2-composer*
   ([[workshop-superseded-by-composer]]), the fix is not per-handler bolt-ons:
   **R2-WEB `blob_routes`/`diagnostics` should declare a required role, and the
   plugin enforces it uniformly** — so a generated HTTP interface is
   secure-by-construction and a route *cannot* accidentally ship ungated. The
   per-handler gaps documented here are the cautionary example feeding that
   model.

## Interim mitigation (until 1–3 land)

- Keep the sensor AP **closed and non-internet-facing**; treat it as the sole
  trust boundary.
- Do **not** expose `:21042` / the HTTP surface beyond a trusted LAN.
- Treat the relay/remote path as **untrusted** for anything mutating until
  cert-gating lands (read-only viewing only).

## Cross-references

- `ensemble/controller.yaml` — `registrations.r2-web` (blob_routes/diagnostics).
- SPEC-R2-WORKSHOP-DASHBOARD §13 (OTA), SPEC-R2-WORKSHOP-ACCESS (roles, §5.1).
- PLAN phases: 9-light ✅ / 9-secure ⏳, 5d (remote read-only deferred).
- `docs/own-hive-web-ui-recipe.md` (the two-hive / R2-WEB split).
