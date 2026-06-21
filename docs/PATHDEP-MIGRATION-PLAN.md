# Path-dep migration runbook — r2-discovery + r2-fnv (for #24 Profile-A)

**Status:** PREP (supervisor-cleared). **Execute on:** Roy's Profile-A greenlight
AND hive's "esp-radio CoC sending" ping (trait settled). Until then this is a
ready-to-run plan, not executed — it churns currently-working builds.

**Goal:** workshop boards JOIN the transient mesh via an esp-idf `NegotiationRadio`
impl over the SAME canonical `r2-discovery` engine (cross-platform TN proof,
north-star). core's call: PATH-DEP r2-discovery + r2-fnv (NOT vendor — r2-discovery
is interop-critical + fast-moving; a vendored snapshot = mesh incompatibility).

## Facts established (prep)
- Vendored crates depping `r2-fnv` (must unify to one copy): **r2-core, r2-trust,
  r2-wire, r2-engine, r2-bootstrap, r2-wasm** (+ `crates/r2-fnv` itself, to drop).
- Canonical `r2-discovery` uses `std` (peer_table.rs HashMap/Mutex) — **fine for my
  esp-idf target (Path A = std)**; hive's no_std side uses the no-alloc tier. So
  r2-discovery is compatible with my build via its alloc/std tier.
- Sibling canonical checkout present at `../r2-core/crates/{r2-fnv,r2-discovery}`.
- Relative path from `crates/<x>/Cargo.toml` → canonical = `../../../r2-core/crates/<crate>`
  (verify per-crate depth at execution).
- core is providing a shared `r2_discovery::ControlMsg` (byte-exact encode/decode
  both platforms) — closes the codec point; my impl decodes via that type.

## Steps (execute in order; checkpoint first)
0. **Checkpoint:** branch `pathdep-migration` (or commit) before — GitHub rollback buffer.
1. **Unify r2-fnv:** repoint the 6 consumers' `r2-fnv = { path = "../r2-fnv" }` →
   `{ path = "../../../r2-core/crates/r2-fnv" }`; delete `crates/r2-fnv`. (Fallback
   if path-repoint fights resolution: a root `[patch]` per core.) Verify ONE r2-fnv:
   `cargo tree -i r2-fnv` shows a single version.
2. **Path-dep r2-discovery:** add to `crates/r2-esp/Cargo.toml`:
   `r2-discovery = { path = "../../../r2-core/crates/r2-discovery" (pin 53c1e58), default-features = false, features = ["alloc"] }`
   (alloc tier = AsyncTransport/ControlMsg/NegObservation/BeaconFlags; pick the
   minimal tier that exports ControlMsg + the beacon codec + NegotiationRadio).
3. **Repoint beacon:** `crates/r2-esp/src/beacon.rs` imports `r2_core::beacon` →
   `r2_discovery::beacon` {BeaconFlags(+power_state), PowerState, LegacyBeacon,
   build_legacy_beacon, parse_legacy_beacon, BEACON_VERSION, compute_rbid}. Drop the
   now-dead vendored `crates/r2-core/src/beacon.rs` + its mod/re-export.
4. **VERIFY builds (gates):** host `cargo test` (r2-tn/r2-trust/r2-wire still green) +
   ESP `cargo check --release` for devkitc (ble), dfr1195, c6-tn. Canonical
   r2-discovery must compile for **xtensa + riscv** esp-idf (its std/alloc tier). If
   Cargo resolution or an esp build fights → ping core (he offered).
5. **Impl `r2_esp::negotiation` (esp-idf NegotiationRadio):** a struct over the
   existing radio glue, impl the trait:
   - `advertise(BeaconAd)` → `beacon::start`/update advert.
   - `poll_scan()->NegObservation` → from `beacon` scan; fill hive_id + ap_capable
     (BeaconFlags bit2 once Roy lands it) + power_state (BeaconFlags bits 1-0). Also
     populate the **HiveId↔BLE-addr map** here.
   - `send_control(HiveId,&ControlMsg)` → map HiveId→addr → encode with the SHARED
     codec (core 844d53e): `let mut b=[0u8; ControlMsg::MAX_ENCODED_LEN]; let n=
     msg.encode(&mut b);` → `l2cap::send_to(addr, &b[..n])` (my frame adds the
     [len_lo,len_hi] prefix). MAX_ENCODED_LEN=103 ≪ MTU 512, no frag.
   - `poll_control()->(HiveId,ControlMsg)` → `l2cap::drain_received()`→(payload,addr)
     → map addr→HiveId → `ControlMsg::decode(payload)` → `Option` (total-safe, None
     on bad input; payload = frame AFTER my len-prefix strip). Same r2_discovery type
     hive encodes with → byte-identical.
   - `bring_up_provider(params)` → `wifi_ap::start`.
   - `join_provider(params)` → `wifi_sta::connect_static`.
   - `data_plane_state()` → from wifi conn state / `get_gateway()`.
   - `teardown_data_plane()` / `now_ms()` → esp-idf time.
6. **Test:** esp-idf↔esp-radio control-plane interop with hive (CoC PSM 0x00D2,
   shared ControlMsg). Then full mesh: workshop board joins hive's transient mesh.
7. **Rollback:** if it fights, `git switch -` / revert to the checkpoint; the TN
   carriers + sensor firmware are unaffected on vendored crates.

## Canonical reference + refinements (core brief R2-24-NEGOTIATION-BRIEF.md @631b758)
Pairs with this runbook — the canonical surface my impl targets. Key refinements
folded in for step 5:
- **Imports** (`r2_discovery`): NegotiationEngine/Radio/State, NegObservation,
  NodeCaps, DataPlaneParams, ControlMsg, DataPlaneState + compute_rbid,
  derive_beacon_session_key, resolve_rbid / resolve_rbid_windowed.
- **poll_scan RBID resolution:** observed RBID is HMAC-rotating
  (`HMAC-SHA256(session_key, epoch_be64)[0:8]`); resolve it to hive_id via
  `resolve_rbid(observed)` (session_key = `derive_beacon_session_key(hk, hive_id)`).
  The HiveId↔BLE-addr map = resolve_rbid(scanned RBID) → hive_id, paired with the
  connectable-adv addr.
- **I POPULATE my flags:** `provider_capable` = can-SoftAP (true for my boards),
  `power_state` = actual hive state (bits 1-0); read peers' from their flags byte.
- **TWO adv sets (NEW for my beacon.rs):** run BOTH the non-connectable RBID beacon
  (discovery, what beacon.rs does today) AND a CONNECTABLE adv (carries same RBID)
  so a joiner gets the connect addr for the L2CAP channel. beacon.rs needs the
  connectable-adv addition for (A). (Provider-star: election picks ONE provider;
  each joiner opens ONE CoC to it. PSM 0x00D2 = Event; 0x00D3 = OTA, unused by me.)

## Caveats
- Don't start before the NegotiationRadio TRAIT settles (hive's esp-radio impl
  first) — building step 5 against a moving trait = rework.
- provider_capable (BeaconFlags bit 2) pends Roy; until then ap_capable is unknown
  on the wire — eligibility degrades to power-only. Fine for first interop.
- This unifies r2-fnv to canonical (drift-elimination, north-star). It may surface
  other vendored-vs-canonical drift in r2-core/wire/trust — handle per core's
  guided reconcile if so (out of scope for the first cut; only r2-fnv + r2-discovery).
