# Own-hive web-UI recipe (r2-workshop dashboard as the reference)

A short reference for building a **UX-plugin-with-its-own-hive** (e.g.
composer's transient-networking "proof-surface" test UX), using the
r2-workshop dashboard as the worked example. Written for cross-repo reuse.

## The shape: two hives, one channel

```
 controller hive (gateway/server)            browser hive (viewer)
 ────────────────────────────────            ─────────────────────
 serves the static bundle  ──────────────►   loads webapp/ + WASM hive
 forwards R2-WIRE frames   ──/r2 (WS)──────►  R2WorkshopHive.send_event()
   (NO event handlers of its own)              └ DashboardViewerSentant
 fans bus events to /r2    ◄──/r2 (WS)───────  emits r2.<x>.cmd.* gestures
```

Key invariant (R2-PLUGIN §13): **the server role is exhausted by (a) serving
the bundle and (b) forwarding `/r2` frames.** It hosts no UX event handlers.
All UX state + logic lives in the *browser* hive's Viewer sentant and the
bundle JS. A sibling deployment reuses the sentant unchanged and ships a
different bundle skin.

## How r2-workshop does it today (shipping monolith)

`dashboard/src/main.rs` — a single AOT-compiled axum binary:

1. **One TCP port (`:21042`), peek-dispatched** (WIRE §13.5): peek the first
   byte — HTTP-looking → axum (HTTP + WS upgrade); otherwise raw R2-WIRE TCP.
   Unifies MCU raw-TCP peers, browser WS, and HTTP on one port.
   *(Only needed if you also have non-browser raw-TCP peers. A pure
   browser test-UX can skip peek-detect and just run axum HTTP+WS.)*
2. **`/r2` WebSocket = the live event stream** (R2-WIRE-over-WS,
   R2-TRANSPORT §3.5). A `tokio::sync::broadcast` channel (`DashboardEvent`)
   fans every bus event to all connected viewers; a **cached "last state" is
   replayed on connect** so a late-joining viewer re-syncs immediately.
3. **Static bundle** via `tower_http::services::ServeDir` mounted as
   `fallback_service` at `/` — explicit `/api/*` + `/ws/*` routes win;
   everything else falls through to the static assets. Same-origin with the
   WS endpoint ⇒ no CORS.
4. **Browser = canonical viewer**: `crates/r2-wasm` exposes `R2WorkshopHive`
   (wasm-bindgen entry) which registers `DashboardViewerSentant` on the
   in-browser EventBus. JS forwards each `/r2` frame in via
   `send_event(hash, payload)`; the sentant maintains UI-mirror state; the
   page renders from sentant state; operator gestures emit `r2.dash.cmd.*`
   back over `/r2`. (`workshop_hive.rs` shows the "multiple hives, one crate,
   same EventBus, different sentants" pattern — directly cloneable.)

## The formal R2-WEB way (what composer should generate)

r2-workshop already authors this in `ensemble/{controller,viewer}.yaml`. The
binaries are hand-compiled forms of it (Phase 5d-ensemble: the YAML documents
the composition; runtime interpretation via r2-engine is only used by the
browser WASM hive). **Composer is the formal-generation tool, so it should do
this the formal way rather than hand-rolling a monolith.** The whole web
surface is one `registrations.r2-web` block on the hive:

```yaml
registrations:
  r2-web:
    route_prefix: /
    static_bundle: ../webapp/            # the deployment-specific UI skin
    channels:
      - name: r2
        target_sentant: Fleet            # controller bus; or Viewer in-browser
        max_frame_bytes: 65536
    blob_routes:                         # bulk-binary HTTP (NOT events)
      - /api/firmware/{carrier}/binary
      - /api/data/...
    diagnostics:                         # pure-local, no hive state
      - /api/version
```

`channels` = the live `/r2` event stream; `blob_routes` = bulk binary pulls
(deliberately not events); `diagnostics` = stateless local routes. That's the
entire server surface.

## Reuse vs rebuild for a fresh test-UX plugin

**Reuse (pattern + template):**
- The `registrations.r2-web` block verbatim — swap `static_bundle` +
  `target_sentant`.
- The two-hive split (server forwards+serves, never handles; browser hive owns
  UX). This is the precedent composer is citing.
- `/r2` = broadcast-fanout **+ replay-cached-state-on-connect** (the re-sync
  trick is easy to forget and essential for late joiners).
- `crates/r2-wasm` `R2WorkshopHive` / `DashboardViewerSentant` as a literal
  template for a `Proof…Hive` / `Proof…ViewerSentant`.

**Rebuild (deployment-specific):**
- The Viewer sentant's mirror-state + the bundle JS skin.
- The event vocabulary (`r2.<proof>.cmd.*` / `.progress`) + `capabilities`.
- The viewer/controller ensemble YAML roles.

**Skip for a browser-only test UX (don't cargo-cult the monolith):**
- Peek-detect (no raw-TCP MCU peers) — plain axum HTTP+WS.
- The TG cert/access pairing flow, SD/OTA blob routes, battery/sensor
  plumbing — none of that is part of the web-UI pattern.

## Pointers
- `dashboard/src/main.rs` — peek-detect listener, `/r2` WS, `ServeDir` mount.
- `crates/r2-wasm/src/workshop_hive.rs`, `workshop_viewer.rs` — browser hive.
- `ensemble/controller.yaml`, `ensemble/viewer.yaml` — the formal R2-WEB form.
- SPEC-R2-WORKSHOP-DASHBOARD (webapp behaviour), WIRE §13.5 (port unification).
