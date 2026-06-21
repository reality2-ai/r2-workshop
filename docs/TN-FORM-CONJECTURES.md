# TN cross-platform form — conjecture/refutation catalogue (firmware tier)

Workshop's firmware-tier **input** to the TN conjecture-catalogue (specs owns the
master; composer/specs own the hardware-test defs). Framed per
REFUTATIVE-DEVELOPMENT.md: each item is a **falsifiable conjecture** + the
**falsifier** (the adversarial experiment designed to BREAK it) + the **edge cases**
most likely to refute. Confidence = survived refutation, never a green happy-path.

These are the conjectures I'm uniquely placed to surface — they concern the
**esp-idf↔esp-radio cross-platform seam** (workshop EspNegotiationRadio + the
canonical r2-discovery engine vs hive's esp-radio impl) and the **EspNow mesh
data-plane**, which only the cross-platform integration exercises. Scope: the
control plane (L2CAP CoC + ControlMsg), the S0–S4 negotiation, the Mode-1 infra
form, and the Mode-2 EspNow mesh. Hardware finding → specs refines canon FIRST →
then code.

## A. Control-plane cross-platform (L2CAP CoC + ControlMsg)

- **C1.** *Conjecture:* a ControlMsg encoded by workshop (esp-idf) decodes
  byte-identically on hive (esp-radio) and vice-versa, for ALL variants
  (WifiReq/WifiOffer/WifiDone) at the size extremes.
  *Falsifier:* fuzz the offer at ssid_len/psk_len = {0, 1, 31/63, 32/64} +
  ap_hint endianness; assert round-trip equality cross-platform, not just
  same-platform. *Edge:* the 32/64 padding boundary; a max-length offer near the
  MTU-512 / MAX_ENCODED_LEN=103 limit.
- **C2.** *Conjecture:* the `[len_lo,len_hi]` LE CoC framing reassembles correctly
  when a ControlMsg spans >1 BLE notification. *Falsifier:* force fragmentation
  (small negotiated MTU) and assert reassembly; *edge:* a frame whose length's
  high byte is non-zero (len > 255); a split exactly on the length prefix.
- **C3.** *Conjecture:* a dropped/duplicated CoC segment does not wedge the
  decoder. *Falsifier:* inject a duplicate len-prefix / a truncated tail; assert
  the decoder resyncs or cleanly errors (never deadlocks the control loop).

## B. Negotiation engine S0–S4 (cross-platform election)

- **N1.** *Conjecture:* two joiners entering S1 **simultaneously** both converge to
  the SAME provider = lowest live hive_id. *Falsifier:* drive two
  EspNegotiationRadios + one hive node to S1 within the same tick; assert single
  elected provider, no split-brain. *Edge:* simultaneous-joins; equal-cost election
  tie; the lowest-id node being the one that's slowest to advertise.
- **N2.** *Conjecture:* **conductor (provider) death mid-session** → followers
  re-elect WITHOUT a re-flood storm (heal-without-re-flood). *Falsifier:* kill the
  provider after form; assert re-election within T_fallback and bounded control
  traffic. *Edge:* conductor+follower dying together; provider death exactly at the
  T_negotiate boundary.
- **N3.** *Conjecture:* **node-flap** (rapid join/leave) does not leak roster
  entries or strand the engine in a non-Discover state. *Falsifier:* flap a node
  N times; assert roster bounded (≤ NEG_ROSTER) + engine returns to a steady state.
  *Edge:* flap faster than the advertise interval; flap the elected provider.
- **N4.** *Conjecture:* a **stale-state rejoin** (a node returning with an old
  provider belief) is corrected, not honored. *Falsifier:* partition a node, change
  the provider, rejoin it; assert it adopts the current provider. *Edge:*
  partition (not just 1-node loss); asymmetric link where the rejoiner hears the
  old but not the new provider.

## C. Mode-1 infrastructure form (SoftAP join)

- **I1.** *Conjecture:* the elected provider's `bring_up_provider` SoftAP +
  joiners' `join_provider` static-IP STA produces a usable data plane, and
  `provider_addr` is **gateway-discovered** (R2-WIFI §4.3), never hardcoded.
  *Falsifier:* assert the joiner reaches the provider via the discovered gateway,
  and that a wrong/stale ap_hint does not silently pin a bad address. *Edge:*
  gateway != .1; provider re-elected to a different node mid-session.

## D. Mode-2 EspNow mesh (true-mesh data plane)

- **M1.** *Conjecture:* a frame routes A→B over EspNow when the engine selects
  `Transport::EspNow` (mesh_preset), dispatched by `DirectedHop.transport` to the
  EspNowTransport — byte-identical payload cross-platform. *Falsifier:* assert B
  delivers exactly what A sent; *edge:* K=1 (single custodian); K>neighbours;
  asymmetric BLE-discovery-vs-EspNow-data links.
- **M2.** *Conjecture:* per-**origin** dedup holds across the cross-platform mesh —
  the same (msg_id, origin) arriving via two paths delivers ONCE; two DIFFERENT
  origins with the same msg_id both deliver. *Falsifier:* multi-path delivery +
  origin collision; *edge:* the dedup 60s boundary; a frame with no carried origin
  (un-deduplicatable, must NOT collapse to origin=0 — the LoRa-source-0 bug).
- **M3.** *Conjecture:* TTL/K decay is correct across a transport gap
  (WiFi→EspNow→LoRa). *Falsifier:* route across mixed transports; assert TTL=0
  stops, K=15 flood-sentinel is preserved across hops, K halves otherwise.
  *Edge:* TTL=0 on arrival; decay-across-a-gap; K=15 vs K=1 at a transport switch.
- **M4.** *Conjecture:* cross-TG **entanglement is non-transitive** — A↔B and B↔C
  entangled does NOT grant A↔C delivery. *Falsifier:* set up the chain; assert C
  cannot deliver to A's TG without a direct entanglement. *Edge:* a relay in the
  middle TG; entanglement non-transitivity across an EspNow hop.

## Cross-cutting

- **X1.** *seed-sweep:* run every conjecture across a sweep of random hive_id seeds
  (election + dedup must not depend on a lucky id ordering).
- **X2.** *partition heal:* split the mesh into two partitions, then merge; assert
  re-form + dedup hold (no double-delivery on merge, no permanent split-brain).

---
*Owner: workshop (firmware tier). Route hardware findings to specs first. Pairs
with the metal window (hive's ESP-NOW demo + a board slot).*
