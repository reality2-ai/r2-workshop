//! WiFi/UDP peer transport — TRUE TN board-to-board (R2-ROUTE §1.4.4).
//!
//! The implementation is pure-std and host-tested in `r2-tn` (so it runs on
//! both Alfred and ESP-IDF/lwIP). This module re-exports it under the firmware's
//! conventional name so existing references keep working.
//! See `docs/tn-routeengine-smallest-path.md`.

pub use r2_tn::udp::{UdpTransport as WifiUdpTransport, R2_TN_UDP_PORT};
