//! `r2.hb.health` telemetry (#18) — the fleet-dashboard health frame.
//!
//! Byte-identical to hive's shape per composer's HEALTH-TELEMETRY-CONTRACT
//! (af4ebcb): an int-keyed CBOR Compact map carried in an EXTENDED R2-WIRE frame,
//! event = `fnv1a_32("r2.hb.health")`, UNICAST to the collector (no flood),
//! cadence = every 5th originate tick (~15s) + on-change. Health is DECOUPLED
//! from heartbeat sync (set `sync_state=Free` until a conductor-PLL exists).
//!
//! The firmware (`r2_esp::tn`) fills a [`HealthReport`] from runtime state and
//! emits `encode()`d bytes via the node toward the collector hive id.

use r2_cbor::{Encoder, Value};

/// R2-WIRE event name for the health frame; the caller FNV-1a-32 hashes it.
pub const HEALTH_EVENT_NAME: &str = "r2.hb.health";

/// `role` bitset (key 2).
pub mod role {
    /// Heartbeat conductor.
    pub const CONDUCTOR: u8 = 1;
    /// Board-hosted SoftAP.
    pub const AP: u8 = 2;
    /// WiFi station.
    pub const STA: u8 = 4;
    /// Actively relaying for others.
    pub const RELAY: u8 = 8;
}

/// `transports` bitset (key 10).
pub mod transport_bit {
    /// WiFi/UDP.
    pub const WIFI: u8 = 1;
    /// LoRa.
    pub const LORA: u8 = 2;
    /// BLE.
    pub const BLE: u8 = 4;
    /// TCP/IP.
    pub const TCP: u8 = 8;
}

/// `sync_state` (key 7).
pub mod sync_state {
    /// No heartbeat sync (default until conductor-PLL).
    pub const FREE: u8 = 0;
    /// Acquiring lock.
    pub const SYNCING: u8 = 1;
    /// Phase-locked.
    pub const LOCKED: u8 = 2;
}

/// `ota_status` (key 6).
pub mod ota_status {
    /// Running the current release.
    pub const CURRENT: u8 = 0;
    /// An update is available.
    pub const NEEDS_UPDATE: u8 = 1;
    /// Update in progress.
    pub const UPDATING: u8 = 2;
    /// Last update failed (rolled back).
    pub const UPDATE_FAILED: u8 = 3;
}

/// A health snapshot (text fields borrowed to stay zero-alloc / no_std).
pub struct HealthReport<'a> {
    /// 0: this node's hive id.
    pub hive_id: u32,
    /// 1: trust-group id (FNV of tg uuid), 0 if untrusted.
    pub tg: u32,
    /// 2: role bitset ([`role`]).
    pub role: u8,
    /// 3: IP address (text).
    pub ip: &'a str,
    /// 4: firmware version (text).
    pub fw_version: &'a str,
    /// 5: firmware git sha (text).
    pub fw_sha: &'a str,
    /// 6: OTA status ([`ota_status`]).
    pub ota_status: u8,
    /// 7: heartbeat sync state ([`sync_state`]).
    pub sync_state: u8,
    /// 8: phase error in ms (SIGNED; 0 when free).
    pub phase_err_ms: i32,
    /// 9: link quality 0-100 (from RSSI/SNR).
    pub link_q: u8,
    /// 10: transports bitset ([`transport_bit`]).
    pub transports: u8,
    /// 11: uptime in seconds.
    pub uptime_s: u32,
    /// 12: monotonically increasing beat sequence.
    pub beat_seq: u32,
}

impl HealthReport<'_> {
    /// Encode the int-keyed CBOR Compact map into `buf`; returns the byte length.
    /// Keys 0..=12 per the contract, in order.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, r2_cbor::Error> {
        let mut enc = Encoder::new(buf);
        enc.map(13)?;
        enc.kv(0, &Value::UInt(self.hive_id as u64))?;
        enc.kv(1, &Value::UInt(self.tg as u64))?;
        enc.kv(2, &Value::UInt(self.role as u64))?;
        enc.kv(3, &Value::Text(self.ip))?;
        enc.kv(4, &Value::Text(self.fw_version))?;
        enc.kv(5, &Value::Text(self.fw_sha))?;
        enc.kv(6, &Value::UInt(self.ota_status as u64))?;
        enc.kv(7, &Value::UInt(self.sync_state as u64))?;
        // key 8 is SIGNED — encode negative phase error as CBOR negint.
        let phase = if self.phase_err_ms >= 0 {
            Value::UInt(self.phase_err_ms as u64)
        } else {
            Value::NegInt(self.phase_err_ms as i64)
        };
        enc.kv(8, &phase)?;
        enc.kv(9, &Value::UInt(self.link_q as u64))?;
        enc.kv(10, &Value::UInt(self.transports as u64))?;
        enc.kv(11, &Value::UInt(self.uptime_s as u64))?;
        enc.kv(12, &Value::UInt(self.beat_seq as u64))?;
        Ok(enc.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2_cbor::{Decoder, Item};

    #[test]
    fn health_encodes_contract_shape() {
        let r = HealthReport {
            hive_id: 0x4767_b7f3,
            tg: 0x7611_0001,
            role: role::AP | role::RELAY,
            ip: "192.168.71.1",
            fw_version: "0.3.0+abc12345",
            fw_sha: "abc12345",
            ota_status: ota_status::CURRENT,
            sync_state: sync_state::FREE,
            phase_err_ms: -7,
            link_q: 88,
            transports: transport_bit::WIFI | transport_bit::BLE,
            uptime_s: 1234,
            beat_seq: 42,
        };
        let mut buf = [0u8; 256];
        let n = r.encode(&mut buf).unwrap();
        // Definite map of exactly 13 pairs: 0xA0 | 13 = 0xAD.
        assert_eq!(buf[0], 0xAD, "must be a definite CBOR map of 13 pairs");

        // Round-trip: walk the map and check a representative spread of keys.
        let mut dec = Decoder::new(&buf[..n]);
        assert!(matches!(dec.next().unwrap(), Item::Map(13)));
        let mut hive_id = None;
        let mut ip = None;
        let mut role_v = None;
        let mut phase = None;
        let mut beat = None;
        for _ in 0..13 {
            let key = match dec.next().unwrap() {
                Item::UInt(k) => k,
                other => panic!("non-uint key: {other:?}"),
            };
            match (key, dec.next().unwrap()) {
                (0, Item::UInt(v)) => hive_id = Some(v),
                (2, Item::UInt(v)) => role_v = Some(v),
                (3, Item::Text(b)) => ip = Some(core::str::from_utf8(b).unwrap().to_string()),
                (8, Item::NegInt(v)) => phase = Some(v),
                (12, Item::UInt(v)) => beat = Some(v),
                _ => {}
            }
        }
        assert_eq!(hive_id, Some(0x4767_b7f3));
        assert_eq!(role_v, Some((role::AP | role::RELAY) as u64));
        assert_eq!(ip.as_deref(), Some("192.168.71.1"));
        assert_eq!(phase, Some(-7));
        assert_eq!(beat, Some(42));
    }
}
