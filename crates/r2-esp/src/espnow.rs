//! ESP-NOW peer transport — esp-idf [`r2_transport::Transport`] impl for the
//! Mode-2 true-mesh data plane (R2-DISCOVERY §4A `DataPlaneMode::Mesh`).
//!
//! Mirrors the [`crate::peer_wifi_udp::WifiUdpTransport`] pattern (`r2_tn::udp`)
//! but over connectionless ESP-NOW (no AP, no association) so a WORKSHOP esp-idf
//! board JOINS hive's leaderless ESP-NOW mesh = the cross-platform north-star proof
//! (#14 heterogeneity: a different impl joining the esp-radio mesh). The RouteEngine
//! dispatches `ForwardAdvice::Directed(DirectedHop{transport: EspNow, ..})` here by
//! `DirectedHop.transport`.
//!
//! Addressing (same contract as UDP): RouteEngine deals only in hive ids; this
//! transport owns `hive_id ↔ MAC`. `send(target,..)`: `target = 0` → ESP-NOW
//! BROADCAST (the true-mesh flood primitive; receivers filter by `target_hive`);
//! else `hive_id → MAC` unicast. `recv` drains a queue filled by the IDF recv
//! callback (mirrors l2cap's RX_QUEUE), then [`hive_for_mac`] maps src→hive id for
//! `RouteNode::on_inbound`.
//!
//! INTEROP (cross-platform vs hive's esp-radio ESP-NOW — the data-plane analogue of
//! the M7 CoC contract; confirm via fleet): payload = the raw R2-WIRE frame
//! (≤250B `ESP_NOW_MAX_DATA_LEN`), no extra wrapper, STA ifidx, plaintext (R2 trust
//! is at the frame layer). `// TODO(interop)`: confirm hive's fixed channel + whether
//! it fragments R2-WIRE frames > 250B.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use std::sync::Mutex;

use esp_idf_svc::espnow::{EspNow, PeerInfo, BROADCAST};
use esp_idf_svc::sys::{wifi_interface_t_WIFI_IF_STA, EspError, ESP_NOW_MAX_DATA_LEN};

use r2_transport::{LinkQuality, SendError, Transport, TransportId, TransportState};

extern crate alloc;

/// ESP-NOW max application payload (esp-idf `ESP_NOW_MAX_DATA_LEN` = 250).
pub const ESPNOW_MTU: usize = ESP_NOW_MAX_DATA_LEN as usize;

/// An ESP-NOW peer transport for the Mode-2 mesh data plane.
pub struct EspNowTransport {
    espnow: EspNow<'static>,
    /// hive_id → peer MAC (send / target resolution).
    peers: Mutex<BTreeMap<u32, [u8; 6]>>,
    /// peer MAC → hive_id (reverse, for recv → on_inbound's from_hive_id).
    by_mac: Mutex<BTreeMap<[u8; 6], u32>>,
    /// Inbound (src_mac, payload), filled by the IDF recv callback thread.
    rx: Arc<Mutex<VecDeque<([u8; 6], Vec<u8>)>>>,
    /// FIELDED broadcast mode: every frame to BROADCAST regardless of target (the
    /// true-mesh flood primitive — receivers filter by target_hive). Default true.
    broadcast_mode: Mutex<bool>,
    /// WiFi channel for added peers (0 = current). Confirm vs hive's mesh channel.
    channel: u8,
    state: Mutex<TransportState>,
}

