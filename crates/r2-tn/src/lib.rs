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

pub mod udp;

use r2_route::transport::{QualitySample, Transport as RTransport};
use r2_route::{ForwardAction, ForwardRequest, MobilityClass, Observation, RouteEngine, Target};
use r2_transport::Transport;
use r2_wire::extended::prepare_relay_extended;
use r2_wire::{decode_extended, encode_extended, ExtendedHeader, ExtendedMessage, Flags, MsgType};

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
    seq: u32,
}

impl<const N: usize, const P: usize, const D: usize> RouteNode<N, P, D> {
    /// Create a node identified by `my_hive_id` (FNV-1a of the device UUID).
    pub fn new(my_hive_id: u32) -> Self {
        Self {
            my_hive_id,
            engine: RouteEngine::new(),
            seq: 0,
        }
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

    /// Originate a frame to `dest_hive` and send it toward the chosen next hop.
    ///
    /// Returns the next-hop hive id the frame was sent to.
    pub fn originate<T: Transport>(
        &mut self,
        dest_hive: u32,
        event_hash: u32,
        payload: &[u8],
        tx: &T,
        now: u32,
    ) -> Result<u32, &'static str> {
        self.seq = self.seq.wrapping_add(1);
        let msg_id = self.seq;

        let header = ExtendedHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: DEFAULT_TTL,
            k: DEFAULT_K,
            msg_id,
            event_hash,
            payload_len: payload.len() as u32,
            target_group: 0,
            target_hive: dest_hive,
        };
        let msg = ExtendedMessage {
            header,
            route: None,
            payload,
            hmac_tag: None,
        };
        let mut buf = vec![0u8; 22 + payload.len() + 8];
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

        // Addressed to us → deliver locally. (Broadcast handling is a later
        // milestone; first light is unicast A->B / A->R->B.)
        // TODO(delivery-dedup): own a seen-set keyed on (origin, msg_id) before
        // multi-path flood delivery — open seam q for direct frames with no
        // route stack (origin not in header); candidate spec refinement.
        if dest == self.my_hive_id {
            return Inbound::Deliver {
                event_hash,
                payload: msg.payload.to_vec(),
            };
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
