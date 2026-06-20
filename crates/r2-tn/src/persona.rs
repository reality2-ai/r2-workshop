//! Persona-bundle reader — the device's trust-group identity, provisioned by
//! composer's `gen-persona` and written RAW (not NVS) at flash offset 0x12000.
//!
//! Contract (composer-owned, symmetric with composer's `build_persona_bundle` /
//! `bundle_round_trips_through_cbor_decoder`): an `r2_cbor` int-keyed map(7):
//!
//! | key | field                  | CBOR        |
//! |-----|------------------------|-------------|
//! | 0   | tg_id (UUID string)    | text        |
//! | 1   | dek                    | bytes(32)   |
//! | 2   | hk (group HMAC key)    | bytes(32)   |
//! | 3   | device_master_secret   | bytes(32)   |
//! | 4   | cert (EMPTY in v0.1)   | bytes       |
//! | 5   | tg_pk                  | bytes(32)   |
//! | 6   | issued_at              | uint        |
//!
//! The firmware reads the raw blob from flash (esp-storage, per-platform) and
//! calls [`parse_persona`]; [`Persona::trust_params`] derives the canonical
//! §6.2.1 wire `hive_id`, the `tg` hash, and hands back `hk` for `with_trust`.
//!
//! NORTH-STAR: this lives in workshop's `r2-tn` so it is host-testable now and is
//! a tested reference; if core/hive lift the parser into a shared canonical crate
//! (e.g. r2-trust, where `derive_hive_id` already lives), the firmware should
//! switch to calling that — the SCHEMA above is the durable contract.

use r2_cbor::{Decoder, Item};

/// A decoded persona bundle (trust-group identity material).
pub struct Persona {
    /// Trust-group id (UUID string) — `derive_hive_id` info + display label.
    pub tg_id: String,
    /// Data encryption key (32 bytes).
    pub dek: [u8; 32],
    /// Group HMAC key (32 bytes) — feeds `with_trust(hk)`.
    pub hk: [u8; 32],
    /// Device master secret (32 bytes) — `derive_hive_id` ikm.
    pub master_secret: [u8; 32],
    /// Device certificate (empty in v0.1).
    pub cert: Vec<u8>,
    /// Trust-group public key (32 bytes).
    pub tg_pk: [u8; 32],
    /// Issuance timestamp (unix seconds).
    pub issued_at: u64,
}

impl Persona {
    /// Derive the trust params the node needs: the canonical §6.2.1 wire
    /// `hive_id` (u32), the `tg` hash (= `fnv1a_32(tg_id)`, what peers gate on),
    /// and the group HMAC key. `hive_id` is byte-identical to composer's custody
    /// value (raw-hyphenated UUID, no v4 forcing — core abde165).
    pub fn trust_params(&self) -> Result<(u32, u32, [u8; 32]), &'static str> {
        let (_uuid, hive_id) = r2_trust::derive_hive_id(&self.master_secret, &self.tg_id)
            .map_err(|_| "derive_hive_id failed")?;
        let tg = r2_fnv::fnv1a_32(self.tg_id.as_bytes());
        Ok((hive_id, tg, self.hk))
    }
}

