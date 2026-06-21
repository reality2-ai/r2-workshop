//! [`Node`] — the firmware-facing TN runner: a [`RouteNode`] bundled with a
//! [`UdpTransport`](crate::udp::UdpTransport), exposing the two calls the
//! firmware loop needs: [`Node::originate`] and [`Node::poll`].
//!
//! Keeping this here (host-buildable, host-tested) means the ESP firmware glue
//! is trivial: construct a `Node`, `set_peer`, then in a thread call `poll(now)`
//! each tick and `originate(...)` on a trigger. The routing/dedup/relay
//! complexity is exercised on Alfred, not discovered on the bench.
//! See `docs/tn-routeengine-smallest-path.md`.

use crate::udp::UdpTransport;
use crate::{Inbound, RouteNode};
use r2_route::transport::Transport as RTransport;

/// A delivered frame addressed to this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    /// R2-WIRE event hash (FNV-1a of the event name).
    pub event_hash: u32,
    /// Decoded payload bytes.
    pub payload: Vec<u8>,
}

/// What a [`Node::poll`] surfaced from one inbound datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollEvent {
    /// A frame addressed to (and trust-gated for) this node.
    Delivered(Delivered),
    /// A conductor Heartbeat for our trust group — drive the lub-dub LED
    /// (visual beat-as-one; no PLL). Firmware toggles its LED pin on this.
    Beat,
}

/// A running TN node over WiFi/UDP.
///
/// Generic over the engine table sizes; use [`McuNode`] on the firmware.
pub struct Node<const N: usize = 64, const P: usize = 64, const D: usize = 64> {
    route: RouteNode<N, P, D>,
    tx: UdpTransport,
    buf: Vec<u8>,
}

/// Constrained-MCU [`Node`] profile (`RouteEngine<16,16,32>`) for the firmware.
pub type McuNode = Node<16, 16, 32>;

impl<const N: usize, const P: usize, const D: usize> Node<N, P, D> {
    /// Build a node from its hive id and a bound transport (untrusted — open
    /// routing; no signing, no deliver-gate).
    pub fn new(my_hive_id: u32, tx: UdpTransport) -> Self {
        Self {
            route: RouteNode::new(my_hive_id),
            tx,
            buf: vec![0u8; 1500],
        }
    }

    /// Build a TRUSTED node: signs every originated frame with the group HMAC
    /// and gates delivery (R2-TRUST B1). `my_tg` = `fnv1a_32(tg_id)`, `hk` = the
    /// group HMAC key — both from the persona bundle ([`crate::persona`]).
    pub fn new_with_trust(my_hive_id: u32, tx: UdpTransport, my_tg: u32, hk: [u8; 32]) -> Self {
        Self {
            route: RouteNode::new(my_hive_id).with_trust(my_tg, hk),
            tx,
            buf: vec![0u8; 1500],
        }
    }

    /// Add a live inter-TG entanglement (peering HMAC + ENC keys) to a trusted
    /// node, for cross-TG delivery (rung-2a/2b). No-op if untrusted.
    pub fn entangle(&mut self, peering_hmac: [u8; 32], enc_key: [u8; 32]) {
        self.route.entangle(peering_hmac, enc_key);
    }

    /// Access the transport (e.g. to `set_peer` from discovery / static seed).
    pub fn transport(&self) -> &UdpTransport {
        &self.tx
    }

    /// Seed a direct neighbour so origination can route before any inbound
    /// traffic teaches the engine the peer (first-light static seed).
    pub fn seed_direct(&mut self, peer_hive_id: u32, now: u32) {
        self.route.seed_direct(peer_hive_id, RTransport::Wifi, now);
    }

