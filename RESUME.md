# RESUME — r2-workshop (workshop-worker)

_Owned by this session. Keep current. Last updated 2026-06-09._
Master save (read-only): `r2-fleet/fleet-context/FLEET-CONTEXT-SAVE.md` (+ plan + DEV_STATUS).
(Relocated 2026-06-18 from `claude-fleet/fleet-context/`; claude-fleet is now tooling-code-only.)

## FRONTIER (2026-06-20, post-DELIVERED)
- **⏸ C6 rebake PAUSED (supervisor):** Roy may pull the FireBeetles/C6 for the
  9-board all-S3 mesh (5 DFR1195 + 4 XIAO, composer's flash). C6 fielded work is
  CODE-COMPLETE + blob shipped (`/tmp/c6-tn-fielded.bin` 3416cbfd, AP_ID=0x480e900e
  ✓metal-confirmed) — resumes when boards reconnect, no loss. Refocus = rung-2b
  (done+core-validated) + trust tier (done) + XIAO support if composer asks.
- **DONE this session:** SEC-01 zeroize hardening (`9550821`, key structs
  non-Copy + zeroize-on-drop + redacted Debug; r2-tn 21/21). Sizing safe: firmware
  McuNode=RouteNode<16,16,32> (16 neighbours ≥ 8 for 9-board; DEDUP 32).
- **DONE — shared parse_persona adopted** (`8fe10d9`): `persona_flash` calls
  `r2_trust::parse_persona` (PV1-locked to composer's producer); my `r2_tn::persona`
  fork + r2-fnv dep REMOVED; r2-esp +r2-trust dep. r2-trust 30/30, r2-tn 19/19.
- **DONE — beat-LED node hook** (`d763c91`): `on_inbound` Heartbeat-for-our-TG →
  `Inbound::Heartbeat`; `Node::poll → PollEvent{Delivered|Beat}`; tn loop logs BEAT.
  Host test (r2-tn 20/20). hive's contract (MsgType::Heartbeat + target_group==my_tg,
  no PLL/HMAC, visual only).
- **TODO when C6 resumes (boards out):** (1) beat-LED GPIO DRIVE — toggle GPIO15/D13
  mono on `PollEvent::Beat` (firmware glue + composer §12.3 carrier-aware driver;
  needs the LED pin peripheral + boards). (2) REBUILD fielded C6 blob (3416cbfd
  predates shared-parser + beat-hook — functionally equiv; rebuild for PV1 parser)
  with R2_TN_AP_ID=0x480e900e/r2-fieldlab/r2fieldlab. Both OTA-ship.
- **ACTIVE — R2-WIRE v0.6 msg_id-into-span LANDED** (core canonical e21f863, Roy
  authorized; closes the msg_id-rewrite replay vector). NEW HMAC span = `type ||
  msg_id(2BE compact/4BE extended) || event_hash || target_group || target_hive ||
  payload`. Same-version boards unaffected (just tag bytes differ); **version-mix
  deliver-blocks** (v0.5↔v0.6 span mismatch) → pull v0.6 to ALL boards together.
  ASKED core to re-sync the hmac.rs span into my VENDORED r2-wire (preserve my
  alloc/no_std `_inner` split via route-through-`authenticated_bytes_extended`);
  I review+commit+test, core verifies vs vector b705ebae. NOT urgent (boards out;
  same-version demo fine) but REQUIRED before the C6 rejoin hive's v0.6 r2-fieldlab.
  Relays still never touch msg_id (§8.5) — my relay path (ttl/k/route only) unchanged.
  r2-route v0.4 origin-dedup = multi-hop/LoRa only, NOT a 9-board blocker (my
  single-hop broadcast mesh already gets exactly-once via route_stack[0] dedup).

- **C6 MESH UP** — composer flashed 3 C6 (exit 0; 6bd0=AP/7e44/6eb0=STA, blob
  9a35b7f8). But it's a SEPARATE mesh from the DFR1195/r2-fieldlab one (my C6 hosts
  its own r2-tn-lab AP).
