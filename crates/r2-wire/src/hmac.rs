//! HMAC envelope for authenticated messaging (R2-WIRE §10, R2-TRUST §6).
//!
//! The wire protocol authenticates only **immutable** fields — TTL, K,
//! msg_id, and route_stack are mutable (relay nodes change them) and
//! explicitly excluded from the HMAC input.
//!
//! ## Authenticated bytes
//!
//! **Compact:** `type(1) || event_hash(4) || target(4) || payload(N)`
//!
//! **Extended:** `type(1) || event_hash(4) || target_group(4) || target_hive(4) || payload(N)`
//!
//! ## Tag sizes
//!
//! - Compact: 8 bytes (truncated HMAC-SHA256)
//! - Extended: 32 bytes (full HMAC-SHA256)
//!
//! ## Usage
//!
//! The [`HmacProvider`] trait is crypto-agnostic — `r2-wire` defines *what*
//! to authenticate, the caller supplies *how*. `r2-trust` provides the
//! concrete implementation using HKDF-derived keys.

use crate::types::{CompactMessage, ExtendedMessage, Flags};

/// Compact HMAC tag size — truncated to first 8 bytes of HMAC-SHA256.
pub const COMPACT_TAG_LEN: usize = 8;
/// Extended HMAC tag size — full 32-byte HMAC-SHA256 output.
pub const EXTENDED_TAG_LEN: usize = 32;

/// Maximum authenticated-bytes buffer for compact messages.
///
/// 1 (type) + 2 (msg_id) + 4 (event_hash) + 4 (target) + 180 (max CBOR compact)
/// = 191 (R2-WIRE v0.6: msg_id bound into the span).
const COMPACT_AUTH_MAX: usize = 1 + 2 + 4 + 4 + 180;

/// Maximum authenticated-bytes buffer for extended messages: header
/// `type(1)+msg_id(4)+event_hash(4)+target_group(4)+target_hive(4)` = 17, plus a
/// 4 KB stack payload bound (R2-WIRE v0.6).
const EXT_AUTH_MAX: usize = 17 + 4096;

/// Crypto-agnostic HMAC provider (R2-WIRE §10.3).
///
/// Implementors compute HMAC-SHA256 over the authenticated bytes and return
/// the tag. The trait has no dependencies on any crypto crate — `r2-wire`
/// only defines the interface.
///
/// # Constant-time requirement
///
/// [`verify_compact`] and [`verify_extended`] use the provider's output
/// and perform constant-time comparison. Implementations SHOULD also use
/// constant-time MAC finalization internally.
pub trait HmacProvider {
    /// Compute truncated 8-byte HMAC tag for compact frames.
    fn mac_compact(&self, authenticated_bytes: &[u8]) -> [u8; COMPACT_TAG_LEN];

    /// Compute full 32-byte HMAC tag for extended frames.
    fn mac_extended(&self, authenticated_bytes: &[u8]) -> [u8; EXTENDED_TAG_LEN];
}

// ---------------------------------------------------------------------------
// Authenticated bytes extraction
// ---------------------------------------------------------------------------

/// Build the authenticated byte sequence for a compact message (R2-WIRE §10.2).
///
/// Returns the number of bytes written into `buf`.
///
/// Layout (R2-WIRE v0.6 §10.2): `type(1) || msg_id(2 BE) || event_hash(4) ||
/// target(4) || payload(N)`. `msg_id` is bound into the span (relays preserve it,
/// so it is NOT mutable — binding it closes the rewrite-to-bypass-dedup replay
/// vector); TTL/K/route stay EXCLUDED (genuinely mutable).
pub fn authenticated_bytes_compact(msg: &CompactMessage<'_>, buf: &mut [u8]) -> usize {
    let payload_len = msg.payload.len();
    let total = 1 + 2 + 4 + 4 + payload_len;
    debug_assert!(buf.len() >= total);

    buf[0] = msg.header.msg_type as u8;
    buf[1..3].copy_from_slice(&msg.header.msg_id.to_be_bytes());
    buf[3..7].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[7..11].copy_from_slice(&msg.header.target.to_be_bytes());
    buf[11..11 + payload_len].copy_from_slice(msg.payload);
    total
}

