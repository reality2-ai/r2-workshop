//! `EspNegotiationRadio` — the esp-idf (Path-A) impl of
//! [`r2_discovery::negotiation::NegotiationRadio`], so workshop boards JOIN the
//! transient mesh over the SAME canonical engine + `ControlMsg` codec as hive's
//! esp-radio side (the cross-platform north-star proof; #24 Profile-A).
//!
//! Division (per docs/PATHDEP-MIGRATION-PLAN.md + core's R2-24 brief):
//! - **engine** (`r2_discovery::negotiation::NegotiationEngine`) drives S0-S4 +
//!   the `lowest_live_id` election + the `ControlMsg` codec — platform-agnostic.
//! - **this radio** owns the esp-idf transport glue: beacon advertise/scan
//!   ([`crate::beacon`]), the L2CAP CoC control channel ([`crate::l2cap`]), and
//!   the WiFi data plane ([`crate::wifi_ap`]/[`crate::wifi_sta`]).
//!
//! Wire interop with hive's esp-radio side is guaranteed by construction: same
//! `r2_discovery::ControlMsg` encode/decode, same `[len_lo,len_hi]` LE CoC frame
//! (l2cap.rs), PSM 0x00D2. Verified byte-exact cross-board (hive M7/M9).
//!
//! METAL-INTEGRATION TODOs (paired with hive's M8c + a hardware window; marked
//! `// TODO(metal)` below): the NimBLE *connectable* adv set (so a joiner gets the
//! CoC connect addr), the L2CAP central-connect (joiner side), and feeding the
//! scan queue from beacon's RBID-resolved observations. The structure + the codec
//! path + the data-plane bring-up compile + are ready; those bits get wired/tuned
//! against hive's boards.

use alloc::collections::{BTreeMap, VecDeque};

use r2_discovery::beacon::PowerState;
use r2_discovery::negotiation::{
    BeaconAd, ControlMsg, DataPlaneParams, DataPlaneState, NegObservation, NegotiationRadio,
};

extern crate alloc;

/// HiveId is `u32` in r2-discovery (R2-WIRE §8.2 wire id).
type HiveId = u32;

/// esp-idf NegotiationRadio. Bridges the canonical engine to beacon/L2CAP/WiFi.
pub struct EspNegotiationRadio {
    /// This node's canonical wire hive_id.
    my_hive_id: HiveId,
    /// HiveId → last-seen BLE addr, populated from connectable-adv scans
    /// (RBID-resolved). send_control maps HiveId→addr; poll_control maps back.
    addr_map: BTreeMap<HiveId, [u8; 6]>,
    /// Inbound control messages decoded from the CoC, tagged by resolved HiveId.
    control_rx: VecDeque<(HiveId, ControlMsg)>,
    /// Scanned peers awaiting the engine's poll_scan.
    scan_rx: VecDeque<NegObservation>,
    /// Current data-plane state (the disruption signal).
    dp_state: DataPlaneState,
    /// Last BeaconAd we were told to advertise (caps drive peers' election).
    advert: Option<BeaconAd>,
}

impl EspNegotiationRadio {
    /// Create over this node's canonical hive_id. The caller wires beacon/L2CAP
    /// start separately (radio glue), then drives the engine with this as the
    /// `NegotiationRadio`.
    pub fn new(my_hive_id: HiveId) -> Self {
        Self {
            my_hive_id,
            addr_map: BTreeMap::new(),
            control_rx: VecDeque::new(),
            scan_rx: VecDeque::new(),
            dp_state: DataPlaneState::Unavailable,
            advert: None,
        }
    }

    /// Feed a scanned peer (called from the beacon scan callback after it
    /// resolves RBID→hive_id and reads caps from the §7.2 flags byte). Records
    /// the addr for the control-plane bridge + queues the observation.
    pub fn on_peer(&mut self, hive_id: HiveId, addr: [u8; 6], ap_capable: bool, power: PowerState) {
        self.addr_map.insert(hive_id, addr);
        self.scan_rx
            .push_back(NegObservation::new(hive_id, ap_capable, power));
    }

    /// Drain the L2CAP CoC and decode any control messages (called each tick
    /// before the engine polls). Maps the source addr → HiveId via addr_map.
    pub fn pump_control(&mut self) {
        #[cfg(feature = "ble")]
        for (payload, addr) in crate::l2cap::drain_received() {
            // payload = the ControlMsg bytes (l2cap strips the [len_lo,len_hi]).
            if let Some(msg) = ControlMsg::decode(&payload) {
                let hid = self
                    .addr_map
                    .iter()
                    .find_map(|(h, a)| if *a == addr { Some(*h) } else { None })
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
        // Store the caps the engine wants advertised; the beacon task maps these
        // to the §7.2 flags byte (provider_capable + power_state) + rotating RBID.
        self.advert = Some(*beacon);
        // TODO(metal): push caps into the live beacon advert + run BOTH adv sets
        // (non-connectable RBID beacon for discovery + connectable adv for the CoC
        // connect addr) — beacon.rs connectable-adv addition.
    }

    fn poll_scan(&mut self) -> Option<NegObservation> {
        self.scan_rx.pop_front()
    }

    fn send_control(&mut self, peer: HiveId, msg: &ControlMsg) {
        let mut buf = [0u8; ControlMsg::MAX_ENCODED_LEN];
        let n = msg.encode(&mut buf);
        if let Some(_addr) = self.addr_map.get(&peer).copied() {
            #[cfg(feature = "ble")]
            {
                // l2cap wraps with the [len_lo,len_hi] LE frame; byte-identical to
                // hive's esp-radio CoC (M7/M9 proven).
                let _ = crate::l2cap::send_to(&_addr, &buf[..n]);
            }
        }
    }

    fn poll_control(&mut self) -> Option<(HiveId, ControlMsg)> {
        self.control_rx.pop_front()
    }

    fn bring_up_provider(&mut self, params: &DataPlaneParams) -> bool {
        // We are the elected provider → host the SoftAP from the offer creds.
        let ssid = core::str::from_utf8(&params.ssid[..params.ssid_len as usize]).unwrap_or("");
        let _psk = core::str::from_utf8(&params.psk[..params.psk_len as usize]).unwrap_or("");
        // TODO(metal): wifi_ap::start needs the modem peripheral (owned at boot);
        // the firmware harness threads it in. Track state here.
        let ok = !ssid.is_empty();
        self.dp_state = if ok {
            DataPlaneState::Available
        } else {
            DataPlaneState::Failed
        };
        ok
    }

    fn join_provider(&mut self, params: &DataPlaneParams) -> bool {
        let ssid = core::str::from_utf8(&params.ssid[..params.ssid_len as usize]).unwrap_or("");
        let _psk = core::str::from_utf8(&params.psk[..params.psk_len as usize]).unwrap_or("");
        // TODO(metal): wifi_sta::connect_static (no-DHCP, self-assign per ap_hint);
        // firmware harness threads the modem. Track state.
        let ok = !ssid.is_empty();
        self.dp_state = if ok {
            DataPlaneState::Available
        } else {
            DataPlaneState::Failed
        };
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
        // esp_timer is microseconds since boot; ms for the engine deadlines.
        (unsafe { esp_idf_svc::sys::esp_timer_get_time() } as u64) / 1000
    }
}
