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

    // ── #18 health telemetry (r2.hb.health) ──
    /// Collector hive id (the AP hub) to UNICAST health to; `None` disables
    /// health emit (e.g. on the AP/collector itself).
    pub collector: Option<u32>,
    /// `fnv1a_32("r2.hb.health")` — health event hash (firmware computes it).
    pub health_event: u32,
    /// Emit health every N originate beats (5 per composer's contract → ~15s).
    pub health_every: u32,
    /// This node's role bitset (`r2_tn::health::role`).
    pub role: u8,
    /// Trust-group id (0 if untrusted).
    pub tg: u32,
    /// Firmware version + git sha (baked R2_FW_VER / R2_GIT_SHA).
    pub fw_version: &'static str,
    /// Firmware git sha.
    pub fw_sha: &'static str,
    /// FIELDED broadcast mode: when set, all frames go to this subnet broadcast
    /// (e.g. 192.168.4.255:21042) — hive's r2-fieldlab transport. `None` =
    /// per-peer unicast (the board-hosted r2-tn-lab demo).
    pub broadcast_addr: Option<SocketAddr>,
    /// Trust context `(tg, hk)` from the persona bundle: when set, the node signs
    /// every originated frame with the group HMAC and gates delivery (a TRUSTED
    /// member of TG `tg`). `None` = untrusted open routing.
    pub trust: Option<(u32, [u8; 32])>,
}

/// Bind the transport, build the node, and run the receive/originate loop on a
/// dedicated thread. Returns once the thread is spawned.
pub fn spawn(cfg: TnConfig) -> Result<()> {
    let tx = WifiUdpTransport::bind(cfg.local_ip)?;
    for (id, addr) in &cfg.peers {
        tx.set_peer(*id, *addr);
    }
    let mut node = match cfg.trust {
        Some((tg, hk)) => McuNode::new_with_trust(cfg.my_hive_id, tx, tg, hk),
        None => McuNode::new(cfg.my_hive_id, tx),
    };
    for (id, _) in &cfg.peers {
        node.seed_direct(*id, 0);
    }
    // FIELDED: broadcast every frame to the subnet (hive's r2-fieldlab); the
    // RouteEngine advice still computed, receivers filter by target_hive.
    if let Some(bcast) = cfg.broadcast_addr {
        node.transport().set_broadcast_addr(Some(bcast));
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
            let ip_str = cfg.local_ip.to_string();
            let start = Instant::now();
            let mut next_originate = cfg.originate_period;
            let mut beat: u32 = 0;
            let health_every = cfg.health_every.max(1);
            loop {
                let now = start.elapsed().as_secs() as u32;

                // Drain everything currently queued.
                while let Some(ev) = node.poll(now) {
                    match ev {
                        r2_tn::PollEvent::Delivered(d) => info!(
                            "[tn] DELIVERED ev={:08x} {} bytes — HARDWARE FRAME",
                            d.event_hash,
                            d.payload.len()
                        ),
                        // Conductor heartbeat for our TG → lub-dub beat. Logged for
                        // now; driving the carrier LED (GPIO15 mono on C6) is the
                        // pending firmware-glue step (composer §12.3 carrier-aware).
                        r2_tn::PollEvent::Beat => info!("[tn] BEAT — heartbeat for our TG"),
                    }
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
                        beat = beat.wrapping_add(1);

                        // #18: every 5th beat, UNICAST r2.hb.health to the
                        // collector (the AP hub). sync_state=0 (free) until a
                        // conductor-PLL exists; link_q placeholder until per-peer
                        // RSSI. NO flood (originate -> Directed to the collector).
                        if beat % health_every == 0 {
                            if let Some(collector) = cfg.collector {
                                let report = r2_tn::health::HealthReport {
                                    hive_id: cfg.my_hive_id,
                                    tg: cfg.tg,
                                    role: cfg.role,
                                    ip: &ip_str,
                                    fw_version: cfg.fw_version,
                                    fw_sha: cfg.fw_sha,
                                    ota_status: r2_tn::health::ota_status::CURRENT,
                                    sync_state: r2_tn::health::sync_state::FREE,
                                    phase_err_ms: 0,
                                    link_q: 100,
                                    transports: r2_tn::health::transport_bit::WIFI,
                                    uptime_s: now,
                                    beat_seq: beat,
                                };
                                let mut hbuf = [0u8; 256];
                                match report.encode(&mut hbuf) {
                                    Ok(n) => match node.originate(collector, cfg.health_event, &hbuf[..n], now) {
                                        Ok(_) => info!("[tn] health -> collector {collector:08x} ({n} B)"),
                                        Err(e) => warn!("[tn] health emit failed: {e}"),
                                    },
                                    Err(e) => warn!("[tn] health encode failed: {e:?}"),
                                }
                            }
                        }
                        next_originate = start.elapsed() + cfg.originate_period;
                    }
                }

                std::thread::sleep(Duration::from_millis(20));
            }
        })?;
    Ok(())
}
