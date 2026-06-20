# Smallest path: RouteEngine over r2-esp transports (TRUE TN board-to-board)

_Owner: workshop (bring-up + transports). Engine owner: core (r2-route).
Branch: `tn-routeengine-bringup`. Status: PROPOSAL — seam alignment pending core._

## Goal
Move from today's **hub-star** (boards → r2-dashboard → webapp) to **TRUE TN**:
core's `RouteEngine` routing R2-WIRE frames **board-to-board directly** over
workshop's proven WiFi/BLE transports + R2-WIRE framing + R2-BEACON discovery.

## The seam (already mostly defined by the vendored crates)
RouteEngine is a **pure no_std decision brain** — no I/O:

- `r2-route::engine::RouteEngine::plan_forward(ForwardRequest) -> ForwardAdvice`
  returns `ForwardAction::{Drop | DeliverOnly | Directed(DirectedHop) | Flood(..)}`.
- Neighbour/path tables fed by `ingest_observation(Observation{ hive_id,
  transport, rssi/snr/quality, ... })`.
- `r2-transport::Transport` trait is the **send seam** I implement per medium:
  `fn send(&self, target: u32 /*FNV hash of dest hive id*/, frame: &[u8]) ->
  Result<(), SendError>` + `id()/state()/current_mtu()/link_quality()`.
- `r2-wire` already provides Compact (12B hdr, BLE/LoRa) + Extended (22B hdr,
  WiFi/TCP) headers with `ttl, k, msg_id, target(s), MsgType`, route stacks, and
  Compact↔Extended transcode. My firmware already uses `r2-wire` encode/decode.

So the division is clean:

| core owns (the brain) | workshop owns (the limbs) |
|---|---|
| `RouteEngine` correctness, `ForwardRequest`/`ForwardAdvice` contract | Implement `r2-transport::Transport` for each medium |
| Wire header/route-stack semantics (`r2-wire`) | Receive loop: parse hdr → build `ForwardRequest` → `plan_forward` → act |
| Originated-frame TTL/K defaults, dedup params | Feed `ingest_observation` from R2-BEACON scan (rbid/hive_id + rssi) |
| Addressing semantics (hive_id ↔ target hash, 16-bit compression) | `hive_id → IP` (WiFi) / `→ L2CAP handle` (BLE) resolution |
| | RouteEngine wiring into firmware loop; flash + bring-up on hardware |

## Transport choice for first light: **WiFi/UDP** (smallest)
Reasons it's the smallest first hop:
- Connectionless — no L2CAP pairing handshake; both boards already associate to
  the SoftAP and hold `10.42.0.x` IPs.
- I already send UDP (presence burst) — socket plumbing exists in r2-esp.
- WiFi → `WireFormat::Extended` (22B hdr) per `TransportId::wire_format()`; MTU
  65535 ≫ frame. No fragmentation for first light.
- Mirrors hive's no_std WiFi-UDP bring-up → cross-stack interop is plausible.

BLE L2CAP CoC (PSM 0xD2, already a server in `r2-esp::l2cap`) is the strong
**second** transport (true peer link with no WiFi infra) — added after UDP works.

## Hardware
- 2× ESP32-C6 on tuxedo. **My C6 firmware targets DFR1117 (DFRobot _Beetle_
  ESP32-C6-FH4, 4MB).** ⚠️ Supervisor said "_FireBeetle_ ESP32-C6" — if these are
  FireBeetle 2 C6 (DFR1075), same SoC but different pinout/flash → needs a thin
  new carrier (pin map + flash size + partitions); cheap because the C6 ESP-IDF
  path already exists. **Confirm exact SKU before flashing.**
- ESP32-S3 (devkitc/xiao) can also be TN nodes (same Path-A stack).

## Milestone ladder (each independently demoable)
0. **Seam alignment with core** (spec-first) — answer the open questions below.
1. **1-hop direct A→B over WiFi/UDP**: 2 C6 nodes, static `hive_id→IP` seed,
   originate a frame on A with `destination=B` → `plan_forward` → `Directed(B)`
   → UDP send → B delivers locally. Proves RouteEngine-over-my-transport.
2. **Discovery-fed neighbours**: replace static seed with R2-BEACON →
   `ingest_observation`; A learns B without config.
3. **2-hop relay A→R→B**: place B out of A's range (or filter), C relays via
   `Directed`/`Flood` + TTL/K decrement → proves real multi-hop routing + dedup.
4. **Second transport (BLE L2CAP)** + transport selection (engine scores
   BLE vs WiFi by `LinkQuality`/power cost).
5. **Interop with hive's no_std node** on the same WiFi-UDP wire.

## Open questions for core (seam alignment — smallest blocker)
1. **Originated-frame defaults**: for a frame this node *originates*, what
   initial `ttl` and `k` should I seed `ForwardRequest`/the wire header with?
   (Constrained-MCU profile `RouteEngine<16,16,32>` OK for C6?)
2. **Addressing**: `Transport::send(target: u32)` — is `target` the **full**
   `hive_id` or the **16-bit compressed** id (`route_stack::compress_hive_id_16`)?
   And is `ForwardRequest.source_hop` the compressed immediate-sender id? I'll
   own `hive_id → IP` resolution; just need the canonical key.
3. **Local-delivery test**: after `plan_forward`, do I decide "for me" purely by
   `destination == my_hive_id` (and `DeliverOnly`/`TTL=0`), or is there a helper?
4. **Observation fields**: minimum fields you want in `ingest_observation` from a
   BLE beacon hit (I have rbid, class_hash, rssi) and from a WiFi datagram (src
   hive, rssi N/A → latency?).
5. **Frame mutation on relay**: I assume I decrement TTL / rewrite K / push the
   route stack via `r2-wire` helpers (`append_extended`) before re-send — confirm
   workshop does the header rewrite, engine only advises.

## What I can do solo now (no tuxedo / no Roy)
- This plan + seam doc (done).
- Add `r2-route` + `r2-transport` deps to `r2-esp`; scaffold a
  `peer_wifi_udp` module implementing `r2-transport::Transport` (socket, datagram
  send, recv loop, trait impl) — compile-checked on Alfred. Pending Q2/Q5 only for
  the header-rewrite details; the socket + trait shape don't block.
