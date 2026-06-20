//! Persona bundle parser (consume side) — the single source for turning a
//! provisioned persona blob into usable trust material, mirroring composer's
//! producer. Both hive's DFR1195 firmware and workshop's C6 were hand-writing
//! the same ~40-line decode-glue over `r2_cbor` + [`derive_hive_id`]; this is
//! that glue, ONCE. The platform-specific flash read (e.g. esp-storage @0x12000)
//! stays in firmware; this takes the already-read bytes.
//!
//! Bundle schema (KS1, compact CBOR map with integer keys):
//! `{0: tg_id (text), 1: dek (bytes32), 2: hk (bytes32), 3: master_secret (bytes),
//!   4: cert (bytes), 5: tg_pk (bytes32), 6: issued_at (uint)}`.
//! `hive_id` + `tg_hash` are DERIVED here (not stored in the bundle) so the
//! consume side can never disagree with the producer on the addressing keys.

use r2_cbor::{Decoder, Item};
use zeroize::Zeroize;

use crate::hkdf::derive_hive_id;

/// Parsed persona — the trust material a hive needs to join + speak on its TG.
/// Public addressing fields + the SECRET group keys (`hk`/`dek`, zeroized on
/// drop, SEC-01). `master_secret` is consumed to derive `hive_id` and is NOT
/// retained. Borrows the text/byte fields from the input bundle.
pub struct Persona<'a> {
    /// Trust group UUID string (bundle key 0).
    pub tg_id: &'a str,
    /// Wire `target_group` = FNV-1a-32(tg_id) — the group addressing hash.
    pub tg_hash: u32,
    /// Wire `target_hive` = FNV-1a-32(derive_hive_id(master_secret, tg_id)).
    pub hive_id: u32,
    /// Trust group public key (bundle key 5).
    pub tg_pk: &'a [u8],
    /// Device certificate (bundle key 4) — composer ships it; may be ignored v0.1.
    pub cert: Option<&'a [u8]>,
    /// Intra-TG HMAC key (SECRET — GroupHmac).
    pub hk: [u8; 32],
    /// Payload encryption key (SECRET — group DEK).
    pub dek: [u8; 32],
}

impl Drop for Persona<'_> {
    fn drop(&mut self) {
        self.hk.zeroize();
        self.dek.zeroize();
    }
}

impl core::fmt::Debug for Persona<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Persona")
            .field("tg_id", &self.tg_id)
            .field("tg_hash", &format_args!("{:#010X}", self.tg_hash))
            .field("hive_id", &format_args!("{:#010X}", self.hive_id))
            .field("hk", &"[REDACTED; 32]")
            .field("dek", &"[REDACTED; 32]")
            .finish()
    }
}

fn bytes32(item: Item<'_>) -> Option<[u8; 32]> {
    match item {
        Item::Bytes(b) if b.len() == 32 => {
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Some(a)
        }
        _ => None,
    }
}

