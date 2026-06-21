# BLE ↔ WiFi Transport Negotiation — workshop reference (implemented vs gap)

**Status:** reference for hive + specs. Maps R2-DISCOVERY v0.2 §4A "Transport
Negotiation (Two-Plane Model)" (canon, commit `baa0a94`) onto what **r2-workshop
firmware actually implements today** vs the **gaps hive's orchestration adds on
top**. Workshop owns the per-platform building blocks (BLE + WiFi radio glue on
ESP-IDF) and the simple WiFi data-plane; hive owns the net-new two-plane
negotiation + conductor-election orchestration.

> **Two profiles (§4A.4, commit `1175e66`).** After workshop's ground-truth
> correction, canon now splits the reference:
> - **Profile A — full two-plane** (control-plane-during-data + disruption→
>   fallback + conductor-election provider) → **r2-hive** is the reference.
> - **Profile B — simple WiFi data-plane + gateway-discovery**, configured/static
>   provider, no on-node election/fallback → **workshop's TN carriers**
>   (`dfr1195`/`c6-tn`) are the reference, and are **CONFORMANT today**. Profile B
>   *participates in* but does not *drive* a two-plane mesh, and MUST NOT be cited
>   for Profile A.
>
> So the TN carriers are built **WiFi-only** (`r2-esp` with
> `default-features=false, features=["tn"]` → NimBLE compiled out) — that's
> Profile B by design, not a deficiency. The BLE building blocks below live in the
> **production sensor firmware** (default `ble` feature) and are the parts hive's
> Profile-A orchestration composes on. Growing workshop to Profile A is a separate
> milestone (Roy's call — not scoped).

## Two-plane model (§4A recap)

- **Control plane = BLE** — always-available, low-power presence + discovery +
  credential/offer exchange. Survives when WiFi is down.
- **Data plane = WiFi** — high-bandwidth R2-WIRE transport (the TN mesh /
  dashboard stream), brought up using what the control plane negotiated.

The protocol's value is the **handoff + fallback** between the two: discover on
BLE → bring up WiFi → detect WiFi disruption → fall back to BLE to renegotiate.

## Stage 1 — Discovery & offer (control plane / BLE)

**Implemented (workshop building blocks):**
- `crates/r2-esp/src/beacon.rs` — R2-BEACON BLE advertise + scan + peer table
  (`start()`, `BeaconHandle::peers()`, `class_hash()`; wraps `r2_core::beacon`
  build/parse). Devices announce presence + class; observe peers.
- `crates/r2-esp/src/wifi_prov.rs` — BLE-driven WiFi **credential** exchange +
  NVS persistence (`WifiCredentials`, `save_credentials()`, `load_credentials()`,
  `wifi_offer_hash()`). The "here's how to join my WiFi" offer.
- `crates/r2-esp/src/l2cap.rs` — BLE L2CAP CoC channel for R2-WIRE events over
  BLE (`init()`, `send()`/`send_to()`, `drain_received()`, `is_connected()`) — a
  working BLE *data path* usable as the control-plane transport.

**Gap (hive adds):** a formal **negotiation state machine** over these — offer/
accept of the *transport choice* (not just credentials), and keeping the beacon
plane **active as a control channel while WiFi is up** (§4A.4(1)). Today beacon =
presence advertising, not a persistent negotiation channel.

## Stage 2 — Data-plane bringup (WiFi)

**Implemented (workshop building blocks):**
- `crates/r2-esp/src/wifi_ap.rs` — `start()` brings up the board-hosted SoftAP
  (the data-plane **provider**).
- `crates/r2-esp/src/wifi_sta.rs` — the **joiner**: `connect()` (DHCP) /
  `connect_static()` (static IP, no-DHCP — for hive's embassy-net r2-fieldlab) +
  `get_gateway()` (AP-IP discovery: the AP **is** the gateway, never hardcoded —
  R2-WIFI v0.6 §3.2/§4.3).
- TN mesh runs over this: `crates/r2-esp/src/tn.rs` (`spawn`) + `r2-tn` node.

**This is the part workshop IS a clean reference for:** simple WiFi data-plane +
gateway-discovery, both standalone (board-hosted SoftAP) and fielded (join an
external AP).

## Stage 3 — Disruption detection & fallback / renegotiation

**Implemented (primitive, boot-time only):**
- Firmware boot-fallback: STA can't join → warn + idle; no IP from AP → warn
  (`firmware/*/src/main.rs`). `wifi_sta::reconnect()` re-associates.
- Reachability primitives: connection state + `get_gateway()` (gateway-known ⇒
  data plane up).

**Gap (hive adds — the core of §4A):**
- §4A.4(2) **runtime disruption detection** (assoc/link loss, AP beacon-silence
  > `T_fallback`, AP `power_state` Critical/Survival, AP-IP/gateway unreachable)
  → **fall back to the BLE plane to renegotiate** (not idle, not fail-silently).
  Workshop has no runtime detector + no `T_fallback` timer.
- §4A.4(3) **provider election** = LOWEST eligible `hive_id` (eligible = AP-capable
  + `power_state` Normal/Eco) with **silence-failover to next-lowest**
  (R2-HEARTBEAT conductor pattern). **Workshop uses a CONFIGURED provider**
  (`R2_TN_AP_MAC` standalone / baked `R2_TN_AP_ID` fielded = hive's AP), NOT
  auto-election. The lowest-`hive_id` election is **hive's conductor** impl — cite
  hive, not workshop, for §4A.4(3).

## Provider selection — today vs canon

| | workshop (today) | §4A.4(3) canon (hive) |
|---|---|---|
| Who hosts the SoftAP | configured: `R2_TN_AP_MAC` / baked `R2_TN_AP_ID` | lowest eligible `hive_id` |
| Failover | none (configured AP is fixed) | silence-failover to next-lowest |
| Pattern source | static bring-up convenience | R2-HEARTBEAT conductor election |

## Composition path (how the pieces wire into §4A)

```
            ┌── BLE control plane (always on) ──┐
beacon.rs (advertise/scan/peers)                │  Stage 1: discover + offer
wifi_prov.rs (WiFi credential offer + NVS)      │  l2cap.rs (BLE event path)
            └───────────────┬───────────────────┘
                            │  negotiate transport (HIVE: state machine)
                            ▼
            ┌── WiFi data plane ────────────────┐
wifi_ap.rs (provider SoftAP)                     │  Stage 2: bring up data plane
wifi_sta.rs connect()/connect_static()/get_gateway()  (provider = HIVE election)
tn.rs + r2-tn (R2-WIRE mesh over WiFi)           │
            └───────────────┬───────────────────┘
                            │  HIVE: detect disruption (T_fallback, power_state,
                            │  gateway-unreachable) → renegotiate on BLE
                            ▼
                back to Stage 1 (fall back to control plane)
```

## Layering — where the Profile-A logic should live (north-star)

When hive builds Profile A, the split follows the **r2-route pattern** (pure no_std
engine + a trait; platform impls per-side), so the logic is written once and both
Path-A (ESP-IDF) and Path-B (esp-hal/esp-radio) reuse it:

1. **Protocol primitives** — `r2_core::beacon` (build/parse), `r2-wire`, `r2-trust`
   (persona/credential codec). Already no_std-shared; both sides reuse directly.
2. **Negotiation logic** — the S0..S4 state machine, `T_fallback` timing, the
   lowest-eligible-`hive_id` election decision, disruption-detection — is a **pure
   no_std state-machine MODULE**, NOT buried in either firmware. **Placement
   (core-confirmed):** the EXISTING `r2-discovery` crate — already the canonical
   transport+discovery assembly (`TransportId` + `AsyncTransport`/`PeerMap`/
   `BeaconAdvertiser` traits + `PeerTable` + a no-alloc tier); the negotiation is a
   module *over those existing traits*, not a new crate. core lands the engine when
   hive sends the S0-S4 transition table. (The lowest-`hive_id` election is the same
   deterministic-lowest-id primitive as conductor-PLL — shared, not re-derived.)
3. **Radio glue (the trait impls)** — per-platform, NOT shared: workshop's
   `r2-esp` (esp-idf-svc + NimBLE) is the Path-A impl; hive's esp-radio is the
   Path-B impl. Two thin platform layers is north-star-correct, not a fork.

This mirrors how routing already works: `r2_route::RouteEngine` (pure no_std) +
the `Transport` trait — workshop provides the esp-idf `UdpTransport`, a no_std
board would provide its own. Do #24 Profile A the same way and a future workshop
Profile-A is just an esp-idf trait impl over hive's shared state machine.

**LANDED:** `r2-discovery::negotiation` (core `03648fb`) — pure no_std heap-free
S0-S4 + `NegotiationRadio` trait (advertise / poll_scan→NegObservation /
send_control / poll_control / bring_up_provider / join_provider /
data_plane_state→{Available,Failed} / teardown / now_ms) + `NegotiationEngine<N>`
fixed-cap roster + a shared `lowest_live_id` election primitive (= conductor-PLL's).
A workshop Profile-A = impl `NegotiationRadio` on esp-idf (hive does esp-radio).

**Protocol item before workshop Profile-A** (flagged → core → specs, spec-first).
Eligibility = `provider_capable AND power ∈ {Normal,Eco}`, computed by the engine
from the R2-BEACON §7.2 flags byte: **bits 1-0 = power_state** (00 Normal/01 Eco/10
Critical/11 Survival, §7.2.1 MUST-set) + **bit 2 = provider_capable** (new; bit 2
is reserved-MUST-be-0 today). `ap_capable` is NOT derived from class_hash (specs:
fragile). The provider's eligibility flipping false = the disruption signal.

⚠ **Code-vs-spec drift caught:** the canonical `r2_core::beacon::BeaconFlags`
(fields `{profile,has_bloom,provisioning,mcu_mode,mobile}`, bits 7-3) comments
**"2-0: reserved"** and neither decodes nor encodes the §7.2 power bits 1-0. So
today power_state is **unreadable AND unsent** (encode leaves bits 1-0=0=Normal-by-
accident; Critical/Survival can never be signalled). The §7.2 flags-byte change is
therefore **power_state decode+encode (bits 1-0) PLUS provider_capable (bit 2)** —
same Roy-authorization batch, core lands it in `r2_core::beacon`.
**Workshop side stays trivial:** my `PeerObservation` carries `flags: BeaconFlags`,
so once core adds the fields + I re-sync vendored r2-core, my esp-idf side surfaces
power_state + provider_capable for free. core adjusts `NegObservation`; both
platforms read the same §7.2 byte.

## §4A.4 conformance summary

The §4A.4(1-3) requirements below are **Profile A** (hive's reference). Workshop's
TN carriers are **conformant as Profile B** — Profile B does not require 1/2/3
(configured provider, no on-node election/fallback). The table shows where
workshop has *building blocks* vs where Profile A's net-new orchestration (hive)
lives.

| § (Profile A) | Requirement | workshop (Profile B) | Profile-A owner |
|---|---|---|---|
| 1 | BLE beacon active while WiFi up | building block only (beacon.rs; TN build has BLE OFF) | hive |
| 2 | Detect disruption → fall back to beacon | boot-time idle only; no runtime detector / `T_fallback` | hive |
| 3 | Provider = lowest eligible `hive_id` + silence-failover | configured provider (Profile B by design) | hive (conductor pattern) |
| 4 | Document `T_fallback` + triggers | n/a for Profile B; proposals below if workshop grows to A | hive + workshop (when built) |

**Profile B conformance (workshop, today): ✅** WiFi data-plane + gateway-discovery
(R2-WIFI v0.6 AP=gateway/no-hardcode), configured/static provider, participates in
a two-plane mesh without driving it.

## T_fallback (§4A.4(4))

**Workshop value today: undefined** — no fallback mechanism exists. When workshop
grows the §4A two-plane (a new milestone, on Roy's word), proposed starting
values (transport-profiled, not pinned in canon pending #21/#22 LoRa tuning):

- WiFi data plane: `T_fallback` ≈ 3 × beacon interval or ~6 s of
  assoc/gateway-unreachable, whichever first (WiFi jitter is ~ms; a few seconds
  distinguishes a real drop from a transient).
- Triggers (per §4A.4(2)): STA assoc loss event · `get_gateway()` returns
  None / gateway ARP-unreachable · AP power_state Critical/Survival (once
  power-state is on the beacon) · AP beacon silence > `T_fallback`.

These are **proposals for when the milestone is funded**, not implemented.

---
*Maintained by workshop (ESP32 firmware/build reference). hive builds the
negotiation + election orchestration on these building blocks; specs canon =
R2-DISCOVERY v0.2 §4A.*