/// Read a 32-byte CBOR byte string from the decoder, else error.
fn read32(dec: &mut Decoder, what: &'static str) -> Result<[u8; 32], &'static str> {
    match dec.next().map_err(|_| "cbor")? {
        Item::Bytes(b) if b.len() == 32 => {
            let mut out = [0u8; 32];
            out.copy_from_slice(b);
            Ok(out)
        }
        Item::Bytes(_) => Err(what),
        _ => Err(what),
    }
}

/// Decode a persona bundle from its raw CBOR bytes (the blob at flash 0x12000).
pub fn parse_persona(blob: &[u8]) -> Result<Persona, &'static str> {
    let mut dec = Decoder::new(blob);
    match dec.next().map_err(|_| "cbor")? {
        Item::Map(7) => {}
        Item::Map(_) => return Err("persona: expected map(7)"),
        _ => return Err("persona: not a map"),
    }

    let (mut tg_id, mut dek, mut hk, mut ms, mut tg_pk, mut issued) =
        (None, None, None, None, None, None);
    let mut cert: Vec<u8> = Vec::new();

    for _ in 0..7 {
        let key = match dec.next().map_err(|_| "cbor")? {
            Item::UInt(k) => k,
            _ => return Err("persona: non-uint key"),
        };
        match key {
            0 => {
                tg_id = Some(match dec.next().map_err(|_| "cbor")? {
                    Item::Text(b) => core::str::from_utf8(b)
                        .map_err(|_| "persona: tg_id not utf8")?
                        .to_string(),
                    _ => return Err("persona: tg_id not text"),
                });
            }
            1 => dek = Some(read32(&mut dec, "persona: dek")?),
            2 => hk = Some(read32(&mut dec, "persona: hk")?),
            3 => ms = Some(read32(&mut dec, "persona: master_secret")?),
            4 => {
                cert = match dec.next().map_err(|_| "cbor")? {
                    Item::Bytes(b) => b.to_vec(),
                    _ => return Err("persona: cert not bytes"),
                };
            }
            5 => tg_pk = Some(read32(&mut dec, "persona: tg_pk")?),
            6 => {
                issued = Some(match dec.next().map_err(|_| "cbor")? {
                    Item::UInt(v) => v,
                    _ => return Err("persona: issued_at not uint"),
                });
            }
            _ => return Err("persona: unexpected key"),
        }
    }

    Ok(Persona {
        tg_id: tg_id.ok_or("persona: missing tg_id")?,
        dek: dek.ok_or("persona: missing dek")?,
        hk: hk.ok_or("persona: missing hk")?,
        master_secret: ms.ok_or("persona: missing master_secret")?,
        cert,
        tg_pk: tg_pk.ok_or("persona: missing tg_pk")?,
        issued_at: issued.ok_or("persona: missing issued_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2_cbor::{Encoder, Value};

    /// Build a persona blob the way composer's gen-persona does, then parse it —
    /// the round-trip that keeps the firmware reader symmetric with the producer.
    #[test]
    fn persona_round_trips_through_cbor() {
        let tg = "87810378-2095-0c4b-9e21-000000000001";
        let mut buf = [0u8; 256];
        let mut enc = Encoder::new(&mut buf);
        enc.map(7).unwrap();
        enc.kv(0, &Value::Text(tg)).unwrap();
        enc.kv(1, &Value::Bytes(&[0x11; 32])).unwrap();
        enc.kv(2, &Value::Bytes(&[0x22; 32])).unwrap();
        enc.kv(3, &Value::Bytes(&[0x33; 32])).unwrap();
        enc.kv(4, &Value::Bytes(&[])).unwrap();
        enc.kv(5, &Value::Bytes(&[0x55; 32])).unwrap();
        enc.kv(6, &Value::UInt(1_700_000_000)).unwrap();
        let n = enc.len();

        let p = parse_persona(&buf[..n]).unwrap();
        assert_eq!(p.tg_id, tg);
        assert_eq!(p.hk, [0x22; 32]);
        assert_eq!(p.master_secret, [0x33; 32]);
        assert_eq!(p.tg_pk, [0x55; 32]);
        assert!(p.cert.is_empty());
        assert_eq!(p.issued_at, 1_700_000_000);

        // trust_params: hive_id is deterministic, tg = fnv of the tg_id string.
        let (hive_id, tg_hash, hk) = p.trust_params().unwrap();
        assert_eq!(hk, [0x22; 32]);
        assert_eq!(tg_hash, r2_fnv::fnv1a_32(tg.as_bytes()));
        // Same master_secret + tg_id always derives the same wire id.
        let again = r2_trust::derive_hive_id(&[0x33; 32], tg).unwrap().1;
        assert_eq!(hive_id, again);
    }

    #[test]
    fn rejects_wrong_map_arity() {
        let mut buf = [0u8; 16];
        let mut enc = Encoder::new(&mut buf);
        enc.map(3).unwrap();
        enc.kv(0, &Value::UInt(1)).unwrap();
        enc.kv(1, &Value::UInt(2)).unwrap();
        enc.kv(2, &Value::UInt(3)).unwrap();
        let n = enc.len();
        assert!(parse_persona(&buf[..n]).is_err());
    }
}