- **CROSS-ARCH UNIFY (8-board single mesh) — supervisor DECIDED: C6 join r2-fieldlab.**
  FIELDED-MODE CODE DONE (`e5f0997`, both carriers compile): baked `R2_TN_AP_ID` →
  canonical §6.2.1 hive_id (fnv of load_identity UUID; mints master_secret+TG-of-one
  in NVS) + baked AP id (canonical id not MAC-derivable). Standalone r2-tn-lab mode
  unchanged (MAC-FNV). Health-emit already in fw.
  **VERIFIED FIELDED CONFIG (hive + supervisor confirmed on metal):**
  • SSID=`r2-fieldlab` PSK=`r2fieldlab` (WPA2, 10ch). AP=hive S3 MAC 502698 @
    **192.168.4.1** (embassy-net SoftAP).
  • **NO DHCP** — each C6 STA SELF-ASSIGNS STATIC **192.168.4.(low MAC byte)**
    (avoid .0/.1/.255), mask /24, gw 192.168.4.1.
  • **TRANSPORT = BROADCAST** to **192.168.4.255:21042**; `target_hive` addresses
    delivery (not unicast-to-AP). My UdpTransport must broadcast (SO_BROADCAST).
  • Bake **R2_TN_AP_ID=0x3e0d688f** (hive AP canonical wire id; UUID string TBD
    from hive's next reflash for fnv cross-check). UDP 21042 confirmed.
  • hive ADDING explicit L3 relay (re-broadcast on plan_forward, TTL−1, dedup) so
    cross-arch STA↔STA is guaranteed; raising SoftAP max_connections ≥8.
  **REBAKE — CODE-COMPLETE (`e241067`) except beat-LED; both carriers compile:**
  1. JOIN r2-fieldlab — DONE: broadcast transport (`b663b65`) + static-IP STA
     (`wifi_sta::connect_static`, `3eef46b`) + `run_fielded` main path (early-return
     when R2_TN_AP_ID baked; self-assign 192.168.4.<lowMAC>, bcast .255:21042,
     target AP 0x480e900e). Standalone r2-tn-lab untouched.
  2. PERSONA + trust — DONE: derive_hive_id re-synced (`b68e756`); host-tested
     `r2_tn::persona::parse_persona` (r2-tn 21/21, round-trips composer's bundle);
     `r2_esp::persona_flash::read_persona()` raw esp_flash_read @0x12000;
     `Node::new_with_trust` + TnConfig.trust. Fielded: persona→trusted TG-4b3df45d
     (hive_id+hk), else canonical untrusted (B1 routing-join).
  3. LIGHT beat-LED — **TODO (only remaining rebake item):** hive's beat =
     `MsgType::Heartbeat, target_group==my_tg` (payload conductor4B+ver4B), NOT an
     event hash → filter Heartbeat in C6 deliver path → drive **GPIO15/D13 mono**
     (DFR1075; polarity verify on metal). Needs node to surface Heartbeat frames.
     No PLL. Separable — flash the trusted-join blob first.
  4. #18 health-emit — DONE.
  **FIELDED C6 BLOB** built (R2_TN_AP_ID=0x480e900e R2_TN_AP_SSID=r2-fieldlab
  R2_TN_AP_PSK=r2fieldlab) → composer write-bins per-C6 persona @0x12000 + flashes.
  **BAKE: R2_TN_AP_ID=0x480e900e** (hive post-abde165; supersedes supervisor's
  earlier pre-abde165 0x3e0d688f — confirm w/ hive on metal). SSID r2-fieldlab /
  PSK r2fieldlab / ch6. Routing/relay trust-agnostic (B1); persona = SECURE
  TG-4b3df45d member. Conductor = global-lowest canonical id (C6 won't run PLL).


- **8-board mesh blobs SHIPPED to composer:** S3 `dfr1195` (default 124739a5 /
  relay 5490c063, AP=b60aa0) + C6 `c6-tn` (default **9a35b7f8**, AP=f0:f5:bd:07:6b:d0,
  STAs 7e44/6eb0) — all on tuxedo /tmp/. composer flashes (5 DFR1195 + 3 C6 → 8).
- **DONE — #18 r2.hb.health EMIT** (`41aa8da`): both S3+C6. STA unicasts to AP
  collector every 5th beat (~15s), full 13-key payload, sync_state=0; AP=collector
  (no self-emit). r2-esp re-exports r2_tn::health. Ships to fielded boards via #17
  OTA. (AP-forward-to-dashboard hop is composer's side.) The shipped C6 blob
  9a35b7f8 PREDATES this — re-flash or OTA (asked composer).
- **DONE — rung-2b XChaCha20 cross-TG encryption** (core-vetted plan): Entanglement
  +enc_key; `originate_cross` seals [nonce:24][XChaCha20Poly1305(enc_key,nonce,pt)]
  + PeeringHmac over hdr||nonce||ct; gate's verifying entanglement decrypts →
  deliver plaintext (decrypt-fail→drop). Two MACs per §7.5. nonce caller-supplied
  (esp_random/OsRng). **r2-tn 17/17 + S3/C6 ESP-compile.** Sent core the frame to
  vet the HMAC span. ⇒ **FULL TRUST LADDER COMPLETE in canon code** (routing →
  intra-TG → inter-TG auth → inter-TG encryption).
- **rung-2b CORE-VALIDATED** ("ship it"): sign_extended span = §7.5's immutable
  header (msg_type||event_hash||target_group||target_hive) || nonce || ct — same
  exclusion as GroupHmac (§10.6), REQUIRED because B1 relays rewrite ttl/k/route
  in flight. Verify-then-decrypt correct. **Watch (specs-level, NOT mine):**
  msg_id excluded from span → MITM-rewrite-msg_id replay (harmless idempotent;
  matters for non-idempotent cross-TG commands; 60s dedup mitigates). core asking
  specs if msg_id should join the authenticated span; if ratified → r2-wire span
  change, then I adopt. No change now.
- **DEFERRED — LoRa carrier work (#21, post-WiFi-mesh):** encode SX1262 PINOUT
  per carrier in the board profile (DFR1195 integrated SX1262 vs XIAO Wio-SX1262
  separate module — different SPI/NSS/RST/BUSY/DIO1/ant-switch; FETCH Seeed
  Wio-SX1262 datasheet). core building a PIN-PARAMETRIC SX1262 driver (reads
  per-carrier PinConfig) + per-carrier LED pin (composer §12.3 r2.hw.led). Stage
  when I reach LoRa; my current TN carriers are WiFi-only (r2.hw.lora absent).
- **Small firmware canon TODOs (next touch):** (1) R2-WIFI v0.6 MUST-NOT-hardcode:
  drop the `192.168.71.1` STA fallback → skip-if-no-DHCP-gateway (rarely triggers;
  gateway-read already primary). (2) hive_id §6.2.1 (drop MAC shortcut, needs TG
  provisioning). (3) per-carrier LED pin/polarity for XIAO/C6 + composer §12.3
  r2.hw.led (my TN nodes don't drive an LED yet — coordinate when relevant).
- **XIAO prep — BLOCKED on clarification** (asked supervisor): (a) do the 4 XIAO
  run MY std blob (→ I bake r2-fieldlab creds + XIAO MACs) or HIVE's no_std binary
  (→ hive's bake)? (b) composer's board-info didn't read the 2 seeed-XIAO MACs
  (ACM3/4; need BOOT-button) + found only 2 XIAO + 2 spare-S3, not 4.
- **C6 board MACs** (composer-probed): AP f0:f5:bd:07:6b:d0 (ACM8), STA
  f0:f5:bd:07:7e:44 (ACM5), f0:f5:bd:07:6e:b0 (ACM6).

## ACTIVE: TRUE TN — RouteEngine over my transports (branch `tn-routeengine-bringup`)
**Frame ROUTES board-to-board through core's RouteEngine — incl. over REAL UDP
sockets — done on Alfred, pushed.** Branch HEAD `ff01a04`.
- **`crates/r2-tn` (new, host-buildable) — `cargo test -p r2-tn` = 5/5:**
  - `RouteNode<N,P,D>` driver glue (+ `McuRouteNode = <16,16,32>`): `originate()`
    + `on_inbound()` (parse → deliver-if `target_hive==self` → learn-peer +
    reverse-path → `plan_forward` → `prepare_relay_extended` → `Transport::send`;
    flood excludes the inbound peer). Tests: direct A→B, 2-hop relay A→R→C
    (asserts TTL−1), not-for-us.
  - `udp::UdpTransport` (pure-std `r2-transport::Transport`): send/broadcast,
    non-blocking recv, hive_id↔addr both ways. Test `routes_a_to_b_over_real_udp`
    routes a frame A→B over **actual loopback UDP**.
  - `node::Node`/`McuNode` (firmware-facing): `poll(now)`→`Delivered` +
    `originate(...)`. Test `node_api_routes_a_to_b_over_udp` over real UDP.
- **`crates/r2-esp/peer_wifi_udp.rs`:** re-exports `r2_tn::udp::UdpTransport as
  WifiUdpTransport` (single source). r2-esp deps r2-route/r2-transport/r2-tn.
  **devkitc `cargo check` (ESP32-S3) green** with the full dep graph.
- **Seam = core's, applied verbatim:** originate TTL=5, K=15 (FLOOD_SENTINEL —
  floods new dest, downgrades to Directed once a path is learned; NOT K=1=hold),
  full-u32 `target_hive` addressing, `source_hop=(id>>16)` INLINED (canonical
  r2-wire has no `compress_hive_id_16`; our vendored copy is forked — watch the
  re-sync), relay via `prepare_relay_extended`. Ref loop `r2-harness::MeshNode`.
- **DONE — firmware run-loop wired** (`069ed0e`): `r2_esp::tn::spawn(TnConfig)`
  (feature `tn`, off by default → production build byte-identical) binds
  `WifiUdpTransport` on the SoftAP IP, builds `McuNode`, seeds static peers,
  runs `poll`/`originate` on a thread. devkitc `tn_start()` derives my_hive_id
  (FNV of §6.2.1 hive_id UUID) + reads `R2_TN_PEER_ID/IP/PORT/ORIGINATE` via
  `option_env!`. **Verified ESP32-S3: `cargo check` default AND `--features tn`
  both green.**
- **DONE — DFR1195 carrier built + flash-ready** (`a0a1e1c`): `firmware/esp32-s3/dfr1195`,
  a minimal TN-only carrier. Boards on ttyACM9/10/11 probed via `espflash board-info`:
  **ESP32-S3, 4MB, no-PSRAM** (MACs f4:12:fa:b6:0a:a0 / 52:99:28 / 50:23:e4).
  Config: esp32s3 / FLASHSIZE_4MB / NO SPIRAM / 4MB 2-OTA table / r2-esp `tn`
  no-`ble`. **Board-hosted SoftAP + role-by-MAC** (ONE image): board whose MAC ==
  `R2_TN_AP_MAC` hosts the AP (`r2_esp::wifi_ap`, default SSID r2-tn-lab) + listens;
  others join STA + originate to the AP → STA→AP delivery = the hardware frame.
  Aligned to hive field.lab: UDP **21042**, matching partition table.
- **★ HARDWARE FRAME DELIVERED — TRUE TN A→B ON REAL SILICON (2026-06-20).**
  ttyACM9 (AP, f4:12:fa:b6:0a:a0) + ttyACM10 (STA, 52:99:28), role-by-MAC image.
  AP serial: **`[tn] DELIVERED ev=8127232d 8 bytes — HARDWARE FRAME`** every ~3s,
  matching STA `originated ev=8127232d -> next_hop 4767b7f3`. A real R2-WIRE frame
  routed STA→AP through core's RouteEngine over the WiFi radio (board-hosted SoftAP
  `r2-tn-lab`, STA joins → originates → AP delivers). Full routing tier end-to-end
  on hardware. Captured by firing the monitors detached + reading back the log
  (aws-bos keepalive held the link). Boards run the gateway-fix image (b5749be8);
  flash the canon+trust+relay blobs (124739a5/5490c063) for the trust + #19 demos.
- **BUG FOUND + FIXED (`9c5709f`, blob sha b5749be8):** STA targeted **192.168.4.1**
  (hive's *embassy-net* AP IP) but **esp-idf-svc's SoftAP is 192.168.71.1** → frames
  missed the AP. Fixed: `wifi_sta::get_gateway()` — STA targets its actual gateway
  (= the AP board) regardless of stack. Rebuilt + re-flashed both boards.
- **SPEC finding (route to specs):** the SoftAP/TN AP IP is **platform-stack
  dependent** (esp-idf-svc 192.168.71.1 vs embassy-net 192.168.4.1) — interop needs
  the AP IP discoverable (gateway), not assumed.
- **Link technique (flaky-Tailscale workaround):** can't hold a streaming SSH
  monitor; FIRE the monitors detached (`setsid nohup … espflash monitor … &`,
  return instantly like an echo) then READ the log back in a separate sub-second
  SSH. aws-bos keepalive helps connection setup.
- **NEXT (demos, when re-flashed via composer):** canon+trust+relay blobs
  (124739a5/5490c063) → trust-gate live + STA↔STA-via-AP relay (#19) + dedup/relay
  activity on the proof-surface. For 3 boards = 1 AP +
  2 STA; STA↔STA may be client-isolated → AP relays (RouteNode does).
- **TRUST TIER — RUNG 1 DONE (intra-TG)** (`1858410`): RouteNode `with_trust(my_tg, hk)`
  → originate signs every frame with `GroupHmac` (SHA256, sets target_group=
  FNV(tg_uuid) + 32B tag); DELIVER gated on `target_group==my_tg && verify_extended`
  (else Dropped wrong-TG/HMAC-fail); RELAY untouched = trust-agnostic (B1).
  r2-tn 11/11 host + **ESP32-S3 compiles** (r2-trust x25519/curve25519/chacha
  build for xtensa). r2-tn now deps r2-trust.
- **DONE — #19 mesh address-learning + STA↔STA-via-AP relay variant** (`146003f`):
  Node::poll learns sender hive_id→addr on recv (relay/route-back without static
  seed); dfr1195 RELAY variant (bake R2_TN_STA_A_MAC/B → STAs originate to each
  other, AP relays). Host test `ap_relays_sta_to_sta_with_address_learning` (real
  UDP, 3 Nodes). **Two blobs to composer:** default STA→AP `124739a5`, relay
  STA↔STA `5490c063` (one-image role-by-MAC; both have canon dedup + intra-TG
  trust + learning + OTA receiver). Supersede 343d1ab2.
- **DONE — C6 (RISC-V) TN carrier** (`b656083`): `firmware/esp32-c6/c6-tn`
  (riscv32imac-esp-espidf / esp32c6 / 4MB / no-PSRAM / no-BT / no-LoRa). Reuses
  the dfr1195 TN firmware verbatim (SoftAP + role-by-MAC + OTA + trust + relay) on
  the C6 target (dfr1117's .cargo/config + rust-toolchain). Builds + esp32c6
  merged-image validated. So 12-board mesh carriers covered: S3=dfr1195 blob (XIAO
  shares it, 8MB variant deferred) + C6=c6-tn. composer builds-from-source + bakes
  per-board MACs (I lack the C6 boards' MACs; can probe tuxedo if connected).
- **DONE — #18 build-side** (`146003f`): dfr1195 build.rs bakes
  `R2_FW_VER=<semver>+<sha>` + boot-logs it. #18 PULL = CMD_QUERY. #18 PUSH
  (`r2.hb.health`) awaits composer's HEALTH-TELEMETRY-CONTRACT (asked).
- **RUNG 2a — inter-TG PeeringHmac entanglement: DONE + RATIFIED-CANON-ALIGNED**
  (gate fix `<latest>`): per **specs R2-TRUST v0.7 §7.5.4** (ed7ffd6) + core.
  Deliver-gate = GroupHmac(my hk) FIRST → on fail trial-verify PeeringHmac per
  LIVE entanglement (verifying key identifies origin) → else Dropped("auth
  failed"). **NO E-flag** (R2-WIRE byte0 has no free bit; trial-verify is canon,
  zero wire change). `originate_cross(dest_tg)` sets **target_group=DEST** (origin
  hack removed). `entangle()`/`retire_entanglement()` (DROP-ON-RETIRE, HF-3).
  6 trust tests (intra deliver / wrong-key / cross entangled/not/retired/wrong-key
  + relay-agnostic); r2-tn 16/16 + ESP-compiles.
- **RUNG 2b (canon confidentiality) — PLANNED, with core for vetting:** add
  chacha20poly1305 to r2-tn; Entanglement gains `enc_key`; `originate_cross`
  encrypts payload=[nonce:24][XChaCha20Poly1305(enc_key,nonce,pt)] + signs
  PeeringHmac over hdr||nonce||ct; gate cross branch decrypts with the verifying
  entanglement's enc_key → deliver plaintext. nonce caller-supplied (esp_random/
  OsRng). intra GroupHmac stays plaintext+auth (group-DEK is a later rung). Real
  keys = lexicographically-ordered X25519 `derive_peering_keys`. (Doing it fresh —
  security-critical AEAD, not rushing at session tail.)
- **CANON firmware TODO (drop MAC hive_id shortcut):** dfr1195/c6-tn use
  FNV-of-MAC for `my_hive_id`; canon = FNV(§6.2.1 hive_id_uuid via
  `hive_id::load_identity`), TG-scoped. Requires TG identity provisioning in the
  TN firmware (the trust-provisioning rung). r2-tn host logic is hive_id-agnostic
  (unaffected). AP-IP: specs authoring R2-WIFI/R2-DISCOVERY "AP=gateway" rule —
  don't hardcode (I already read the gateway).
- **Trust-tier reference** (core TRUST-INTEGRATION-BRIEF @ r2-core 5f8798b; not in
  my workspace — relayed summary only). Entry points:
  - Gate ONLY at RouteNode's DELIVER branch (lib.rs ~234); relay stays
    trust-agnostic. deliverable = `dest_tg==mine ? verify_extended(msg, &GroupHmac(hk))
    : entangled(origin_tg) ? verify_extended(msg, &PeeringHmac(p.hmac)) : false`.
  - Crates: `r2-trust` (lifecycle::TrustGroup, cert::DeviceCertificate,
    wire_hmac::{GroupHmac,PeeringHmac} impl HmacProvider, hkdf derive_group/peering
    keys, revocation) + `r2-wire::hmac::{sign_extended, verify_extended}(msg, &impl
    HmacProvider)`. target_group (u32) is the TG discriminator in the ext header.
  - PREREQ: converge dfr1195 hive_id from MAC-shortcut to §6.2.1 (load_identity)
    so target_hive is TG-scoped (agreed canon with hive/specs).
  - Climb: (1) intra-TG GroupHmac deliver-gate; (2) inter-TG PeeringHmac
    entanglement (2 TGs × 2 boards); retire = drop peering key + buffered frames.
- **DONE — delivery-dedup** (`c64a354`): NOT a spec change (core: spec already
  answers it). RouteNode gates the whole message on `(msg_id, SOURCE)` first;
  SOURCE = route_stack[0] (R2-WIRE §3.3) else transport source for a direct frame.
  Separate DedupCache from the engine's relay-dedup. r2-tn 6/6 green + ESP-compiles.
- **DONE — DFR1195 network-OTA receiver + version query** (`d94a770`, Task #17/#18):
  dfr1195 main now calls `ota_tcp::start_listener()` (recv→verify sha→inactive
  slot→reboot; 2-OTA slots already in the table) + `ota_tcp::mark_app_valid()`
  (anti-brick on boot+WiFi+node self-proof) in both AP/STA branches. CMD_QUERY
  serves fw version (#18 PULL). **#18 PUSH pending composer's contract:** a periodic
  `r2.hb.health` (CBOR Compact: role/ip/fw_version/fw_sha/sync_state/link_q/transports)
  matching hive's shape — asked composer for exact fields/CBOR/cadence/transport;
  will add R2_GIT_SHA stamping to dfr1195 build.rs (like devkitc) for fw_sha.
  **Fleet blob refreshed → sha `343d1ab2…`** (canon dedup
  + OTA receiver; on Alfred + tuxedo /tmp/dfr1195-merged.bin; composer flashes it
  via esptool@0x0, role-by-MAC b60aa0=AP). Supersedes b5749be8.
- **Deferred refinement (core, scale-only, not urgent):** local DedupCache key is
  u16 source (matches r2-route's relay cache); for a many-node mesh use the FULL
  32-bit origin (~7.6% 16-bit collision @100 devices, spec-audit-2026-03-11) — a
  small own (msg_id, u32 source) ring; compress to 16b only when WRITING a compact
  route entry. Moot at 2-3 boards.
- **Interop-canon flagged to specs (spec-first):** (a) `target_hive` =
  FNV-1a(§6.2.1 hive_id_uuid) — TG-scoped, required at the trust tier; both
  workshop-dfr1195 (FNV full-MAC) + hive (MAC-low3) must converge from their
  shortcuts. (b) TN AP-IP must be discoverable ("AP = the gateway"), not constant.
- **Working mode:** any on-hardware divergence → refine SPEC first (specs), then code.
- **Doctrine:** [[fleet-operating-doctrine]] — branch experiment; not for `main`
  until proven on hardware. The OTA-gate fix (`699ee32`) is already on `main`.

## Roles
1. **Build & release owner.** Alfred (this `/remote-control workshop` session,
   host `Alfred`, x86_64, checkout `~/Development/R2/r2-workshop`) builds and
   releases all artefacts; **tuxedo-os runs the live hardware** (BLE/WiFi +
   sensors; live dashboard on `:21042` from `/mnt/data/...` — do NOT
   rebuild/restart it from Alfred). aarch64 server builds run on **pi5**
   (`royspi5`) over SSH.
2. **ESP32 firmware/build reference** for hive's general no_std MCU hive, and
   **own-hive web-UI reference** for composer's proof-UX plugin. Both reference
   notes committed (see below). Workshop firmware is **Path A (std/ESP-IDF)** —
   pattern/architecture reference, not no_std-portable code.

## Branch / state
- **Branch:** `main` (this repo commits direct-to-main per convention).
  `origin/main` HEAD: `265b378`. Working tree clean.
- **Build-box: fully provisioned & validated** — espup/esp toolchain installed,
  espflash 4.4.0 (matches tuxedo), wasm-pack installed. All proven:
  firmware ×3 (devkitc/xiao/dfr1117 build + package + slot-fit + sidecar),
  WASM hive (`webapp/pkg`), server x86_64 (Alfred) + aarch64 (pi5) tarballs.
- **Release streams formalized + FIRST CUT DONE:** firmware `fw-vX.Y.Z`,
  server `server-vX.Y.Z`, never combined. **`fw-v0.3.0` PUBLISHED** (6 assets,
  per-carrier .bin+sidecar). build.rs/build-firmware.sh select `--match 'fw-*'`
  + strip prefix; build-server.sh `--match 'server-*'` + strip; build-firmware
  now emits canonical `<slug>-<carrier>-<ver>` archive names. SPEC §13.3/§13.5
  + changelog 0.3.4. Gaps fixed during the cut: tag ambiguity (`d4b7060`),
  canonical naming (`265b378`).
- **SERVER STREAM HELD.** `server-v0.3.1` tag exists on origin but the release
  is NOT published — Roy authorized firmware only. I had briefly published it
  by mistake and deleted it. Do NOT publish a server release without an
  explicit version from Roy. (Server code is at 0.3.1.)

## OTA-revert diagnosis (2026-06-16) — RESOLVED as not-a-bug
Device 10.42.0.47 (devkitc, running **dev-build** 0.3.0) reverted after a cloud
OTA of release `fw-v0.3.0`. Forensics:
- Released asset `...rocker-devkitc-v0.3.0.bin` is **byte-perfect**: size 1530128
  + sha256 `871db034…` match the OTA-TCP log AND the sidecar. Not corruption.
- Valid image header: magic OK, ESP32-S3, 8MB, DIO/40MHz. Not a bad image.
- **No firmware commits since the `fw-v0.3.0` tag** → release image is
  logically IDENTICAL to the running dev build (only the version string differs).
- Anti-brick has **no rollback timer**; `mark_app_valid()` fires only on the
  **first `send_sample` after a dashboard TCP connect** (sender.rs:278).
- ∴ Revert ⟺ the new image rebooted *before* WiFi-connect → dashboard-connect →
  first frame. On the flaky-WiFi box this is the rollback working **as designed**,
  not an OTA bug. (Matches Roy's confirmed flaky-WiFi/BLE theory.)
- OTA of this release is also **functionally pointless** — identical firmware,
  cosmetic version string only.
- Design note for real upgrades (e.g. 0.3.0→0.4.0): gating mark-valid on a full
  dashboard round-trip makes OTA reliability hostage to network flakiness at the
  worst moment. Consider marking valid after the image self-proves (boot + WiFi
  assoc + self-test) rather than requiring an end-to-end dashboard frame — but
  that's a design change to discuss, low priority given supersede-by-composer.

## OTA validity-gate change (2026-06-16) — DRAFTED, hardware-unverified
Fix for the revert above + reusable contract for **composer's OTA plugin**
(Roy: workshop WILL be used in the field before composer is done, so this is a
real fix, not just reference). Moves the anti-brick gate off the dashboard.
- **Before:** `mark_app_valid` fired on the **first dashboard frame round-trip**
  → any single transient reset before that frame rolled back a *good* image.
- **After:** validate at the **streaming stage on LOCAL self-proof** (boot + all
  init + WiFi+DHCP + BLE + driver init all done in `Sender::new`), independent
  of dashboard reachability. ESP-IDF's own docs say mark "as early as possible."
- **Files:** shared `crates/r2-esp/src/ota_tcp.rs` gains `pub fn mark_app_valid()`
  (the reusable contract); each carrier's `sender.rs` (devkitc/xiao/dfr1117)
  replaces the old `mark_app_valid` method + `app_validated` field + loop gate
  with `confirm_image_valid()` called at top of `run()`; main.rs comments updated.
- **Trade-off (documented in code):** no longer rolls back an image that boots
  fine but has a broken dashboard-comms layer — that's an environment/CI concern,
  not an anti-brick concern. Residual: a deterministic panic in the steady-state
  send loop won't roll back (but that returns Err → reconnect, doesn't reset).
- **STATUS:** PUSHED to `origin/main` @ `699ee32` (2026-06-17). Compiles clean
  on all 3 carriers (S3 xtensa + C6 riscv), source-clean. **Source only — NOT
  released**; nothing fetches it until a `fw-v` release is cut, which must wait
  on hardware verification on tuxedo (currently OFF). **Verify when tuxedo is up:**
  flash → OTA-push it → confirm the new image marks valid right after WiFi/streaming
  (serial: "[ota-gate] streaming stage reached … validating image") and survives
  a reboot without rolling back. Only then cut `fw-v0.3.1` (or next).

## Next steps
0. **DEPLOYED to tuxedo @ `d88670a8` (2026-06-12).** Dashboard rebuilt + running
   the latest; `/api/version` correctly shows `d88670a8` clean (build.rs
   version-stamp fix validated). Per-session delete route live (200).
   **Awaiting Roy's verification:**
   - **Hard-refresh the browser (Ctrl-Shift-R)** to load the new webapp →
     confirm the "Sample at" dropdown holds its pick during recording, and
     the 🗑 Delete session button works (local + SD, offline tombstones).
   - **Power the sensors** → confirm the **flapping fix** (idle keepalive 30s→10s
     vs 15s read timeout): idle sensors should now stay connected instead of
     dropping every few seconds. (Deployed but untested — 0 sensors at deploy.)
1. **Server release — HELD.** When Roy gives an explicit version, cut
   `server-vX.Y.Z` (`build-server.sh` is ready; tag `server-v0.3.1` already
   exists if 0.3.1 is wanted). Do not publish without his go.
2. **Live tuxedo dashboard (`8125a18`) predates the list-walk fix (`d066ca1`)**
   — it queries `/releases/latest` (now a server tag, no firmware) so it won't
   see `fw-v0.3.0` until rebuilt to current `main`. Tuxedo-side update.
3. **DFR1195 (ESP32-S3) Path-B no_std build path** (forward dep, not urgent).
   New target NOT covered by the current Path-A/ESP-IDF matrix
   (devkitc/xiao/dfr1117). Pipeline: hive (no_std esp-hal/embassy source) →
   **workshop adds a no_std esp-hal build path + `fw-vX.Y.Z` release +
   OTA sidecars** → composer OTA push. hive coordinates when its scaffold
   compiles (hive branch `platform-trait`).
4. **Demo guard** `.tg_pub_demo_sha256` — deferred (needs upstream demo TG key
   hash; dormant for the rocker since its TG key is real).
5. Watch the **vendored core/hive crate re-sync** (upstream churn) before any
   protocol-touching work.

## Deps / peers (branches per supervisor)
- hive = `platform-trait`; composer = `phase-3-hardware-tier`.
- Chain: specs → core (no_std spec-impl) → hive (crates + platform layers).
  composer orchestrates hives (fleets/plugins/ensembles/OTA/proof-UX); it is
  NOT the hive. North-star: ONE hive codebase everywhere.

## Key references
- `docs/esp32-firmware-build-reference.md` — toolchain, firmware structure,
  `crates/r2-esp` platform-layer modules, Path A vs B honesty (`b2728a5`).
- `docs/own-hive-web-ui-recipe.md` — two-hive split, `registrations.r2-web`,
  reuse vs rebuild (`fb8da79`).
- Memory: `north-star-one-hive-codebase`, `build-release-roles-alfred-tuxedo`,
  `server-vs-firmware-release-streams`, `workshop-superseded-by-composer`.

## Safety (mandatory)
- Never `git add -A` / `git add .`; stage named files only (`git add -u` +
  named new files). No secrets. Public repo `reality2-ai.github.io` gets NO
  internal/fleet context.
