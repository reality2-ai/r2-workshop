//! Leaderless-PCO HEARTBEAT frame builder (R2-HEARTBEAT v0.4) — the platform-tier
//! wire half of the cross-platform heterogeneity proof (workshop esp-idf board joins
//! hive's leaderless ESP-NOW mesh, #14).
//!
//! Byte-correct BY CONSTRUCTION via the shared `r2_wire` encoder + `r2_trust`
//! GroupHmac (hive's "don't re-implement" shortcut): both hive's firmware and this
//! board build the HB through the SAME `encode_extended`/`sign_extended`, so the
//! frames are byte-identical. The PCO *dynamics* (tick/couple) are the SHARED
//! portable engine core extracts from r2-harness (decision (A), one-codebase); this
//! module is only the FRAME the engine's `Fire` emits.
//!
//! Wire contract (r2-hive/docs/espnow-mesh-interop.md §3/§6c): msg_type=Heartbeat(5),
//! flags mcu_origin=1 + has_hmac=1 (has_route=0), ttl=1 k=1, event_hash=0,
//! payload_len=8, target_group=fnv1a_32(TG_UUID), target_hive=0,
//! payload = hive_id(BE4) || VERSION_FNV(BE4); then the 32B GroupHmac tag.
//! Partition gate: coupling happens ONLY if `verify_extended` passes with the TG key.

use r2_trust::wire_hmac::GroupHmac;
use r2_wire::{encode_extended, ExtendedHeader, ExtendedMessage, Flags, MsgType};
use r2_wire::hmac::sign_extended;

/// Canonical TG group ids (`fnv1a_32(TG_UUID)`) for the live 9-board mesh
/// (r2-hive/docs/espnow-mesh-interop.md). The board holds the matching GroupHmac
/// key to JOIN that TG's leaderless sync.
pub const TG_A_GROUP: u32 = 177_560_432;
pub const TG_B_GROUP: u32 = 1_584_099_016;

/// HB payload length: hive_id(4) + version_fnv(4).
pub const HB_PAYLOAD_LEN: usize = 8;
/// Total HB frame size: 22B extended header + 8B payload + 32B HMAC tag.
pub const HB_FRAME_LEN: usize = 22 + HB_PAYLOAD_LEN + 32;

/// Build + GroupHmac-sign a leaderless-PCO heartbeat frame into `buf` (must be
/// ≥ [`HB_FRAME_LEN`]); returns the encoded length (62). `version_fnv` =
/// `VERSION_FNV` the dashboard reads; `hk` = this node's TG GroupHmac key.
pub fn build_heartbeat(
    my_hive_id: u32,
    tg_group: u32,
    version_fnv: u32,
    msg_id: u32,
    hk: [u8; 32],
    buf: &mut [u8],
) -> Result<usize, r2_wire::WireError> {
    let mut payload = [0u8; HB_PAYLOAD_LEN];
    payload[0..4].copy_from_slice(&my_hive_id.to_be_bytes());
    payload[4..8].copy_from_slice(&version_fnv.to_be_bytes());

    let header = ExtendedHeader {
        version: 0,
        msg_type: MsgType::Heartbeat,
        // mcu_origin set now; sign_extended adds has_hmac (preserving mcu_origin).
        // has_route stays false — a HB carries no route stack.
        flags: Flags {
            mcu_origin: true,
            ..Default::default()
        },
        ttl: 1,
        k: 1,
        msg_id,
        event_hash: 0,
        payload_len: HB_PAYLOAD_LEN as u32,
        target_group: tg_group,
        target_hive: 0,
    };
    let mut msg = ExtendedMessage {
        header,
        route: None,
        payload: &payload,
        hmac_tag: None,
    };
    // Sign over the v0.6 span (msg_type||event_hash||target_group||target_hive||
    // payload) — the EXACT partition-gate span hive verifies. Sets has_hmac + tag.
    let (flags, tag) = sign_extended(&msg, &GroupHmac::new(hk));
    msg.header.flags = flags;
    msg.hmac_tag = Some(tag);
    encode_extended(&msg, buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2_wire::{decode_extended, hmac::verify_extended};

    const HIVE_ID: u32 = 0x4767_b7f3;
    const VERSION_FNV: u32 = 0x1234_5678;
    const HK: [u8; 32] = [0x5A; 32];

    // CONJECTURE: a HB built here is byte-identical to hive's documented wire
    // (§3/§6c). FALSIFIER: assert the exact header bytes + layout hive specified —
    // any drift in flags/msg_type/ttl/k/fields fails here, pre-metal.
    #[test]
    fn hb_frame_matches_hive_wire_contract() {
        let mut buf = [0u8; 64];
        let n = build_heartbeat(HIVE_ID, TG_A_GROUP, VERSION_FNV, 7, HK, &mut buf).unwrap();
        assert_eq!(n, HB_FRAME_LEN, "HB must be 62B (22 hdr + 8 payload + 32 hmac)");

        // byte0 = (ver<<6)|(msg_type<<3)|flags; ver=0, msg_type=5, flags=
        // mcu_origin(1)|has_hmac(2)|has_route(0) = 0b011. => 0x28 | 0x03 = 0x2B.
        assert_eq!(buf[0], 0x2B, "byte0: ver0 + Heartbeat(5)<<3 + flags(mcu|hmac)");
        // byte1 = (ttl<<4)|(k&0xF) = (1<<4)|1 = 0x11.
        assert_eq!(buf[1], 0x11, "byte1: ttl=1,k=1");
        // event_hash [6..10] = 0.
        assert_eq!(&buf[6..10], &[0, 0, 0, 0], "event_hash=0");
        // payload_len [10..14] BE = 8.
        assert_eq!(&buf[10..14], &8u32.to_be_bytes(), "payload_len=8");
        // target_group [14..18] BE = TG-A.
        assert_eq!(&buf[14..18], &TG_A_GROUP.to_be_bytes(), "target_group=TG-A");
        // target_hive [18..22] BE = 0 (broadcast).
        assert_eq!(&buf[18..22], &[0, 0, 0, 0], "target_hive=0");
        // payload [22..30] = hive_id(BE) || version_fnv(BE).
        assert_eq!(&buf[22..26], &HIVE_ID.to_be_bytes(), "payload[0..4]=hive_id");
        assert_eq!(&buf[26..30], &VERSION_FNV.to_be_bytes(), "payload[4..8]=version");
    }

    // CONJECTURE: the partition gate admits ONLY the matching TG key. FALSIFIER:
    // the right key verifies; a wrong key (different TG) must NOT — else cross-TG
    // coupling leaks (the isolation #14 proved on metal).
    #[test]
    fn partition_gate_admits_only_matching_tg_key() {
        let mut buf = [0u8; 64];
        let n = build_heartbeat(HIVE_ID, TG_A_GROUP, VERSION_FNV, 1, HK, &mut buf).unwrap();
        let msg = decode_extended(&buf[..n]).expect("HB decodes");
        assert!(
            verify_extended(&msg, &GroupHmac::new(HK)),
            "matching TG key must verify (couple)"
        );
        let wrong = [0xA5; 32];
        assert!(
            !verify_extended(&msg, &GroupHmac::new(wrong)),
            "wrong TG key must NOT verify (no cross-TG coupling)"
        );
    }
}
