//! Persona-bundle flash reader (firmware glue) — the esp-only half of the
//! TG-provisioning reader.
//!
//! composer's `gen-persona` write-bins a RAW CBOR bundle at flash offset 0x12000
//! (a reserved gap between `phy_init` and `ota_0`; NOT a partition — espflash's
//! part-table parser panics on custom subtypes, so it is read by raw offset).
//! This reads it and hands off to the SHARED, PV1-locked parser
//! [`r2_trust::parse_persona`] (the one composer's producer is byte-locked to;
//! it does the r2_cbor decode + `derive_hive_id` + `tg_hash` internally). The
//! borrowed [`r2_trust::Persona`] is consumed here into the owned values the node
//! needs, so nothing escapes the read buffer.

use anyhow::{anyhow, Result};
use esp_idf_svc::sys;

/// The trust material a TN node needs from its persona: canonical wire `hive_id`,
/// the `tg` hash peers gate on, and the group HMAC key.
pub struct PersonaTrust {
    /// Canonical §6.2.1 wire hive id (FNV of the derived UUID).
    pub hive_id: u32,
    /// Trust-group hash (`fnv1a_32(tg_id)`) — `with_trust` tg.
    pub tg_hash: u32,
    /// Group HMAC key.
    pub hk: [u8; 32],
}

/// Raw flash offset of the persona bundle (composer + hive convention).
pub const PERSONA_OFFSET: u32 = 0x12000;
/// Bytes to read — the bundle is ~180 B; the gap is up to 0x2000. 512 covers it.
const PERSONA_READ_LEN: usize = 512;

/// Read + decode the persona bundle from flash, returning the owned trust
/// material. `Err` if the read fails, the region is erased (all 0xFF —
/// unprovisioned), or the CBOR is malformed.
pub fn read_persona() -> Result<PersonaTrust> {
    let mut buf = [0u8; PERSONA_READ_LEN];
    // chip = NULL → esp_flash_default_chip is substituted (per the IDF API).
    let rc = unsafe {
        sys::esp_flash_read(
            core::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            PERSONA_OFFSET,
            PERSONA_READ_LEN as u32,
        )
    };
    if rc != sys::ESP_OK as i32 {
        return Err(anyhow!("esp_flash_read @{PERSONA_OFFSET:#x} failed: {rc}"));
    }
    if buf.iter().all(|&b| b == 0xFF) {
        return Err(anyhow!("no persona at {PERSONA_OFFSET:#x} (erased flash)"));
    }
    // Shared PV1-locked parser; extract owned values before `buf` drops.
    let p = r2_trust::parse_persona(&buf).ok_or_else(|| anyhow!("persona parse failed"))?;
    Ok(PersonaTrust {
        hive_id: p.hive_id,
        tg_hash: p.tg_hash,
        hk: p.hk,
    })
}
