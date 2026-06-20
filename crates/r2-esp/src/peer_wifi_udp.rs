//! WiFi/UDP peer transport — TRUE TN board-to-board (R2-ROUTE §1.4.4).
//!
//! Implements [`r2_transport::Transport`] for direct board-to-board R2-WIRE
//! datagram delivery on the SoftAP subnet. This is the *limbs* side of the
//! RouteEngine seam: core's `r2_route::RouteEngine` is the pure decision brain
//! (`plan_forward` → `ForwardAdvice`); this transport carries the bytes once
//! the engine picks a next-hop. See `docs/tn-routeengine-smallest-path.md`.
//!
//! Chosen as the first-light transport because it is connectionless (boards
//! already associate to the SoftAP and hold `10.42.0.x` addresses), reuses the
//! UDP plumbing already present for the presence burst, and matches the
//! `WireFormat::Extended` that `TransportId::Wifi` expects.
//!
//! ## Addressing
//! RouteEngine deals only in hive ids. `workshop` owns `hive_id → SocketAddr`
//! resolution — a static seed for first light (milestone 1), later fed from
//! R2-BEACON discovery (milestone 2). The exact key (full hive_id vs 16-bit
//! compressed) is pending core's seam answer (Q2); we key on the full `u32`
//! `hive_id` here and will adapt if core wants the compressed id.
//!
//! ## Scope of this module
//! Transport mechanics only: bind, datagram send (incl. broadcast), and a
//! non-blocking receive the RouteEngine loop drains. Header parsing, TTL/K
//! rewrite, dedup, and `plan_forward` orchestration live in the firmware's
//! route loop (pending core Q1/Q3/Q5) — kept out of here so the transport
//! stays medium-only, per the `Transport` contract ("MUST NOT alter the
//! R2-WIRE bytes").

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Mutex;

use log::{debug, warn};
use r2_transport::{LinkQuality, SendError, Transport, TransportId, TransportState};

/// UDP port for R2-WIRE TN peer datagrams.
///
/// Distinct from the hub data plane (`:21042`) and the OTA/reset/log/data
/// listeners (`:21043`–`:21047`) so a TN node can run alongside the legacy
/// hub-star link during bring-up.
pub const R2_TN_UDP_PORT: u16 = 21050;

/// A WiFi/UDP peer transport binding.
///
/// Owns a bound UDP socket plus a `hive_id → peer addr` table. `Transport`
/// methods take `&self` (the trait is shared-ref), so interior state is behind
/// `Mutex`.
pub struct WifiUdpTransport {
    sock: UdpSocket,
    /// target hive id → peer socket addr (workshop-owned resolution).
    peers: Mutex<HashMap<u32, SocketAddr>>,
    state: Mutex<TransportState>,
}

impl WifiUdpTransport {
    /// Bind the TN UDP socket on the SoftAP-assigned local address.
    ///
    /// Non-blocking so the single-threaded route loop can interleave
    /// `recv` with `plan_forward`/`send` without parking.
    pub fn bind(local_ip: Ipv4Addr) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(SocketAddrV4::new(local_ip, R2_TN_UDP_PORT))?;
        sock.set_nonblocking(true)?;
        sock.set_broadcast(true).ok();
        Ok(Self {
            sock,
            peers: Mutex::new(HashMap::new()),
            state: Mutex::new(TransportState::Available),
        })
    }

    /// Seed/refresh a peer's address.
    ///
    /// Static seed for milestone 1; milestone 2 feeds this from R2-BEACON
    /// discovery once a `hive_id ↔ IP` mapping is established.
    pub fn set_peer(&self, hive_id: u32, addr: SocketAddr) {
        self.peers.lock().unwrap().insert(hive_id, addr);
    }

    /// Number of currently known peers.
    pub fn peer_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Mark the transport state (e.g. `Unavailable` when WiFi drops). The
    /// RouteEngine reads this via `Transport::state()` for transport scoring.
    pub fn set_state(&self, state: TransportState) {
        *self.state.lock().unwrap() = state;
    }

    /// Non-blocking receive of one datagram: `(src addr, frame bytes)` written
    /// into `buf`. The route loop calls this, then builds a `ForwardRequest`
    /// from the R2-WIRE header. `None` = no datagram ready (WouldBlock).
    pub fn recv(&self, buf: &mut [u8]) -> Option<(SocketAddr, usize)> {
        match self.sock.recv_from(buf) {
            Ok((n, src)) => Some((src, n)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(e) => {
                warn!("[tn-udp] recv error: {e}");
                None
            }
        }
    }
}

impl Transport for WifiUdpTransport {
    fn id(&self) -> TransportId {
        TransportId::Wifi
    }

    fn state(&self) -> TransportState {
        *self.state.lock().unwrap()
    }

    /// Send a complete R2-WIRE frame to `target` (FNV/hive-id key).
    ///
    /// `target == 0` ⇒ broadcast: one datagram per known peer (first-light
    /// flood substrate; a real L2/IP broadcast address can replace this later).
    /// The frame bytes are sent verbatim — no R2-WIRE mutation here.
    fn send(&self, target: u32, frame: &[u8]) -> Result<(), SendError> {
        if frame.len() > self.current_mtu() {
            return Err(SendError::PayloadTooLarge);
        }
        let peers = self.peers.lock().unwrap();
        if target == 0 {
            // Broadcast to all known peers.
            let mut any = false;
            for (&hid, addr) in peers.iter() {
                match self.sock.send_to(frame, addr) {
                    Ok(_) => any = true,
                    Err(e) => warn!("[tn-udp] bcast to {hid:08x} ({addr}) failed: {e}"),
                }
            }
            return if any { Ok(()) } else { Err(SendError::Unreachable) };
        }
        let addr = peers.get(&target).copied().ok_or(SendError::Unreachable)?;
        match self.sock.send_to(frame, addr) {
            Ok(_) => {
                debug!("[tn-udp] sent {} B to {target:08x} ({addr})", frame.len());
                Ok(())
            }
            Err(e) => {
                warn!("[tn-udp] send to {target:08x} ({addr}) failed: {e}");
                Err(SendError::IoError)
            }
        }
    }

    /// Link quality to a neighbour. First light: binary reachable/quality=1.0
    /// for known peers (WiFi has no per-peer RSSI here). Refined later with
    /// measured RTT (`latency_ms`).
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
