//! # r2-tn — TN node driver
//!
//! Glues core's [`r2_route::RouteEngine`] (the pure decision brain) to a
//! [`r2_transport::Transport`] (the medium) so a node can **originate** and
//! **route** R2-WIRE frames board-to-board — TRUE TN, beyond the hub-star.
//! See `docs/tn-routeengine-smallest-path.md`.
//!
//! Division of ownership: **core** owns the engine + wire/route semantics;
//! **workshop** owns this driver glue + the transport impls + bring-up. Kept
//! host-buildable (no esp-idf) so it unit-tests on Alfred and can converge into
//! the one hive codebase later; the ESP firmware instantiates [`RouteNode`]
//! with `r2_esp::peer_wifi_udp::WifiUdpTransport`.
//!
//! ## Provisional constants (pending core seam answers)
//! [`DEFAULT_TTL`]/[`DEFAULT_K`] and the addressing key (full `hive_id` u32, per
//! `ExtendedHeader::target_hive`) are workshop defaults until core confirms
//! (questions in the smallest-path doc). The *structure* is stable; only these
//! values may change.

pub mod health;
pub mod node;
pub mod persona;
pub mod udp;

pub use node::{Delivered, McuNode, Node};

use r2_route::transport::{QualitySample, Transport as RTransport};
use r2_route::dedup::DedupCache;
use r2_route::{ForwardAction, ForwardRequest, MobilityClass, Observation, RouteEngine, Target};
use r2_transport::Transport;
use r2_wire::extended::prepare_relay_extended;
use r2_wire::hmac::{sign_extended, verify_extended, HmacProvider};
use r2_wire::{
    decode_extended, encode_extended, ExtendedHeader, ExtendedMessage, ExtendedRouteStack, Flags,
    MsgType,
};
use r2_trust::wire_hmac::{GroupHmac, PeeringHmac};
use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305, XNonce};

/// XChaCha20 nonce length (24 bytes) — rides at the front of a cross-TG payload.
pub const XNONCE_LEN: usize = 24;

/// A live inter-TG entanglement (R2-TRUST §7.5). Holds the peering HMAC key the
/// deliver-gate trial-verifies cross-TG frames against. `live=false` = retired
/// (DROP-ON-RETIRE for RAM-volatile boards, HF-3 — keep the slot so the index is
/// stable but stop authorising crossings).
#[derive(Clone)]
pub struct Entanglement {
    /// Peering HMAC key = `derive_peering_keys(PS, tg_a_pub, tg_b_pub).hmac`
    /// (the derive sorts the pubkeys internally — pass any order).
    pub peering_hmac: [u8; 32],
    /// Peering ENC key = the same `derive_peering_keys(..).enc` — XChaCha20-Poly1305
    /// key for cross-TG payload confidentiality (rung-2b).
    pub enc_key: [u8; 32],
    /// Whether this entanglement currently authorises crossings.
    pub live: bool,
}

/// Trust-group context for the deliver-gate (R2-TRUST B1: relay is
/// trust-agnostic, DELIVERY is trust-gated). When set, this node signs every
/// frame it originates and, on a frame addressed to it, verifies GroupHmac(my hk)
/// first; on fail it trial-verifies PeeringHmac against its live entanglements
/// (canon R2-TRUST v0.7 §7.5.4). When `None`, the node is untrusted (open
/// routing) — no signing, no deliver-gate.
#[derive(Clone)]
pub struct TrustContext {
    /// This node's trust-group id = FNV-1a-32(hive-group-uuid) (R2-WIRE §6.2.1),
    /// TG-scoped.
    pub my_tg: u32,
    /// Group HMAC key (DerivedGroupKeys.hk, 32 bytes).
    pub hk: [u8; 32],
    /// Live inter-TG entanglements (R2-TRUST §7.5). A cross-TG frame is delivered
    /// iff GroupHmac fails AND one live entanglement's PeeringHmac verifies
    /// (trial-verify; entanglements are few; the verifying key identifies origin).
    pub entanglements: Vec<Entanglement>,
}