impl EspNowTransport {
    /// Initialise ESP-NOW (WiFi STA must already be started by the firmware) on
    /// `channel` (0 = current), register the recv callback, add the BROADCAST peer.
    pub fn new(channel: u8) -> Result<Self, EspError> {
        let espnow = EspNow::take()?;
        let rx: Arc<Mutex<VecDeque<([u8; 6], Vec<u8>)>>> = Arc::new(Mutex::new(VecDeque::new()));
        let rx_cb = rx.clone();
        // The closure runs on a hidden IDF thread; it just enqueues (src, bytes).
        espnow.register_recv_cb(move |info, data| {
            if let Ok(mut q) = rx_cb.lock() {
                q.push_back((*info.src_addr, data.to_vec()));
            }
        })?;
        // Broadcast peer so flood sends work without a per-peer add.
        espnow.add_peer(PeerInfo {
            peer_addr: BROADCAST,
            channel,
            ifidx: wifi_interface_t_WIFI_IF_STA,
            encrypt: false,
            ..Default::default()
        })?;
        Ok(Self {
            espnow,
            peers: Mutex::new(BTreeMap::new()),
            by_mac: Mutex::new(BTreeMap::new()),
            rx,
            broadcast_mode: Mutex::new(true),
            channel,
            state: Mutex::new(TransportState::Available),
        })
    }

    /// Seed/refresh a peer's MAC (from the beacon/RBID HiveId↔MAC mapping) and
    /// register it with ESP-NOW for unicast.
    pub fn set_peer(&self, hive_id: u32, mac: [u8; 6]) -> Result<(), EspError> {
        if !self.espnow.peer_exists(mac).unwrap_or(false) {
            self.espnow.add_peer(PeerInfo {
                peer_addr: mac,
                channel: self.channel,
                ifidx: wifi_interface_t_WIFI_IF_STA,
                encrypt: false,
                ..Default::default()
            })?;
        }
        self.peers.lock().unwrap().insert(hive_id, mac);
        self.by_mac.lock().unwrap().insert(mac, hive_id);
        Ok(())
    }

    /// Resolve a frame's source MAC back to a known peer hive id.
    pub fn hive_for_mac(&self, mac: &[u8; 6]) -> Option<u32> {
        self.by_mac.lock().unwrap().get(mac).copied()
    }

    /// Enable/disable fielded broadcast mode (default on for the true-mesh flood).
    pub fn set_broadcast_mode(&self, on: bool) {
        *self.broadcast_mode.lock().unwrap() = on;
    }

    /// Non-blocking receive of one frame: `(src_mac, payload)`.
    pub fn recv(&self) -> Option<([u8; 6], Vec<u8>)> {
        self.rx.lock().unwrap().pop_front()
    }

    /// Number of known unicast peers.
    pub fn peer_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Mark the transport state (e.g. `Unavailable` when WiFi drops).
    pub fn set_state(&self, state: TransportState) {
        *self.state.lock().unwrap() = state;
    }
}

impl Transport for EspNowTransport {
    fn id(&self) -> TransportId {
        TransportId::EspNow
    }

    fn state(&self) -> TransportState {
        *self.state.lock().unwrap()
    }

    /// Send a complete R2-WIRE frame to `target` (hive id; 0 = broadcast). In
    /// broadcast mode every frame goes to BROADCAST (receivers filter by
    /// target_hive — the true-mesh primitive). Bytes sent verbatim, no mutation.
    fn send(&self, target: u32, frame: &[u8]) -> Result<(), SendError> {
        if frame.len() > ESPNOW_MTU {
            // TODO(interop): confirm whether hive fragments R2-WIRE > 250B; for now
            // reject (the RouteEngine MTU should keep frames within ESP-NOW limits).
            return Err(SendError::PayloadTooLarge);
        }
        let broadcast = *self.broadcast_mode.lock().unwrap() || target == 0;
        let mac = if broadcast {
            BROADCAST
        } else {
            match self.peers.lock().unwrap().get(&target).copied() {
                Some(m) => m,
                None => return Err(SendError::Unreachable),
            }
        };
        self.espnow.send(mac, frame).map_err(|_| SendError::IoError)
    }

    fn link_quality(&self, hive_id: u32) -> Option<LinkQuality> {
        if self.peers.lock().unwrap().contains_key(&hive_id) {
            Some(LinkQuality { quality: 1.0, ..Default::default() })
        } else {
            None
        }
    }
}
