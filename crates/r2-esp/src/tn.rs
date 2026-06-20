//! TN node run-loop — wires r2-tn's `Node` (RouteEngine + UdpTransport) into a
//! background thread for board-to-board frame routing over the real WiFi radio.
//!
//! Feature-gated (`tn`), OFF by default, so the production sensor firmware build
//! is byte-identical. A TN board is flashed with `--features tn` and configured
//! via build-time env (`R2_TN_PEER_ID`, `R2_TN_PEER_IP`, optional
//! `R2_TN_PEER_PORT`, `R2_TN_ORIGINATE`). See
//! `docs/tn-routeengine-smallest-path.md`.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{info, warn};
use r2_tn::McuNode;

use crate::peer_wifi_udp::WifiUdpTransport;

/// Configuration for [`spawn`].
pub struct TnConfig {
    /// This node's hive id (FNV-1a of its hive_id UUID, R2-WIRE §6.2.1).
    pub my_hive_id: u32,
    /// Local SoftAP-assigned address to bind the TN UDP socket on.
    pub local_ip: Ipv4Addr,
    /// Static peer seed: `(hive_id, addr)` — first-light substitute for
    /// R2-BEACON discovery.
    pub peers: Vec<(u32, SocketAddr)>,
    /// If set, periodically originate a frame to this destination hive id.
    pub originate_to: Option<u32>,
    /// Interval between originated frames.
    pub originate_period: Duration,
    /// R2-WIRE event hash for originated frames.
    pub event_hash: u32,
    /// Payload for originated frames.
    pub payload: Vec<u8>,
}

/// Bind the transport, build the node, and run the receive/originate loop on a
/// dedicated thread. Returns once the thread is spawned.
pub fn spawn(cfg: TnConfig) -> Result<()> {
    let tx = WifiUdpTransport::bind(cfg.local_ip)?;
    for (id, addr) in &cfg.peers {
        tx.set_peer(*id, *addr);
    }
    let mut node = McuNode::new(cfg.my_hive_id, tx);
    for (id, _) in &cfg.peers {
        node.seed_direct(*id, 0);
    }

    std::thread::Builder::new()
        .stack_size(8192)
        .name("tn".into())
        .spawn(move || {
            info!(
                "[tn] node up hive_id={:08x} on {} — {} peer(s), originate_to={:?}",
                cfg.my_hive_id,
                cfg.local_ip,
                cfg.peers.len(),
                cfg.originate_to.map(|d| format!("{d:08x}"))
            );
            let start = Instant::now();
            let mut next_originate = cfg.originate_period;
            loop {
                let now = start.elapsed().as_secs() as u32;

                // Drain everything currently queued.
                while let Some(d) = node.poll(now) {
                    info!(
                        "[tn] DELIVERED ev={:08x} {} bytes — HARDWARE FRAME",
                        d.event_hash,
                        d.payload.len()
                    );
                }

                if let Some(dest) = cfg.originate_to {
                    if start.elapsed() >= next_originate {
                        match node.originate(dest, cfg.event_hash, &cfg.payload, now) {
                            Ok(nh) => info!(
                                "[tn] originated ev={:08x} -> next_hop {:08x}",
                                cfg.event_hash, nh
                            ),
                            Err(e) => warn!("[tn] originate failed: {e}"),
                        }
                        next_originate = start.elapsed() + cfg.originate_period;
                    }
                }

                std::thread::sleep(Duration::from_millis(20));
            }
        })?;
    Ok(())
}
