//! `EspNegotiationRadio` + `spawn` — the esp-idf (Path-A) impl of
//! [`r2_discovery::negotiation::NegotiationRadio`] + the run-loop, so workshop
//! boards JOIN the transient mesh over the SAME canonical engine + `ControlMsg`
//! codec as hive's esp-radio side (the cross-platform north-star proof; #24
//! Profile-A).
//!
//! Division (per docs/PATHDEP-MIGRATION-PLAN.md + core's R2-24 brief):
//! - **engine** (`r2_discovery::negotiation::NegotiationEngine`) drives S0-S4 +
//!   the `lowest_live_id` election + the `ControlMsg` codec — platform-agnostic.
//!   `engine.poll(&mut radio)` is the tick; `request_data_plane()` signals a need.
//! - **this radio** owns the esp-idf transport glue: beacon advertise/scan
//!   ([`crate::beacon`]), the L2CAP CoC control channel ([`crate::l2cap`]), and
//!   the WiFi data plane ([`crate::wifi_ap`]/[`crate::wifi_sta`]).
//!
//! Wire interop with hive's esp-radio side is guaranteed by construction: same
//! `r2_discovery::ControlMsg` encode/decode, same `[len_lo,len_hi]` LE CoC frame
//! (l2cap.rs), PSM 0x00D2 — verified byte-exact cross-board (hive M7/M9).
//!
//! THREADING: the engine + radio live on the spawned tick thread; the beacon
//! scan CALLBACK (NimBLE host thread) feeds observations through a [`NegSink`]
//! (shared `Arc<Mutex>` queues). L2CAP recv is pulled in-thread via
//! `l2cap::drain_received` (it has its own mutex).
//!
//! METAL-INTEGRATION TODOs (`// TODO(metal)`, paired with hive's M8c + a hardware
//! window): the NimBLE *connectable* adv set (so a joiner gets the CoC connect
//! addr), the L2CAP central-connect (joiner side), the beacon-scan→`resolve_rbid`
//! →`NegSink` wiring, and the WiFi bring-up modem threading. The structure +
//! engine tick loop + codec path compile + are ready; those bits get wired/tuned
//! against hive's boards (NimBLE/L2CAP/coexistence specifics need metal).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use core::time::Duration;
use std::sync::Mutex;

use log::info;

use r2_discovery::beacon::PowerState;
use r2_discovery::negotiation::{
    BeaconAd, ControlMsg, DataPlaneParams, DataPlaneState, NegObservation, NegotiationEngine,
    NegotiationRadio,
};

extern crate alloc;

/// HiveId is `u32` in r2-discovery (R2-WIRE §8.2 wire id).
type HiveId = u32;

/// Constrained roster size for the engine on an MCU (peers in a transient mesh).
pub const NEG_ROSTER: usize = 16;
/// Profile-A timers (R2-WIFI §3.3.1): T_fallback bounds peer silence, T_negotiate
/// bounds the offer wait.
pub const T_FALLBACK_MS: u64 = 5_000;
pub const T_NEGOTIATE_MS: u64 = 10_000;

/// Shared sink the beacon scan callback pushes into (NimBLE thread → engine
/// thread). Cheap to clone (Arc). The firmware captures one in its
/// `beacon::start` `on_peer_observed` closure and calls [`NegSink::observe`].
#[derive(Clone)]
pub struct NegSink {
    scan_rx: Arc<Mutex<VecDeque<NegObservation>>>,
    addr_map: Arc<Mutex<BTreeMap<HiveId, [u8; 6]>>>,
}

impl NegSink {
    /// Record a scanned peer: its resolved hive_id (from `resolve_rbid`), BLE addr
    /// (for the CoC control bridge), and caps from the §7.2 flags byte.
    pub fn observe(&self, hive_id: HiveId, addr: [u8; 6], ap_capable: bool, power: PowerState) {
        if let Ok(mut m) = self.addr_map.lock() {
            m.insert(hive_id, addr);
        }
        if let Ok(mut q) = self.scan_rx.lock() {
            q.push_back(NegObservation::new(hive_id, ap_capable, power));
        }
    }
}

/// esp-idf NegotiationRadio. Bridges the canonical engine to beacon/L2CAP/WiFi.
pub struct EspNegotiationRadio {
    my_hive_id: HiveId,
    addr_map: Arc<Mutex<BTreeMap<HiveId, [u8; 6]>>>,
    scan_rx: Arc<Mutex<VecDeque<NegObservation>>>,
    control_rx: VecDeque<(HiveId, ControlMsg)>,
    dp_state: DataPlaneState,
    advert: Option<BeaconAd>,
}

impl EspNegotiationRadio {
    /// Create over this node's canonical hive_id. Returns the radio + the
    /// [`NegSink`] the firmware wires to its beacon scan callback.
    pub fn new(my_hive_id: HiveId) -> (Self, NegSink) {
        let addr_map = Arc::new(Mutex::new(BTreeMap::new()));
        let scan_rx = Arc::new(Mutex::new(VecDeque::new()));
        let sink = NegSink {
            scan_rx: scan_rx.clone(),
            addr_map: addr_map.clone(),
        };
        (
            Self {
                my_hive_id,
                addr_map,
                scan_rx,
                control_rx: VecDeque::new(),
                dp_state: DataPlaneState::Unavailable,
                advert: None,
            },
            sink,
        )
    }