/// Build the authenticated byte sequence for an extended message (R2-WIRE §10.2).
///
/// Returns the number of bytes written into `buf`.
///
/// Layout (R2-WIRE v0.6 §10.2): `type(1) || msg_id(4 BE) || event_hash(4) ||
/// target_group(4) || target_hive(4) || payload(N)`. `msg_id` bound into the span
/// (preserved by relays → not mutable; closes the replay vector); TTL/K/route
/// stay EXCLUDED (mutable).
pub fn authenticated_bytes_extended(msg: &ExtendedMessage<'_>, buf: &mut [u8]) -> usize {
    let payload_len = msg.payload.len();
    let total = 1 + 4 + 4 + 4 + 4 + payload_len;
    debug_assert!(buf.len() >= total);

    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.msg_id.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[9..13].copy_from_slice(&msg.header.target_group.to_be_bytes());
    buf[13..17].copy_from_slice(&msg.header.target_hive.to_be_bytes());
    buf[17..17 + payload_len].copy_from_slice(msg.payload);
    total
}

/// MAC the extended authenticated span into a build-mode-adaptive buffer.
/// `alloc` (host/cloud): heap-alloc the exact size — unbounded payload. `no_std`
/// (MCU): a bounded `EXT_AUTH_MAX` stack buffer (callers guard oversize). Both
/// route through the single-source `authenticated_bytes_extended` (one span,
/// includes the v0.6 msg_id). Folds workshop's alloc/no_std improvement.
fn mac_extended_span(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
) -> [u8; EXTENDED_TAG_LEN] {
    #[cfg(feature = "alloc")]
    {
        let total = 1 + 4 + 4 + 4 + 4 + msg.payload.len();
        let mut buf = alloc::vec![0u8; total];
        let n = authenticated_bytes_extended(msg, &mut buf);
        hmac.mac_extended(&buf[..n])
    }
    #[cfg(not(feature = "alloc"))]
    {
        let mut buf = [0u8; EXT_AUTH_MAX];
        let n = authenticated_bytes_extended(msg, &mut buf);
        hmac.mac_extended(&buf[..n])
    }
}

// ---------------------------------------------------------------------------
// Sign (apply HMAC tag to a message)
// ---------------------------------------------------------------------------

/// Compute and attach the HMAC tag to a compact message.
///
/// Returns a new `Flags` with `has_hmac = true` and the 8-byte tag.
/// The caller should set `msg.header.flags = flags` and `msg.hmac_tag = Some(tag)`
/// before encoding, or use the returned pair directly.
pub fn sign_compact(
    msg: &CompactMessage<'_>,
    hmac: &impl HmacProvider,
) -> (Flags, [u8; COMPACT_TAG_LEN]) {
    let mut auth_buf = [0u8; COMPACT_AUTH_MAX];
    let len = authenticated_bytes_compact(msg, &mut auth_buf);
    let tag = hmac.mac_compact(&auth_buf[..len]);
    let flags = Flags {
        has_hmac: true,
        ..msg.header.flags
    };
    (flags, tag)
}

/// Compute and attach the HMAC tag to an extended message.
///
/// Returns a new `Flags` with `has_hmac = true` and the 32-byte tag.
pub fn sign_extended(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
) -> (Flags, [u8; EXTENDED_TAG_LEN]) {
    // Span (v0.6) = type+msg_id+event_hash+2×target+payload — built ONCE by the
    // single-source authenticated_bytes_extended. Buffer is build-mode adaptive
    // (workshop's improvement): `alloc` heap-allocs the exact size (host/cloud,
    // unbounded payload); `no_std` stack-allocs the bounded EXT_AUTH_MAX (MCU).
    #[cfg(not(feature = "alloc"))]
    debug_assert!(
        1 + 4 + 4 + 4 + 4 + msg.payload.len() <= EXT_AUTH_MAX,
        "extended payload too large for no_std stack HMAC"
    );
    let tag = mac_extended_span(msg, hmac);
    let flags = Flags {
        has_hmac: true,
        ..msg.header.flags
    };
    (flags, tag)
}