/// Parse a persona bundle (consume side). Returns `None` on any malformed /
/// missing-required-field bundle. The required fields are tg_id(0), dek(1),
/// hk(2), master_secret(3), tg_pk(5); cert(4) is optional, issued_at(6) ignored.
pub fn parse_persona(bytes: &[u8]) -> Option<Persona<'_>> {
    let mut d = Decoder::new(bytes);
    let n = match d.next().ok()? {
        Item::Map(n) => n,
        _ => return None,
    };

    let mut tg_id: Option<&str> = None;
    let mut dek: Option<[u8; 32]> = None;
    let mut hk: Option<[u8; 32]> = None;
    let mut master_secret: Option<&[u8]> = None;
    let mut cert: Option<&[u8]> = None;
    let mut tg_pk: Option<&[u8]> = None;

    for _ in 0..n {
        let key = match d.next().ok()? {
            Item::UInt(k) => k,
            _ => return None, // compact map keys are integers
        };
        let val = d.next().ok()?;
        match key {
            0 => match val {
                Item::Text(b) => tg_id = Some(core::str::from_utf8(b).ok()?),
                _ => return None,
            },
            1 => dek = Some(bytes32(val)?),
            2 => hk = Some(bytes32(val)?),
            3 => match val {
                Item::Bytes(b) => master_secret = Some(b),
                _ => return None,
            },
            4 => match val {
                Item::Bytes(b) => cert = Some(b),
                _ => return None,
            },
            5 => match val {
                Item::Bytes(b) => tg_pk = Some(b),
                _ => return None,
            },
            _ => {} // 6 issued_at + any future keys: ignored
        }
    }

    let tg_id = tg_id?;
    let (_, hive_id) = derive_hive_id(master_secret?, tg_id).ok()?;
    Some(Persona {
        tg_id,
        tg_hash: r2_fnv::fnv1a_32(tg_id.as_bytes()),
        hive_id,
        tg_pk: tg_pk?,
        cert,
        hk: hk?,
        dek: dek?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2_cbor::{Encoder, Value};

    fn unhex(s: &str) -> alloc::vec::Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn parse_persona_pv1_locks_consume_to_composer_producer() {
        // composer's byte-exact producer vector PV1 (hop confirmed all 6 fields).
        // master+tg_id = KS1, so the DERIVED hive_id MUST reproduce KS1's
        // 87810378-2095-0c4b-e827-228c80fd8cb3 / wire 0xd43b5801 — this locks
        // consume<->produce AND that derive_hive_id matches composer's derivation
        // byte-exact (not just the format).
        const PV1: &str = "a700782435353065383430302d653239622d343164342d613731362d343436363535343430303030015820d4fa97e56bc42c19ff494d5f8214d8ca75acbe614b9fb9f54b961abdb86162850258204e822f6c82f780814dc00e6f2c6541e91452ac112f391c504b0b779b5573349f035820aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa04400558207d59c5623dd40a74aa4d5a32ac645d3b3f95daeae4c22be25476dd6a486f7382061a698f50c0";
        let bundle = unhex(PV1);
        let p = parse_persona(&bundle).expect("PV1 must parse");

        assert_eq!(p.tg_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(p.hive_id, 0xd43b_5801, "hive_id must = KS1 wire d43b5801");
        assert_eq!(
            p.dek.as_slice(),
            unhex("d4fa97e56bc42c19ff494d5f8214d8ca75acbe614b9fb9f54b961abdb8616285").as_slice()
        );
        assert_eq!(
            p.hk.as_slice(),
            unhex("4e822f6c82f780814dc00e6f2c6541e91452ac112f391c504b0b779b5573349f").as_slice()
        );
        assert_eq!(
            p.tg_pk,
            unhex("7d59c5623dd40a74aa4d5a32ac645d3b3f95daeae4c22be25476dd6a486f7382").as_slice()
        );
        assert_eq!(p.cert, Some(&[][..]), "cert is present-but-empty in v0.1");

        // The KS1 derivation, directly: master=0xAA*32 + tg_id -> the KS1 uuid.
        let (uuid, wire) =
            derive_hive_id(&[0xAAu8; 32], "550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(
            core::str::from_utf8(&uuid).unwrap(),
            "87810378-2095-0c4b-e827-228c80fd8cb3"
        );
        assert_eq!(wire, 0xd43b_5801);
    }

    #[test]
    fn parse_persona_round_trips_a_ks1_bundle() {
        // Build a KS1-shaped bundle {0:tg_id,1:dek,2:hk,3:master_secret,4:cert,
        // 5:tg_pk,6:issued_at} with the encoder, then parse it.
        let tg_id = "4b3df45d-0000-0000-0000-000000000000";
        let ms = [0x42u8; 32];
        let dek = [0x11u8; 32];
        let hk = [0x22u8; 32];
        let tg_pk = [0x33u8; 32];
        let cert = [0xAAu8; 8];

        let mut buf = [0u8; 512];
        let used = {
            let mut e = Encoder::new(&mut buf);
            e.map(7).unwrap();
            e.kv(0, &Value::Text(tg_id)).unwrap();
            e.kv(1, &Value::Bytes(&dek)).unwrap();
            e.kv(2, &Value::Bytes(&hk)).unwrap();
            e.kv(3, &Value::Bytes(&ms)).unwrap();
            e.kv(4, &Value::Bytes(&cert)).unwrap();
            e.kv(5, &Value::Bytes(&tg_pk)).unwrap();
            e.kv(6, &Value::UInt(1_700_000_000)).unwrap();
            e.len()
        };

        let p = parse_persona(&buf[..used]).expect("bundle must parse");
        assert_eq!(p.tg_id, tg_id);
        assert_eq!(p.hk, hk);
        assert_eq!(p.dek, dek);
        assert_eq!(p.tg_pk, &tg_pk);
        assert_eq!(p.cert, Some(&cert[..]));
        // addressing keys are DERIVED — match the standalone derivations.
        assert_eq!(p.tg_hash, r2_fnv::fnv1a_32(tg_id.as_bytes()));
        let (_, wire) = derive_hive_id(&ms, tg_id).unwrap();
        assert_eq!(p.hive_id, wire);

        // malformed bundles → None (not a panic).
        assert!(parse_persona(&[]).is_none());
        assert!(parse_persona(&[0xFF]).is_none());
    }
}