    /// Drain the L2CAP CoC and decode control messages (call each tick before the
    /// engine polls). Maps the source addr → HiveId via addr_map.
    pub fn pump_control(&mut self) {
        #[cfg(feature = "ble")]
        for (payload, addr) in crate::l2cap::drain_received() {
            if let Some(msg) = ControlMsg::decode(&payload) {
                let hid = self
                    .addr_map
                    .lock()
                    .ok()
                    .and_then(|m| m.iter().find_map(|(h, a)| (*a == addr).then_some(*h)))
                    .unwrap_or(0);
                self.control_rx.push_back((hid, msg));
            }
        }
    }

    /// This node's hive id.
    pub fn hive_id(&self) -> HiveId {
        self.my_hive_id
    }
}

impl NegotiationRadio for EspNegotiationRadio {
    fn advertise(&mut self, beacon: &BeaconAd) {
        self.advert = Some(*beacon);
        // TODO(metal): push caps (provider_capable + power_state) into the live
        // beacon advert + run BOTH adv sets (non-connectable RBID beacon for
        // discovery + a CONNECTABLE adv carrying the same RBID so a joiner gets the
        // CoC connect addr). beacon.rs connectable-adv addition.
    }

    fn poll_scan(&mut self) -> Option<NegObservation> {
        self.scan_rx.lock().ok().and_then(|mut q| q.pop_front())
    }

    fn send_control(&mut self, peer: HiveId, msg: &ControlMsg) {
        let mut buf = [0u8; ControlMsg::MAX_ENCODED_LEN];
        let n = msg.encode(&mut buf);
        let addr = self.addr_map.lock().ok().and_then(|m| m.get(&peer).copied());
        if let Some(_addr) = addr {
            #[cfg(feature = "ble")]
            {
                // l2cap wraps with the [len_lo,len_hi] LE frame; byte-identical to
                // hive's esp-radio CoC (M7/M9). TODO(metal): if no channel to this
                // peer yet, central-connect first (joiner side).
                let _ = crate::l2cap::send_to(&_addr, &buf[..n]);
            }
        }
    }

    fn poll_control(&mut self) -> Option<(HiveId, ControlMsg)> {
        self.control_rx.pop_front()
    }

    fn bring_up_provider(&mut self, params: &DataPlaneParams) -> bool {
        let ssid = core::str::from_utf8(&params.ssid[..params.ssid_len as usize]).unwrap_or("");
        let _psk = core::str::from_utf8(&params.psk[..params.psk_len as usize]).unwrap_or("");
        // TODO(metal): wifi_ap::start (needs the modem, threaded by the firmware).
        let ok = !ssid.is_empty();
        self.dp_state = if ok { DataPlaneState::Available } else { DataPlaneState::Failed };
        ok
    }

    fn join_provider(&mut self, params: &DataPlaneParams) -> bool {
        let ssid = core::str::from_utf8(&params.ssid[..params.ssid_len as usize]).unwrap_or("");
        let _psk = core::str::from_utf8(&params.psk[..params.psk_len as usize]).unwrap_or("");
        // TODO(metal): wifi_sta::connect_static (no-DHCP self-assign per ap_hint).
        let ok = !ssid.is_empty();
        self.dp_state = if ok { DataPlaneState::Available } else { DataPlaneState::Failed };
        ok
    }

    fn data_plane_state(&self) -> DataPlaneState {
        self.dp_state
    }

    fn teardown_data_plane(&mut self) {
        // TODO(metal): drop the WiFi handle (graceful WifiDone).
        self.dp_state = DataPlaneState::Unavailable;
    }

    fn now_ms(&self) -> u64 {
        (unsafe { esp_idf_svc::sys::esp_timer_get_time() } as u64) / 1000
    }
}

/// Config for [`spawn`].
pub struct NegotiationConfig {
    /// This node's canonical wire hive_id (from the persona / §6.2.1 identity).
    pub my_hive_id: HiveId,
    /// Can this board be the WiFi provider (SoftAP)? → election eligibility.
    pub ap_capable: bool,
    /// Initial power state (bits 1-0 of the §7.2 flags byte).
    pub power_state: PowerState,
    /// Whether this node needs the data plane (drives S0→S1).
    pub want_data: bool,
}

/// Build the engine + radio, return the [`NegSink`] (wire it to your beacon scan
/// callback), and run the negotiation tick loop on a dedicated thread.
/// Returns the sink immediately; the radio is owned by the loop.
///
/// The firmware must, separately (radio glue): start the beacon (feeding the sink
/// via `NegSink::observe`), start the L2CAP CoC server, and hold the WiFi modem for
/// `bring_up`/`join`. METAL: those starts + the connectable-adv/central-connect are
/// the `TODO(metal)` items wired against hive's boards.
pub fn spawn(cfg: NegotiationConfig) -> std::io::Result<NegSink> {
    let (mut radio, sink) = EspNegotiationRadio::new(cfg.my_hive_id);
    let caps = r2_discovery::negotiation::NodeCaps::new(cfg.ap_capable, cfg.power_state);
    let mut engine = NegotiationEngine::<NEG_ROSTER>::new(
        cfg.my_hive_id,
        caps,
        T_FALLBACK_MS,
        T_NEGOTIATE_MS,
    );
    if cfg.want_data {
        engine.request_data_plane();
    }

    std::thread::Builder::new()
        .stack_size(8192)
        .name("neg".into())
        .spawn(move || {
            info!(
                "[neg] node up hive_id={:08x} ap_capable={} — joining transient mesh",
                cfg.my_hive_id, cfg.ap_capable
            );
            let mut last_state = engine.state();
            loop {
                radio.pump_control();
                let state = engine.poll(&mut radio);
                if state != last_state {
                    info!("[neg] {last_state:?} -> {state:?} (provider={:?})", engine.provider());
                    last_state = state;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })?;
    Ok(sink)
}