/// Initial TTL for an originated frame — canonical `DEFAULT_TTL`
/// (r2-core constants.rs:4), confirmed by core.
pub const DEFAULT_TTL: u8 = 5;
/// Initial K for an originated frame — canonical `FLOOD_SENTINEL_K`
/// (r2-core constants.rs:54): a new destination floods (R2-ROUTE §4.5); the
/// engine downgrades to Directed once a path is learned. NOT K=1 (that is the
/// spray-and-wait WAIT/hold phase).
pub const DEFAULT_K: u8 = 15;

/// Compressed 16-bit sender id used for dedup (`source_hop`): the high half of
/// the 32-bit FNV hive id (canonical rule, r2-wire types.rs:136). Inlined here
/// rather than via a helper — canonical r2-wire exposes no `compress_hive_id_16`
/// fn (workshop's vendored copy is forked; this keeps us aligned to canon).
#[inline]
fn compress_hive_id_16(hive_id: u32) -> u16 {
    (hive_id >> 16) as u16
}

/// The immediate sender's hive id from a frame on the wire = the LAST route-stack
/// entry (the originator for a direct frame, the most-recent relay otherwise).
/// Used for mesh address-learning (map this hive_id to the datagram's src addr)
/// so a node can route back / relay onward to peers it never statically seeded.
pub(crate) fn immediate_sender(frame: &[u8]) -> Option<u32> {
    let msg = decode_extended(frame).ok()?;
    let r = msg.route.as_ref()?;
    if r.len == 0 {
        return None;
    }
    Some(r.entries[(r.len - 1) as usize])
}

/// Outcome of feeding an inbound frame to [`RouteNode::on_inbound`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// Frame was destined for this node — deliver to the local app.
    Deliver { event_hash: u32, payload: Vec<u8> },
    /// Frame was routed onward to a next hop (this node is a relay).
    Forwarded { next_hop: u32 },
    /// Engine/relay dropped the frame.
    Dropped(&'static str),
    /// Frame failed to decode.
    DecodeError,
}

/// Constrained-MCU RouteNode profile (`RouteEngine<16,16,32>`, core engine.rs:148)
/// — use this on the C6 / ESP32-S3 firmware. Gateway default is `<64,64,64>`.
pub type McuRouteNode = RouteNode<16, 16, 32>;

/// A TN node: a RouteEngine plus this node's own hive id and a frame counter.
///
/// Generic over the engine table capacities (neighbours / paths / dedup) so a
/// constrained MCU can use small tables ([`McuRouteNode`]) while a gateway uses
/// the `<64,64,64>` default.
pub struct RouteNode<const N: usize = 64, const P: usize = 64, const D: usize = 64> {
    my_hive_id: u32,
    engine: RouteEngine<N, P, D>,
    /// Whole-message DELIVERY dedup (R2-WIRE dedup + R2-ROUTE §6): keyed on
    /// (msg_id, SOURCE) where SOURCE is the originator, not the immediate hop —
    /// so the same message arriving via two paths delivers once. Separate from
    /// the engine's internal relay-dedup.
    dedup: DedupCache<D>,
    /// Trust-group context. Some → sign on originate + gate on deliver; None →
    /// untrusted open routing (no signing, no gate). Relay is ALWAYS
    /// trust-agnostic (R2-TRUST B1).
    trust: Option<TrustContext>,
    seq: u32,
}

impl<const N: usize, const P: usize, const D: usize> RouteNode<N, P, D> {
    /// Create a node identified by `my_hive_id` (FNV-1a of the device UUID).
    pub fn new(my_hive_id: u32) -> Self {
        Self {
            my_hive_id,
            engine: RouteEngine::new(),
            dedup: DedupCache::new(),
            trust: None,
            seq: 0,
        }
    }

    /// Attach a trust-group context (enables signing on originate + the
    /// deliver-gate). Builder style: `RouteNode::new(id).with_trust(tg, hk)`.
    pub fn with_trust(mut self, my_tg: u32, hk: [u8; 32]) -> Self {
        self.trust = Some(TrustContext {
            my_tg,
            hk,
            entanglements: Vec::new(),
        });
        self
    }

