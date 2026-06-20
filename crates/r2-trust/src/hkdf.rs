use alloc::vec::Vec;

use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;

use crate::error::Result;
use crate::types::KEY_LEN;

/// Derived trust group keys (DEK + HK).
#[derive(Clone, PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct DerivedGroupKeys {
    /// Data encryption key (for payload encryption within the trust group).
    pub dek: [u8; 32],
    /// HMAC key (for message authentication within the trust group).
    pub hk: [u8; 32],
}

impl core::fmt::Debug for DerivedGroupKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DerivedGroupKeys")
            .field("dek", &"[REDACTED; 32]")
            .field("hk", &"[REDACTED; 32]")
            .finish()
    }
}

/// Derived entanglement (peering) keys for cross-group communication.
#[derive(Clone, PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct PeeringKeys {
    /// HMAC key for cross-group message authentication.
    pub hmac: [u8; 32],
    /// Encryption key for cross-group payload encryption.
    pub enc: [u8; 32],
}

impl core::fmt::Debug for PeeringKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PeeringKeys")
            .field("hmac", &"[REDACTED; 32]")
            .field("enc", &"[REDACTED; 32]")
            .finish()
    }
}

/// Derive trust group DEK and HK from the signing key.
pub fn derive_group_keys(signing_key: &SigningKey) -> Result<DerivedGroupKeys> {
    let sk = signing_key.to_bytes();
    let pk = signing_key.verifying_key().to_bytes();
    derive_group_keys_raw(&sk, &pk)
}

/// Derive trust group keys from raw key bytes.
pub fn derive_group_keys_raw(
    trust_group_secret: &[u8; KEY_LEN],
    trust_group_public: &[u8; KEY_LEN],
) -> Result<DerivedGroupKeys> {
    let dek = derive_label(trust_group_secret, trust_group_public, b"R2-TRUST-v0.1-DEK")?;
    let hk = derive_label(
        trust_group_secret,
        trust_group_public,
        b"R2-TRUST-v0.1-HMAC",
    )?;
    Ok(DerivedGroupKeys { dek, hk })
}

/// Derive peering keys from the shared secret produced by X25519.
pub fn derive_peering_keys(
    shared_secret: &[u8; 32],
    trust_group_a: &[u8; KEY_LEN],
    trust_group_b: &[u8; KEY_LEN],
) -> Result<PeeringKeys> {
    // Lexicographic ordering ensures both sides derive the same keys (R2-TRUST §7.5).
    let (first, second) = if trust_group_a <= trust_group_b {
        (trust_group_a, trust_group_b)
    } else {
        (trust_group_b, trust_group_a)
    };
    let mut salt = Vec::with_capacity(KEY_LEN * 2);
    salt.extend_from_slice(first);
    salt.extend_from_slice(second);
    let hmac = hkdf_expand(shared_secret, &salt, b"R2-TRUST-v0.1-PEER-HMAC")?;
    let enc = hkdf_expand(shared_secret, &salt, b"R2-TRUST-v0.1-PEER-ENC")?;
    Ok(PeeringKeys { hmac, enc })
}

/// HKDF key derivation helper for trust group keys (info = label || TG_PK).
fn derive_label(
    trust_group_secret: &[u8; KEY_LEN],
    trust_group_public: &[u8; KEY_LEN],
    label: &[u8],
) -> Result<[u8; 32]> {
    let mut info = Vec::with_capacity(label.len() + KEY_LEN);
    info.extend_from_slice(label);
    info.extend_from_slice(trust_group_public);
    hkdf_expand(trust_group_secret, trust_group_public, &info)
}

pub(crate) fn hkdf_expand(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)?;
    Ok(okm)
}

// ── VENDOR RE-SYNC from r2-core canonical r2-trust @ abde165 (derive_hive_id) ──
// Single source of truth for the TG-scoped hive_id (R2-WIRE §6.2.1). composer
// (bundle producer) + hive (firmware) + workshop (C6 reader) MUST all call this
// so the hive_id can never drift. CRITICAL: RAW hyphenation, NOT RFC-4122-v4-bit-
// forced — canon = specs' KS1 vector (8781037820950c4b… → 87810378-2095-0c4b-…,
// NOT …-4c4b-…); v4-forcing would discard HKDF entropy + diverge from KS1.

/// Derive a hive's TG-scoped mesh identity (R2-WIRE §6.2.1).
/// `hive_id_bytes = HKDF-SHA256(ikm=master_secret, salt="r2-hive-id-v1",
/// info=tg_id)[0:16]`; UUID = raw 8-4-4-4-12 hyphenation (no v4 forcing);
/// `wire_u32 = FNV-1a-32(uuid_string)`. Deterministic + TG-scoped, alloc-free.
pub fn derive_hive_id(master_secret: &[u8], tg_id: &str) -> Result<([u8; 36], u32)> {
    let okm = hkdf_expand(master_secret, b"r2-hive-id-v1", tg_id.as_bytes())?;
    let mut b = [0u8; 16];
    b.copy_from_slice(&okm[..16]);
    let uuid = format_uuid_raw(&b);
    let wire = r2_fnv::fnv1a_32(&uuid);
    Ok((uuid, wire))
}

/// Hyphenate 16 bytes as a lowercase `8-4-4-4-12` UUID string (36 bytes) — RAW,
/// no version/variant bit forcing (canon: matches specs' KS1 vector).
fn format_uuid_raw(b: &[u8; 16]) -> [u8; 36] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 36];
    let mut oi = 0usize;
    for (i, &byte) in b.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            out[oi] = b'-';
            oi += 1;
        }
        out[oi] = HEX[(byte >> 4) as usize];
        out[oi + 1] = HEX[(byte & 0x0F) as usize];
        oi += 2;
    }
    out
}