// ---------------------------------------------------------------------------
// Verify (check HMAC tag on a received message)
// ---------------------------------------------------------------------------

/// Verify the HMAC tag on a compact message.
///
/// Returns `true` if the tag matches (constant-time comparison).
/// Returns `false` if no tag is present or the tag doesn't match.
pub fn verify_compact(msg: &CompactMessage<'_>, hmac: &impl HmacProvider) -> bool {
    let received_tag = match msg.hmac_tag {
        Some(tag) => tag,
        None => return false,
    };

    let mut auth_buf = [0u8; COMPACT_AUTH_MAX];
    let len = authenticated_bytes_compact(msg, &mut auth_buf);
    let expected = hmac.mac_compact(&auth_buf[..len]);

    constant_time_eq(&received_tag, &expected)
}

/// Verify the HMAC tag on an extended message.
///
/// Returns `true` if the tag matches (constant-time comparison).
/// Returns `false` if no tag is present or the tag doesn't match.
pub fn verify_extended(msg: &ExtendedMessage<'_>, hmac: &impl HmacProvider) -> bool {
    let received_tag = match msg.hmac_tag {
        Some(tag) => tag,
        None => return false,
    };

    // no_std verification is bounded by the stack buffer; alloc handles any size.
    #[cfg(not(feature = "alloc"))]
    if 1 + 4 + 4 + 4 + 4 + msg.payload.len() > EXT_AUTH_MAX {
        return false; // too large for stack verification
    }

    // Single-source span (v0.6, includes msg_id) — same builder as sign_extended.
    let expected = mac_extended_span(msg, hmac);
    constant_time_eq(&received_tag, &expected)
}

// ---------------------------------------------------------------------------
// Frame classification (R2-TRUST §6.3)
// ---------------------------------------------------------------------------

/// Inbound frame classification (R2-TRUST §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// HMAC verified with trust group key — same trust group.
    SameGroup,
    /// HMAC present but no matching key — relay opaquely.
    Relay,
    /// No HMAC tag (H flag = 0) — unauthenticated.
    Unauthenticated,
}

/// Classify an inbound compact frame (R2-TRUST §6.3).
///
/// - `group_hmac`: the trust group's HMAC provider (if this device is a member).
///
/// Returns `None` if the HMAC is present but **invalid** (frame MUST be dropped).
pub fn classify_compact(
    msg: &CompactMessage<'_>,
    group_hmac: Option<&impl HmacProvider>,
) -> Option<FrameClass> {
    if msg.hmac_tag.is_none() {
        return Some(FrameClass::Unauthenticated);
    }

    // HMAC is present. Try to verify.
    match group_hmac {
        Some(hmac) => {
            if verify_compact(msg, hmac) {
                Some(FrameClass::SameGroup)
            } else {
                None // Invalid HMAC — drop frame
            }
        }
        None => {
            // We have no key for this group — forward opaquely.
            Some(FrameClass::Relay)
        }
    }
}

/// Classify an inbound extended frame (R2-TRUST §6.3).
///
/// Same semantics as [`classify_compact`].
pub fn classify_extended(
    msg: &ExtendedMessage<'_>,
    group_hmac: Option<&impl HmacProvider>,
) -> Option<FrameClass> {
    if msg.hmac_tag.is_none() {
        return Some(FrameClass::Unauthenticated);
    }

    match group_hmac {
        Some(hmac) => {
            if verify_extended(msg, hmac) {
                Some(FrameClass::SameGroup)
            } else {
                None
            }
        }
        None => Some(FrameClass::Relay),
    }
}

// ---------------------------------------------------------------------------
// Constant-time comparison (R2-WIRE §10.6 step 3)
// ---------------------------------------------------------------------------

/// Constant-time byte slice equality (no early exit on mismatch).
#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