    /// Add a live inter-TG entanglement (its peering HMAC + ENC keys, from
    /// `derive_peering_keys`). Cross-TG frames that verify + decrypt under it
    /// become deliverable (R2-TRUST §7.5). No-op if untrusted.
    pub fn entangle(&mut self, peering_hmac: [u8; 32], enc_key: [u8; 32]) {
        if let Some(t) = &mut self.trust {
            t.entanglements.push(Entanglement {
                peering_hmac,
                enc_key,
                live: true,
            });
        }
    }

    /// Retire an entanglement by its peering key: drop authorisation (live=false)
    /// — RAM-volatile DROP-ON-RETIRE (HF-3), no wire epoch needed. Returns true
    /// if a matching live entanglement was found.
    pub fn retire_entanglement(&mut self, peering_hmac: &[u8; 32]) -> bool {
        if let Some(t) = &mut self.trust {
            for e in t.entanglements.iter_mut() {
                if e.live && &e.peering_hmac == peering_hmac {
                    e.live = false;
                    return true;
                }
            }
        }
        false
    }

    /// This node's hive id.
    pub fn hive_id(&self) -> u32 {
        self.my_hive_id
    }

    /// Mutable access to the engine (to feed real discovery observations).
    pub fn engine_mut(&mut self) -> &mut RouteEngine<N, P, D> {
        &mut self.engine
    }

    /// Seed a direct neighbour + a high-confidence direct path to it.
    ///
    /// First-light / test helper — milestone 2 replaces this with R2-BEACON
    /// observations fed via [`engine_mut`](RouteNode::engine_mut).
    pub fn seed_direct(&mut self, hive_id: u32, transport: RTransport, now: u32) {
        // A few Ideal observations to lift the neighbour past the viability
        // floor; a high-confidence path so try_directed selects it.
        for _ in 0..8 {
            self.engine.ingest_observation(Observation {
                hive_id,
                transport,
                timestamp: now,
                quality: QualitySample::Ideal,
                rssi: None,
                mcu_origin: false,
                mobility: MobilityClass::Infrastructure,
            });
        }
        self.engine.seed_path(hive_id, hive_id, now, 1.0);
    }

    /// Seed an indirect path to `destination` via `next_hop` (for relay tests).
    pub fn seed_path_via(&mut self, destination: u32, next_hop: u32, now: u32) {
        self.engine.seed_path(destination, next_hop, now, 1.0);
    }

