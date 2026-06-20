//! WiFi/UDP peer transport — pure-std [`r2_transport::Transport`] impl for
//! board-to-board R2-WIRE datagrams (TRUE TN, first-light transport).
//!
//! Pure `std::net` (no esp-idf), so it is host-testable on Alfred AND runs on
//! ESP-IDF (lwIP provides POSIX UDP, same as the existing TCP listeners). The
//! ESP firmware re-exports this as `r2_esp::peer_wifi_udp::WifiUdpTransport`.
//! See `docs/tn-routeengine-smallest-path.md`.
//!
//! Addressing: RouteEngine deals only in hive ids; this transport owns the
//! `hive_id ↔ SocketAddr` mapping (static seed for first light, R2-BEACON-fed
//! later). `recv` reports the raw `SocketAddr`; [`UdpTransport::hive_for_addr`]
//! maps it back to the sender's hive id for `RouteNode::on_inbound`.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Mutex;

use r2_transport::{LinkQuality, SendError, Transport, TransportId, TransportState};

/// UDP port for R2-WIRE TN peer datagrams — the canonical R2_UDP_PORT
/// (R2-WIFI §4), aligned with hive's field.lab for cross-stack interop. (This
/// is the same number as the dashboard's TCP hub port, but a different
/// socket/protocol; a standalone TN node runs no hub listener.)
pub const R2_TN_UDP_PORT: u16 = 21042;

/// A WiFi/UDP peer transport binding.
pub struct UdpTransport {
    sock: UdpSocket,
    /// hive_id → peer addr (for send / target resolution).
    peers: Mutex<HashMap<u32, SocketAddr>>,
    /// peer addr → hive_id (reverse, for recv → on_inbound's from_hive_id).
    by_addr: Mutex<HashMap<SocketAddr, u32>>,
    /// FIELDED broadcast mode: when set, EVERY frame is sent to this subnet
    /// broadcast addr (e.g. 192.168.4.255:21042) regardless of `target` — the
    /// receivers filter by `target_hive`. Used on hive's r2-fieldlab (no per-peer
    /// unicast; matches hive's broadcast transport). `None` = unicast (the
    /// board-hosted r2-tn-lab demo, addr-per-peer).
    broadcast_addr: Mutex<Option<SocketAddr>>,
    state: Mutex<TransportState>,
}

impl UdpTransport {
    /// Bind the TN UDP socket on the SoftAP-assigned local address (fixed port).
    pub fn bind(local_ip: Ipv4Addr) -> std::io::Result<Self> {
        Self::bind_addr(SocketAddr::V4(SocketAddrV4::new(local_ip, R2_TN_UDP_PORT)))
    }

    /// Bind to an explicit address (use port 0 for an ephemeral test port).
    pub fn bind_addr(addr: SocketAddr) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(addr)?;
        sock.set_nonblocking(true)?;
        sock.set_broadcast(true).ok();
        Ok(Self {
            sock,
            peers: Mutex::new(HashMap::new()),
            by_addr: Mutex::new(HashMap::new()),
            broadcast_addr: Mutex::new(None),
            state: Mutex::new(TransportState::Available),
        })
    }

    /// Enable FIELDED broadcast mode: every frame is sent to `addr` (the subnet
    /// broadcast, e.g. 192.168.4.255:21042) regardless of target. `None` reverts
    /// to per-peer unicast.
    pub fn set_broadcast_addr(&self, addr: Option<SocketAddr>) {
        *self.broadcast_addr.lock().unwrap() = addr;
    }

    /// The socket's local address (resolves an ephemeral port).
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// Seed/refresh a peer's address (static seed; R2-BEACON-fed later).
    pub fn set_peer(&self, hive_id: u32, addr: SocketAddr) {
        self.peers.lock().unwrap().insert(hive_id, addr);
        self.by_addr.lock().unwrap().insert(addr, hive_id);
    }

    /// Resolve a datagram's source addr back to a known peer hive id.
    pub fn hive_for_addr(&self, addr: SocketAddr) -> Option<u32> {
        self.by_addr.lock().unwrap().get(&addr).copied()
    }

    /// Number of currently known peers.
    pub fn peer_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Mark the transport state (e.g. `Unavailable` when WiFi drops).
    pub fn set_state(&self, state: TransportState) {
        *self.state.lock().unwrap() = state;
    }

    /// Non-blocking receive of one datagram: `(src addr, len)` into `buf`.
    pub fn recv(&self, buf: &mut [u8]) -> Option<(SocketAddr, usize)> {
        match self.sock.recv_from(buf) {
            Ok((n, src)) => Some((src, n)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(_) => None,
        }
    }
}

impl Transport for UdpTransport {
    fn id(&self) -> TransportId {
        TransportId::Wifi
    }

    fn state(&self) -> TransportState {
        *self.state.lock().unwrap()
    }

