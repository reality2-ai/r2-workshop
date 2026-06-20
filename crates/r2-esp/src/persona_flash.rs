//! Persona-bundle flash reader (firmware glue) — the esp-only half of the
//! TG-provisioning reader.
//!
//! composer's `gen-persona` write-bins a RAW CBOR bundle at flash offset
//! 0x12000 (a reserved gap between `phy_init` and `ota_0`; NOT a partition —
//! espflash's part-table parser panics on custom subtypes, so it is read by raw
//! offset). This reads it and hands off to the shared, host-tested parser
//! [`r2_tn::persona::parse_persona`]. North-star: the SCHEMA + parser are shared
//! (r2-tn today); only this raw flash read is platform-specific.

use anyhow::{anyhow, Result};
use esp_idf_svc::sys;
use r2_tn::persona::{parse_persona, Persona};

/// Raw flash offset of the persona bundle (composer + hive convention).
pub const PERSONA_OFFSET: u32 = 0x12000;
/// Bytes to read — the bundle is ~180 B (map(7): tg_id + 4×32 + cert + ts);
/// the gap is up to 0x2000. 512 covers it; parse stops at the map's end.
const PERSONA_READ_LEN: usize = 512;

/// Read + decode the persona bundle from flash. `Err` if the read fails, the
/// region is erased (all 0xFF — unprovisioned), or the CBOR is malformed.
pub fn read_persona() -> Result<Persona> {
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
    parse_persona(&buf).map_err(|e| anyhow!("persona parse: {e}"))
}
