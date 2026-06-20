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
    /// Build a node from its hive id and a bound transport.
    pub fn new(my_hive_id: u32, tx: UdpTransport) -> Self {
        Self {
            route: RouteNode::new(my_hive_id),
            tx,
            buf: vec![0u8; 1500],
        }
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
    /// through the engine: returns `Some(Delivered)` if it was for us, else
    /// `None` (relayed onward, dropped, or no datagram ready).
    pub fn poll(&mut self, now: u32) -> Option<Delivered> {
        // Split borrows: recv into buf, then route using tx immutably.
        let (src, n) = self.tx.recv(&mut self.buf)?;
        let from = self.tx.hive_for_addr(src).unwrap_or(0);
        let Self { route, tx, buf } = self;
        match route.on_inbound(&buf[..n], from, tx, now) {
            Inbound::Deliver { event_hash, payload } => Some(Delivered { event_hash, payload }),
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
            Some(Delivered {
                event_hash: EV,
                payload: b"node-api".to_vec()
            })
        );
    }
}