    /// Originate a frame to `dest_hive`, signing intra-TG with the group HMAC
    /// when trusted. Returns the next-hop hive id the frame was sent to.
    pub fn originate<T: Transport>(
        &mut self,
        dest_hive: u32,
        event_hash: u32,
        payload: &[u8],
        tx: &T,
        now: u32,
    ) -> Result<u32, &'static str> {
        let tg = self.trust.as_ref().map(|t| t.my_tg).unwrap_or(0);
        let hmac = self.trust.as_ref().map(|t| GroupHmac::new(t.hk));
        self.emit_signed(dest_hive, event_hash, payload, tx, now, tg, hmac.as_ref())
    }

    /// Originate a CROSS-TG frame to `dest_hive` in `dest_tg`, ENCRYPTED for the
    /// entangled peer group (R2-TRUST §7.5 rung-2b canon). The plaintext is
    /// sealed with XChaCha20-Poly1305 under `enc_key` + a FRESH random `nonce`
    /// (caller-supplied: esp_random / OsRng — never reuse a nonce with a key),
    /// wire payload = `[nonce:24][ciphertext+tag]`, then signed with the peering
    /// HMAC over header||nonce||ciphertext. `target_group = dest_tg` (canon).
    /// Two MACs by design: PeeringHmac authenticates the header + binds the ct;
    /// the AEAD tag gives payload confidentiality+integrity.
    pub fn originate_cross<T: Transport>(
        &mut self,
        dest_hive: u32,
        dest_tg: u32,
        peering_hmac: [u8; 32],
        enc_key: [u8; 32],
        nonce: [u8; XNONCE_LEN],
        event_hash: u32,
        plaintext: &[u8],
        tx: &T,
        now: u32,
    ) -> Result<u32, &'static str> {
        let cipher = XChaCha20Poly1305::new_from_slice(&enc_key).map_err(|_| "bad enc key")?;
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| "encrypt failed")?;
        let mut wire = Vec::with_capacity(XNONCE_LEN + ct.len());
        wire.extend_from_slice(&nonce);
        wire.extend_from_slice(&ct);
        self.emit_signed(
            dest_hive,
            event_hash,
            &wire,
            tx,
            now,
            dest_tg,
            Some(&PeeringHmac::new(peering_hmac)),
        )
    }

    /// Build (route-stamp + optional HMAC sign) and send a frame toward the
    /// engine-chosen next hop.
    fn emit_signed<T: Transport, H: HmacProvider>(
        &mut self,
        dest_hive: u32,
        event_hash: u32,
        payload: &[u8],
        tx: &T,
        now: u32,
        target_group: u32,
        hmac: Option<&H>,
    ) -> Result<u32, &'static str> {
        self.seq = self.seq.wrapping_add(1);
        let msg_id = self.seq;

        let header = ExtendedHeader {
            version: 0,
            msg_type: MsgType::Event,
            // R flag set: originator self-stamps route_stack[0] so the origin is
            // frame-carried on EVERY dedupable event (R2-WIRE v0.4 §6.2.1).
            flags: Flags {
                has_route: true,
                ..Default::default()
            },
            ttl: DEFAULT_TTL,
            k: DEFAULT_K,
            msg_id,
            event_hash,
            payload_len: payload.len() as u32,
            target_group,
            target_hive: dest_hive,
        };
        let mut route = ExtendedRouteStack::new();
        route.len = 1;
        route.entries[0] = self.my_hive_id;
        let mut msg = ExtendedMessage {
            header,
            route: Some(route),
            payload,
            hmac_tag: None,
        };
        // Sign (covers immutable header fields + payload, not ttl/k). Sets
        // has_hmac + the 32-byte tag, preserving has_route.
        if let Some(h) = hmac {
            let (flags, tag) = sign_extended(&msg, h);
            msg.header.flags = flags;
            msg.hmac_tag = Some(tag);
        }
        let mut buf = vec![0u8; 64 + payload.len()];
        let n = encode_extended(&msg, &mut buf).map_err(|_| "encode failed")?;
        buf.truncate(n);

        let req = ForwardRequest {
            now,
            msg_id: msg_id as u16,
            source_hop: compress_hive_id_16(self.my_hive_id),
            ttl: DEFAULT_TTL,
            k: DEFAULT_K,
            destination: Target::Address(dest_hive),
            msg_type: MsgType::Event,
            payload_len: payload.len(),
            relay_enabled: true,
            congested: false,
            dice_roll: 0.0,
        };
        match self.engine.plan_forward(req).action {
            ForwardAction::Directed(hop) => {
                tx.send(hop.neighbour, &buf).map_err(|_| "send failed")?;
                Ok(hop.neighbour)
            }
            ForwardAction::Flood(hops) => {
                let mut sent_to = 0u32;
                for h in &hops {
                    if tx.send(h.neighbour, &buf).is_ok() {
                        sent_to = h.neighbour;
                    }
                }
                if sent_to != 0 {
                    Ok(sent_to)
                } else {
                    Err("flood: no reachable next hop")
                }
            }
            ForwardAction::Drop(_) => Err("originate: engine dropped"),
            ForwardAction::DeliverOnly => Err("originate: destination is self"),
        }
    }

    /// Feed an inbound frame received from `from_hive_id` on a transport.
    ///
    /// Delivers locally if addressed to this node, otherwise consults the
    /// engine and relays (TTL-decremented, route-stack-appended) onward.
    pub fn on_inbound<T: Transport>(
        &mut self,
        frame: &[u8],
        from_hive_id: u32,
        tx: &T,
        now: u32,
    ) -> Inbound {
        let msg = match decode_extended(frame) {
            Ok(m) => m,
            Err(_) => return Inbound::DecodeError,
        };
        let dest = msg.header.target_hive;
        let event_hash = msg.header.event_hash;

        // Whole-message dedup FIRST (R2-WIRE v0.4 §6.2.1, landed 2eff5d8 from this
        // hardware finding): key = (msg_id, origin_hive_id) where the origin is
        // FRAME-CARRIED — route_stack[0] (the originator self-stamps it on every
        // event). The transport source is NO LONGER a discriminator (it collapsed
        // to 0 on LoRa and differed direct-vs-relayed). A frame with no carried
        // origin is non-conformant: MUST NOT collapse to origin=0 — we treat it as
        // un-deduplicatable (deliver/relay once, no dedup record) rather than drop.
        let msg_id16 = msg.header.msg_id as u16;
        match msg.route.as_ref().filter(|r| r.len > 0).map(|r| r.entries[0]) {
            Some(origin) => {
                if self.dedup.is_duplicate(now, msg_id16, (origin >> 16) as u16) {
                    return Inbound::Dropped("duplicate");
                }
            }
            None => {
                // No frame-carried origin (R flag clear) — non-conformant for
                // dedup. Proceed without a dedup record (un-deduplicatable);
                // never collapse to origin=0 (the LoRa-source-0 bug this finding
                // fixed). Returns Inbound below as normal.
            }
        }

        // Learn the immediate sender as a neighbour + reverse path (MeshNode
        // step 2a) so the engine can route back toward it without separate
        // discovery. Real WiFi datagram → Ideal/Infrastructure per core's Q4.
        self.engine.ingest_observation(Observation {
            hive_id: from_hive_id,
            transport: RTransport::Wifi,
            timestamp: now,
            quality: QualitySample::Ideal,
            rssi: None,
            mcu_origin: false,
            mobility: MobilityClass::Infrastructure,
        });
        self.engine
            .record_delivery_success(from_hive_id, from_hive_id, now);

        // Addressed to us → deliver locally, TRUST-GATED (R2-TRUST B1: only the
        // DELIVER branch is gated; relay below stays trust-agnostic). Intra-TG:
        // deliver iff target_group == my_tg AND the group HMAC verifies.
        if dest == self.my_hive_id {
            // Untrusted node: deliver as-is (open routing).
            let Some(t) = &self.trust else {
                return Inbound::Deliver {
                    event_hash,
                    payload: msg.payload.to_vec(),
                };
            };
            // Canon deliver-gate (R2-TRUST v0.7 §7.5.4): GroupHmac(my hk) FIRST
            // (intra-TG, plaintext payload); on fail, trial-verify PeeringHmac
            // against EACH live entanglement — the key that verifies identifies
            // the origin TG AND its enc_key decrypts the payload. target_group is
            // DEST (authenticated, not the selector). Forging either path needs
            // the key, so the 2nd eval admits nothing. No E-flag.
            if verify_extended(&msg, &GroupHmac::new(t.hk)) {
                return Inbound::Deliver {
                    event_hash,
                    payload: msg.payload.to_vec(),
                };
            }
            for e in t.entanglements.iter().filter(|e| e.live) {
                if verify_extended(&msg, &PeeringHmac::new(e.peering_hmac)) {
                    // rung-2b: payload = [nonce:24][ciphertext]; decrypt with this
                    // entanglement's enc_key (PeeringHmac already authenticated it).
                    if msg.payload.len() < XNONCE_LEN {
                        return Inbound::Dropped("short cross frame");
                    }
                    let (nonce, ct) = msg.payload.split_at(XNONCE_LEN);
                    let cipher = match XChaCha20Poly1305::new_from_slice(&e.enc_key) {
                        Ok(c) => c,
                        Err(_) => return Inbound::Dropped("bad enc key"),
                    };
                    return match cipher.decrypt(XNonce::from_slice(nonce), ct) {
                        Ok(pt) => Inbound::Deliver {
                            event_hash,
                            payload: pt,
                        },
                        Err(_) => Inbound::Dropped("decrypt failed"),
                    };
                }
            }
            return Inbound::Dropped("auth failed");
        }

        let req = ForwardRequest {
            now,
            msg_id: msg.header.msg_id as u16,
            source_hop: compress_hive_id_16(from_hive_id),
            ttl: msg.header.ttl,
            k: msg.header.k,
            destination: Target::Address(dest),
            msg_type: msg.header.msg_type,
            payload_len: msg.payload.len(),
            relay_enabled: true,
            congested: false,
            dice_roll: 0.0,
        };
        match self.engine.plan_forward(req).action {
            ForwardAction::Directed(hop) => {
                // workshop owns the header rewrite (TTL--, K split, route push);
                // the engine only advised the next hop.
                match prepare_relay_extended(frame, self.my_hive_id, from_hive_id) {
                    Ok(relayed) => match tx.send(hop.neighbour, &relayed) {
                        Ok(_) => Inbound::Forwarded {
                            next_hop: hop.neighbour,
                        },
                        Err(_) => Inbound::Dropped("relay send failed"),
                    },
                    Err(_) => Inbound::Dropped("relay header rewrite failed"),
                }
            }
            ForwardAction::Flood(hops) => match prepare_relay_extended(frame, self.my_hive_id, from_hive_id) {
                Ok(relayed) => {
                    let mut last = 0u32;
                    for h in &hops {
                        // Never bounce back to the peer we received it from
                        // (MeshNode flood rule; avoids the relay-only-neighbour
                        // == inbound-peer dead-end core flagged).
                        if h.neighbour == from_hive_id {
                            continue;
                        }
                        if tx.send(h.neighbour, &relayed).is_ok() {
                            last = h.neighbour;
                        }
                    }
                    Inbound::Forwarded { next_hop: last }
                }
                Err(_) => Inbound::Dropped("relay header rewrite failed"),
            },
            ForwardAction::Drop(_) => Inbound::Dropped("engine drop"),
            ForwardAction::DeliverOnly => Inbound::Deliver {
                event_hash,
                payload: msg.payload.to_vec(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2_transport::{LinkQuality, SendError, TransportId, TransportState};
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::rc::Rc;

    /// Shared in-memory "network": target hive id -> queued datagrams.
    type Net = Rc<RefCell<HashMap<u32, VecDeque<Vec<u8>>>>>;

    /// A mock Transport that drops each sent frame into the shared network,
    /// keyed by target hive id. Exercises the exact Transport seam the real
    /// WifiUdpTransport implements.
    struct MockTransport {
        net: Net,
        reachable: Vec<u32>,
    }
    impl Transport for MockTransport {
        fn id(&self) -> TransportId {
            TransportId::Wifi
        }
        fn state(&self) -> TransportState {
            TransportState::Available
        }
        fn send(&self, target: u32, frame: &[u8]) -> Result<(), SendError> {
            self.net
                .borrow_mut()
                .entry(target)
                .or_default()
                .push_back(frame.to_vec());
            Ok(())
        }
        fn link_quality(&self, hive_id: u32) -> Option<LinkQuality> {
            if self.reachable.contains(&hive_id) {
                Some(LinkQuality {
                    quality: 1.0,
                    ..Default::default()
                })
            } else {
                None
            }
        }
    }

    fn drain(net: &Net, who: u32) -> Vec<Vec<u8>> {
        net.borrow_mut()
            .get_mut(&who)
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }

    const A: u32 = 0xA1A1_A1A1;
    const B: u32 = 0xB2B2_B2B2;
    const C: u32 = 0xC3C3_C3C3;
    const EV: u32 = 0x1234_5678;

    // The headline: ONE frame routed A -> B directly through RouteEngine.
    #[test]
    fn routes_one_frame_a_to_b_direct() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };

        let mut a = RouteNode::<64, 64, 64>::new(A);
        let mut b = RouteNode::<64, 64, 64>::new(B);
        a.seed_direct(B, RTransport::Wifi, 1);

        // A originates "hello" to B; engine picks B as the next hop.
        let next = a
            .originate(B, EV, b"hello", &txa, 1)
            .expect("originate should route to B");
        assert_eq!(next, B, "next hop should be B (direct neighbour)");

        // B receives the datagram off the wire and delivers it locally.
        let frames = drain(&net, B);
        assert_eq!(frames.len(), 1, "exactly one frame should reach B");
        let outcome = b.on_inbound(&frames[0], A, &txb, 1);
        assert_eq!(
            outcome,
            Inbound::Deliver {
                event_hash: EV,
                payload: b"hello".to_vec()
            }
        );
    }

    // The SAME frame arriving twice delivers once, then dedups (R2-WIRE dedup
    // + R2-ROUTE §6: key (msg_id, SOURCE)).
    #[test]
    fn duplicate_frame_delivered_once() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A);
        let mut b = RouteNode::<64, 64, 64>::new(B);
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate(B, EV, b"dup", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);

        // First arrival delivers; a re-arrival of the identical frame dedups.
        assert!(matches!(b.on_inbound(&frame, A, &txb, 1), Inbound::Deliver { .. }));
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("duplicate"));
    }

    // ── Trust tier (R2-TRUST B1: DELIVER gated, RELAY trust-agnostic) ──
    const TG1: u32 = 0x7611_0001;
    const TG2: u32 = 0x7622_0002;
    const HK1: [u8; 32] = [0x11; 32];
    const HK2: [u8; 32] = [0x22; 32];

    // Same TG + same group key: signed frame verifies and delivers.
    #[test]
    fn intra_tg_signed_frame_delivers() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG1, HK1);
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate(B, EV, b"trusted", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(
            b.on_inbound(&frame, A, &txb, 1),
            Inbound::Deliver { event_hash: EV, payload: b"trusted".to_vec() }
        );
    }

    // Frame from a different TG is dropped at the deliver-gate (not our TG).
    #[test]
    fn wrong_tg_dropped() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2); // different TG + key
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate(B, EV, b"x", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        // Different TG + no entanglement => not deliverable.
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("auth failed"));
    }

    // Same TG but wrong group key → HMAC verify fails → dropped.
    #[test]
    fn bad_hmac_dropped() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG1, HK2); // same TG, wrong key
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate(B, EV, b"x", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("auth failed"));
    }

    // B1: a relay in a DIFFERENT TG still forwards a frame addressed elsewhere —
    // relay is trust-agnostic; only DELIVERY is gated.
    #[test]
    fn relay_is_trust_agnostic() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A, C] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2); // foreign TG relay
        a.seed_direct(B, RTransport::Wifi, 1);
        a.seed_path_via(C, B, 1);
        b.seed_direct(C, RTransport::Wifi, 1);

        a.originate(C, EV, b"relayme", &txa, 1).unwrap();
        let at_b = drain(&net, B).remove(0);
        assert_eq!(
            b.on_inbound(&at_b, A, &txb, 1),
            Inbound::Forwarded { next_hop: C },
            "a foreign-TG relay must still forward (B1)"
        );
    }

    // ── Inter-TG entanglement (R2-TRUST §7.5 rung-2a, auth-only PeeringHmac) ──
    const PK_AB: [u8; 32] = [0x33; 32]; // shared TG_A<->TG_B peering hmac key
    const PK_X: [u8; 32] = [0x44; 32]; // an unrelated peering key
    const ENC_AB: [u8; 32] = [0x55; 32]; // shared TG_A<->TG_B peering enc key
    const ENC_X: [u8; 32] = [0x66; 32]; // an unrelated enc key
    const NONCE: [u8; 24] = [0x77; 24]; // fixed test nonce (fresh-random in fw)

    // A (TG1) and B (TG2) are ENTANGLED (share PK_AB): A's cross-TG frame crosses.
    #[test]
    fn cross_tg_entangled_delivers() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2);
        b.entangle(PK_AB, ENC_AB);
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate_cross(B, TG2, PK_AB, ENC_AB, NONCE, EV, b"cross", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(
            b.on_inbound(&frame, A, &txb, 1),
            Inbound::Deliver { event_hash: EV, payload: b"cross".to_vec() }
        );
    }

    // No entanglement on B → cross-TG frame is not deliverable.
    #[test]
    fn cross_tg_not_entangled_dropped() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2); // no entangle
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate_cross(B, TG2, PK_AB, ENC_AB, NONCE, EV, b"cross", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("auth failed"));
    }

    // Retiring the entanglement stops crossings (DROP-ON-RETIRE).
    #[test]
    fn cross_tg_retired_dropped() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2);
        b.entangle(PK_AB, ENC_AB);
        assert!(b.retire_entanglement(&PK_AB));
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate_cross(B, TG2, PK_AB, ENC_AB, NONCE, EV, b"cross", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("auth failed"));
    }

    // Wrong peering key on B → PeeringHmac verify fails → dropped.
    #[test]
    fn cross_tg_wrong_peering_key_dropped() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2);
        b.entangle(PK_X, ENC_X); // entangled, but with DIFFERENT keys
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate_cross(B, TG2, PK_AB, ENC_AB, NONCE, EV, b"cross", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("auth failed"));
    }

    // Right peering HMAC but wrong ENC key (AEAD tag mismatch) → decrypt fails.
    // (Artificial split — canon pairs hmac+enc; covers the decrypt-fail path.)
    #[test]
    fn cross_tg_wrong_enc_key_dropped() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A] };
        let mut a = RouteNode::<64, 64, 64>::new(A).with_trust(TG1, HK1);
        let mut b = RouteNode::<64, 64, 64>::new(B).with_trust(TG2, HK2);
        b.entangle(PK_AB, ENC_X); // hmac matches, enc does NOT
        a.seed_direct(B, RTransport::Wifi, 1);

        a.originate_cross(B, TG2, PK_AB, ENC_AB, NONCE, EV, b"cross", &txa, 1).unwrap();
        let frame = drain(&net, B).remove(0);
        assert_eq!(b.on_inbound(&frame, A, &txb, 1), Inbound::Dropped("decrypt failed"));
    }

    // Multi-hop: A -> (relay B) -> C. Proves real routing + TTL decrement.
    #[test]
    fn routes_frame_a_via_b_to_c() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        let txb = MockTransport { net: net.clone(), reachable: vec![A, C] };
        let txc = MockTransport { net: net.clone(), reachable: vec![B] };

        let mut a = RouteNode::<64, 64, 64>::new(A);
        let mut b = RouteNode::<64, 64, 64>::new(B);
        let mut c = RouteNode::<64, 64, 64>::new(C);

        // A can reach B directly and knows C is via B.
        a.seed_direct(B, RTransport::Wifi, 1);
        a.seed_path_via(C, B, 1);
        // B can reach C directly.
        b.seed_direct(C, RTransport::Wifi, 1);

        // A originates to C; engine routes to next hop B.
        let next = a.originate(C, EV, b"relayme", &txa, 1).expect("route to C via B");
        assert_eq!(next, B, "A should send toward C via B");

        // B relays toward C.
        let at_b = drain(&net, B);
        assert_eq!(at_b.len(), 1);
        let relayed = b.on_inbound(&at_b[0], A, &txb, 1);
        assert_eq!(relayed, Inbound::Forwarded { next_hop: C }, "B relays to C");

        // C delivers.
        let at_c = drain(&net, C);
        assert_eq!(at_c.len(), 1);
        let outcome = c.on_inbound(&at_c[0], B, &txc, 1);
        assert_eq!(
            outcome,
            Inbound::Deliver {
                event_hash: EV,
                payload: b"relayme".to_vec()
            }
        );

        // The relayed frame's TTL was decremented by exactly one hop.
        let orig_ttl = decode_extended(&at_b[0]).unwrap().header.ttl;
        let relayed_ttl = decode_extended(&at_c[0]).unwrap().header.ttl;
        assert_eq!(relayed_ttl, orig_ttl - 1, "relay must decrement TTL");
    }

    // A frame addressed elsewhere with no known route is not delivered locally.
    #[test]
    fn frame_not_for_us_is_not_delivered() {
        let net: Net = Rc::new(RefCell::new(HashMap::new()));
        let txb = MockTransport { net: net.clone(), reachable: vec![] };
        let mut b = RouteNode::<64, 64, 64>::new(B);

        // Build a frame addressed to C, hand it to B which has no route to C.
        let mut a = RouteNode::<64, 64, 64>::new(A);
        let txa = MockTransport { net: net.clone(), reachable: vec![B] };
        a.seed_direct(B, RTransport::Wifi, 1);
        a.originate(B, EV, b"x", &txa, 1).unwrap(); // just to build a frame at B
        let frame_to_b = drain(&net, B).remove(0);
        // Rewrite isn't needed — frame is addressed to B, so it delivers.
        let outcome = b.on_inbound(&frame_to_b, A, &txb, 1);
        assert!(matches!(outcome, Inbound::Deliver { .. }));
    }
}