    /// Send a complete R2-WIRE frame to `target` (full hive id; 0 = broadcast
    /// to all known peers). Bytes are sent verbatim — no R2-WIRE mutation.
    fn send(&self, target: u32, frame: &[u8]) -> Result<(), SendError> {
        if frame.len() > self.current_mtu() {
            return Err(SendError::PayloadTooLarge);
        }
        // FIELDED broadcast mode: one datagram to the subnet broadcast; the
        // RouteEngine's Directed/Flood advice still computed, but on a broadcast
        // medium every send is a broadcast and receivers filter by target_hive.
        if let Some(bcast) = *self.broadcast_addr.lock().unwrap() {
            return self
                .sock
                .send_to(frame, bcast)
                .map(|_| ())
                .map_err(|_| SendError::IoError);
        }
        let peers = self.peers.lock().unwrap();
        if target == 0 {
            let mut any = false;
            for addr in peers.values() {
                if self.sock.send_to(frame, addr).is_ok() {
                    any = true;
                }
            }
            return if any { Ok(()) } else { Err(SendError::Unreachable) };
        }
        let addr = peers.get(&target).copied().ok_or(SendError::Unreachable)?;
        self.sock
            .send_to(frame, addr)
            .map(|_| ())
            .map_err(|_| SendError::IoError)
    }

    fn link_quality(&self, hive_id: u32) -> Option<LinkQuality> {
        if self.peers.lock().unwrap().contains_key(&hive_id) {
            Some(LinkQuality {
                quality: 1.0,
                ..Default::default()
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Inbound, RouteNode};
    use r2_route::transport::Transport as RTransport;

    const A: u32 = 0xA1A1_A1A1;
    const B: u32 = 0xB2B2_B2B2;
    const EV: u32 = 0x1234_5678;

    // End-to-end over REAL UDP sockets on loopback: A originates -> the frame
    // crosses the wire -> B receives + delivers. The same loop the firmware
    // runs, minus the radio.
    #[test]
    fn routes_a_to_b_over_real_udp() {
        let lo = |p: u16| SocketAddr::from(([127, 0, 0, 1], p));
        let txa = UdpTransport::bind_addr(lo(0)).unwrap();
        let txb = UdpTransport::bind_addr(lo(0)).unwrap();
        let a_addr = txa.local_addr().unwrap();
        let b_addr = txb.local_addr().unwrap();
        txa.set_peer(B, b_addr);
        txb.set_peer(A, a_addr);

        let mut na = RouteNode::<16, 16, 32>::new(A);
        let mut nb = RouteNode::<16, 16, 32>::new(B);
        na.seed_direct(B, RTransport::Wifi, 0);

        let next = na.originate(B, EV, b"hi-over-udp", &txa, 0).unwrap();
        assert_eq!(next, B);

        // Drain B's socket (loopback is fast but async — spin briefly).
        let mut buf = [0u8; 512];
        let mut got = None;
        for _ in 0..200 {
            if let Some((src, n)) = txb.recv(&mut buf) {
                got = Some((src, n));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (src, n) = got.expect("B should receive the datagram over UDP");
        let from = txb.hive_for_addr(src).expect("known peer");
        assert_eq!(from, A);

        let outcome = nb.on_inbound(&buf[..n], from, &txb, 0);
        assert_eq!(
            outcome,
            Inbound::Deliver {
                event_hash: EV,
                payload: b"hi-over-udp".to_vec()
            }
        );
    }

    // FIELDED broadcast mode: with a broadcast addr set and NO per-peer mapping,
    // a send to an arbitrary target still reaches the broadcast addr. (Uses a
    // concrete loopback addr as the "broadcast" target — loopback can't do real
    // subnet broadcast, but this proves the send path ignores target/peer-map and
    // hits the configured addr, which is the fielded behaviour.)
    #[test]
    fn broadcast_mode_sends_regardless_of_target() {
        let lo = |p: u16| SocketAddr::from(([127, 0, 0, 1], p));
        let txa = UdpTransport::bind_addr(lo(0)).unwrap();
        let txb = UdpTransport::bind_addr(lo(0)).unwrap();
        let b_addr = txb.local_addr().unwrap();
        // No set_peer at all; just point broadcast at B's addr.
        txa.set_broadcast_addr(Some(b_addr));

        // Target is an unknown hive id — unicast would return Unreachable, but
        // broadcast mode sends anyway.
        txa.send(0xDEAD_BEEF, b"bcast-frame").expect("broadcast send ok");

        let mut buf = [0u8; 64];
        let mut got = None;
        for _ in 0..200 {
            if let Some((_src, n)) = txb.recv(&mut buf) {
                got = Some(n);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let n = got.expect("B should receive the broadcast datagram");
        assert_eq!(&buf[..n], b"bcast-frame");
    }
}