    /// Originate a frame to `dest_hive`; returns the next-hop it was sent to.
    pub fn originate(
        &mut self,
        dest_hive: u32,
        event_hash: u32,
        payload: &[u8],
        now: u32,
    ) -> Result<u32, &'static str> {
        self.route.originate(dest_hive, event_hash, payload, &self.tx, now)
    }

    /// Poll the transport once (non-blocking). If a datagram arrives, run it
    /// through the engine: returns `Some(PollEvent::Delivered)` if it was for us,
    /// `Some(PollEvent::Beat)` for a conductor heartbeat for our TG, else `None`
    /// (relayed onward, dropped, or no datagram ready).
    pub fn poll(&mut self, now: u32) -> Option<PollEvent> {
        // Split borrows: recv into buf, then route using tx immutably.
        let (src, n) = self.tx.recv(&mut self.buf)?;
        // Mesh address-learning: map the immediate sender's hive_id -> its src
        // addr so we can route back / relay onward to peers we never statically
        // seeded (needed for AP-relays-STA1->STA2 on the board-hosted SoftAP,
        // where DHCP addrs aren't known ahead of time). Falls back to any
        // statically-configured mapping if the frame carries no route stack.
        let from = crate::immediate_sender(&self.buf[..n])
            .or_else(|| self.tx.hive_for_addr(src))
            .unwrap_or(0);
        if from != 0 {
            self.tx.set_peer(from, src);
        }
        let Self { route, tx, buf } = self;
        match route.on_inbound(&buf[..n], from, tx, now) {
            Inbound::Deliver { event_hash, payload } => {
                Some(PollEvent::Delivered(Delivered { event_hash, payload }))
            }
            Inbound::Heartbeat => Some(PollEvent::Beat),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::time::Duration;

    const A: u32 = 0xA1A1_A1A1;
    const B: u32 = 0xB2B2_B2B2;
    const EV: u32 = 0x5151_5151;

    // The firmware-facing API end to end over real UDP: A.originate -> B.poll.
    #[test]
    fn node_api_routes_a_to_b_over_udp() {
        let lo = SocketAddr::from(([127, 0, 0, 1], 0));
        let txa = UdpTransport::bind_addr(lo).unwrap();
        let txb = UdpTransport::bind_addr(lo).unwrap();
        let a_addr = txa.local_addr().unwrap();
        let b_addr = txb.local_addr().unwrap();
        txa.set_peer(B, b_addr);
        txb.set_peer(A, a_addr);

        let mut a: McuNode = Node::new(A, txa);
        let mut b: McuNode = Node::new(B, txb);
        a.seed_direct(B, 0);

        assert_eq!(a.originate(B, EV, b"node-api", 0).unwrap(), B);

        let mut got = None;
        for _ in 0..200 {
            if let Some(d) = b.poll(0) {
                got = Some(d);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            got,
            Some(PollEvent::Delivered(Delivered {
                event_hash: EV,
                payload: b"node-api".to_vec()
            }))
        );
    }

    // TRUSTED node end-to-end: A and B share tg+hk; A's signed frame verifies at
    // B's GroupHmac gate and delivers. Proves new_with_trust wires the gate.
    #[test]
    fn trusted_node_signed_frame_delivers_over_udp() {
        const TG: u32 = 0x4B3D_F45D;
        const HK: [u8; 32] = [0x9A; 32];
        let lo = SocketAddr::from(([127, 0, 0, 1], 0));
        let txa = UdpTransport::bind_addr(lo).unwrap();
        let txb = UdpTransport::bind_addr(lo).unwrap();
        let a_addr = txa.local_addr().unwrap();
        let b_addr = txb.local_addr().unwrap();
        txa.set_peer(B, b_addr);
        txb.set_peer(A, a_addr);

        let mut a: McuNode = Node::new_with_trust(A, txa, TG, HK);
        let mut b: McuNode = Node::new_with_trust(B, txb, TG, HK);
        a.seed_direct(B, 0);

        assert_eq!(a.originate(B, EV, b"trusted", 0).unwrap(), B);
        assert_eq!(
            pump(&mut b),
            Some(PollEvent::Delivered(Delivered { event_hash: EV, payload: b"trusted".to_vec() })),
        );
    }

    // Drain a node a few times (loopback UDP is async).
    fn pump<const N: usize, const P: usize, const D: usize>(node: &mut Node<N, P, D>) -> Option<PollEvent> {
        for _ in 0..100 {
            if let Some(e) = node.poll(0) {
                return Some(e);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        None
    }

    // Roy's #19: AP relays STA1 -> STA2 over the board-hosted SoftAP, with the AP
    // LEARNING each STA's addr dynamically (no static seed of STA addrs on the
    // AP — exactly the DHCP-on-SoftAP case). Real UDP loopback, 3 Node()s.
    #[test]
    fn ap_relays_sta_to_sta_with_address_learning() {
        const AP: u32 = 0x00AA_0001;
        const S1: u32 = 0x0051_0002;
        const S2: u32 = 0x0052_0003;
        const EV2: u32 = 0x9999_0000;
        let lo = || SocketAddr::from(([127, 0, 0, 1], 0));
        let tap = UdpTransport::bind_addr(lo()).unwrap();
        let t1 = UdpTransport::bind_addr(lo()).unwrap();
        let t2 = UdpTransport::bind_addr(lo()).unwrap();
        let ap_addr = tap.local_addr().unwrap();
        // STAs know only their gateway (the AP). The AP seeds NOBODY — it learns
        // both STAs' addresses from the frames they send.
        t1.set_peer(AP, ap_addr);
        t2.set_peer(AP, ap_addr);

        let mut ap: McuNode = Node::new(AP, tap);
        let mut s1: McuNode = Node::new(S1, t1);
        let mut s2: McuNode = Node::new(S2, t2);
        s1.seed_direct(AP, 0); // flood toward AP for unknown dests
        s2.seed_direct(AP, 0);

        // STA2 announces to the AP -> AP learns STA2 (addr + neighbour + path).
        s2.originate(AP, EV, b"s2-hello", 0).unwrap();
        let _ = pump(&mut ap); // AP processes STA2's hello (learns it)

        // STA1 originates to STA2 (no direct path -> floods to the AP).
        s1.originate(S2, EV2, b"s1->s2", 0).unwrap();
        let _ = pump(&mut ap); // AP receives + relays toward STA2 (learned)

        // STA2 receives the AP-relayed frame and delivers it.
        let got = pump(&mut s2);
        assert_eq!(
            got,
            Some(PollEvent::Delivered(Delivered { event_hash: EV2, payload: b"s1->s2".to_vec() })),
            "AP must relay STA1->STA2 using the address it learned"
        );
    }
}
