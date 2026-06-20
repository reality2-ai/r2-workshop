//! r2-workshop TRUE-TN node firmware — DFR1195 / ESP32-S3 (4 MB, no PSRAM).
//!
//! Board-to-board TN node using the **board-hosted SoftAP** pattern: ONE image,
//! role auto-selected by MAC. The board whose MAC matches the baked
//! `R2_TN_AP_MAC` becomes the **AP**; every other board joins it as **STA**.
//! AP↔STA has no client-isolation problem, so a 2-board frame is clean with no
//! external router / no host reconfiguration. The STA originates a frame to the
//! AP (the AP's gateway IP is fixed), the AP delivers it locally → the hardware
//! frame. Frames route via core's RouteEngine over WiFi/UDP (`r2_esp::tn`).
//! See `docs/tn-routeengine-smallest-path.md`.
//!
//! Build-time config (baked via env / build.rs):
//!   * `R2_TN_AP_MAC`  — MAC of the board that should host the AP (role select).
//!   * `R2_TN_AP_SSID` / `R2_TN_AP_PSK` — AP creds. STANDALONE: the board-hosted
//!     SoftAP we create. FIELDED: an EXTERNAL AP to join (e.g. hive's r2-fieldlab).
//!   * `R2_TN_AP_ID`   (hex u32, optional) — the AP board's CANONICAL hive id.
//!     Presence selects FIELDED mode: this node uses the §6.2.1 canonical hive_id
//!     and targets this baked AP id (a canonical id is not MAC-derivable). Absent
//!     = STANDALONE mode (MAC-FNV ids on both sides).
//!   * `R2_TN_MY_ID`   (hex u32, optional) — override this node's hive id.
//!   * `R2_TN_UDP_PORT` (optional) — peer UDP port (default 21050).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use anyhow::{anyhow, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{esp_mac_type_t_ESP_MAC_WIFI_STA, esp_read_mac, link_patches};
use log::{info, warn};
use r2_esp::{hive_id, ota_tcp, tn, wifi_ap, wifi_sta};

const AP_SSID: &str = match option_env!("R2_TN_AP_SSID") {
    Some(s) => s,
    None => "r2-tn-lab",
};
const AP_PSK: &str = match option_env!("R2_TN_AP_PSK") {
    Some(s) => s,
    None => "r2tnlab1234",
};

/// Firmware version for telemetry (#18): `<semver>+<git-sha>`, baked by build.rs.
const FW_VER: &str = match option_env!("R2_FW_VER") {
    Some(s) => s,
    None => env!("CARGO_PKG_VERSION"),
};
/// Firmware git sha for telemetry (#18 key 5), baked by build.rs.
const GIT_SHA: &str = match option_env!("R2_GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

fn main() -> Result<()> {
    link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("==================================================");
    info!("r2-workshop TN node (c6-tn / ESP32-C6 4MB, RISC-V) fw_ver={}", FW_VER);
    info!("==================================================");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let my_mac = read_mac_str();
    let ap_mac = option_env!("R2_TN_AP_MAC").unwrap_or("");
    let is_ap = !ap_mac.is_empty() && normalize_mac(&my_mac) == normalize_mac(ap_mac);

    // hive_id derivation, two modes:
    //  • FIELDED (R2_TN_AP_ID baked = joining an external AP, e.g. hive's
    //    r2-fieldlab): CANONICAL §6.2.1 hive_id = fnv1a_32 of the persisted UUID
    //    identity (mints master_secret + TG-of-one in NVS on first boot). The AP
    //    id is BAKED — a canonical UUID id is not MAC-derivable, so a STA can't
    //    compute hive's AP id; hive supplies it.
    //  • STANDALONE (no R2_TN_AP_ID = board-hosted r2-tn-lab): MAC-FNV ids on
    //    both sides so a STA derives the AP id from its MAC without baking.
    // R2_TN_MY_ID always overrides. (Replaces the unconditional MAC hand-roll per
    // R2-WIRE §6.2.1 canon, while keeping the standalone demo derivable.)
    let (my_hive_id, ap_hive_id) = match option_env!("R2_TN_AP_ID") {
        Some(ap_id) => {
            let my = match option_env!("R2_TN_MY_ID") {
                Some(s) => parse_hex_u32(s)?,
                None => {
                    let id = hive_id::load_identity(nvs.clone())?;
                    info!("[boot] canonical §6.2.1 hive_id_uuid={}", id.hive_id_uuid);
                    r2_core::fnv::r2_hash(&id.hive_id_uuid).unwrap_or(0)
                }
            };
            (my, parse_hex_u32(ap_id)?)
        }
        None => {
            let my = match option_env!("R2_TN_MY_ID") {
                Some(s) => parse_hex_u32(s)?,
                None => fnv_hex(&my_mac),
            };
            (my, fnv_hex(&normalize_mac(ap_mac)))
        }
    };

    info!("[boot] my_mac={my_mac} my_hive_id={my_hive_id:08x} role={}",
          if is_ap { "AP" } else { "STA" });

    let port = option_env!("R2_TN_UDP_PORT")
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(r2_esp::peer_wifi_udp::R2_TN_UDP_PORT);

    if is_ap {
        // ── AP role: host the SoftAP, then listen (STA originates to us). ──
        let (_ap, local_ip) =
            wifi_ap::start(peripherals.modem, sysloop, nvs, AP_SSID, AP_PSK)
                .ok_or_else(|| anyhow!("SoftAP start failed"))?;
        info!("[boot] AP up; binding TN on {local_ip}");
        let ip: Ipv4Addr = local_ip.parse()?;
        tn::spawn(tn::TnConfig {
            my_hive_id,
            local_ip: ip,
            peers: Vec::new(),       // STAs are learned on receipt
            originate_to: None,      // AP listens; the frame is STA -> AP
            originate_period: core::time::Duration::from_secs(3),
            event_hash: hello_event(),
            payload: b"hello-tn".to_vec(),
            // #18: the AP IS the collector (aggregates + forwards) — no self-emit.
            collector: None,
            health_event: health_event(),
            health_every: 5,
            role: r2_esp::health::role::AP,
            tg: 0,
            fw_version: FW_VER,
            fw_sha: GIT_SHA,
        })?;
        // Task #17: network-OTA receiver (recv over WiFi -> verify sha ->
        // write inactive OTA slot -> reboot). #18: CMD_QUERY serves fw version.
        ota_tcp::start_listener();
        // Anti-brick: this image proved boot + WiFi/AP + node up = healthy.
        ota_tcp::mark_app_valid();
        info!("[boot] OTA receiver listening (TCP 21043); image marked valid");
        // Keep the AP handle alive for the process lifetime.
        loop {
            FreeRtos::delay_ms(60_000);
        }
    } else {
        // ── STA role: join the board-hosted AP, originate to the AP board. ──
        match wifi_sta::connect(peripherals.modem, sysloop, nvs, AP_SSID, AP_PSK) {
            Some(_w) => {
                let local_ip = wifi_sta::get_ip().unwrap_or_default();
                info!("[boot] STA joined \"{AP_SSID}\"; local IP={local_ip}");
                if local_ip.is_empty() {
                    warn!("[boot] no IP from AP — cannot run TN");
                } else {
                    let ip: Ipv4Addr = local_ip.parse()?;
                    // The AP board's IP = the STA's default gateway. Read it
                    // from the netif rather than hardcoding — esp-idf-svc's
                    // SoftAP is 192.168.71.1, embassy-net's is 192.168.4.1;
                    // the gateway is authoritative regardless of stack.
                    let ap_ip: Ipv4Addr = wifi_sta::get_gateway()
                        .and_then(|g| g.parse().ok())
                        .unwrap_or(Ipv4Addr::new(192, 168, 71, 1));
                    let ap_addr = SocketAddr::V4(SocketAddrV4::new(ap_ip, port));
                    // Originate target: default = the AP (clean STA->AP delivery).
                    // RELAY variant (Roy #19): if both STA MACs are baked, target
                    // the OTHER STA — the STA floods to the AP (its only seeded
                    // peer), the AP RELAYS to the other STA (RouteEngine + learned
                    // addr). One image; which STA I am is decided by MAC.
                    let originate_to = relay_peer_hive_id(&my_mac).unwrap_or(ap_hive_id);
                    info!("[boot] STA seeds AP {ap_hive_id:08x} @ {ap_addr}; originate_to={originate_to:08x}");
                    tn::spawn(tn::TnConfig {
                        my_hive_id,
                        local_ip: ip,
                        peers: vec![(ap_hive_id, ap_addr)],
                        originate_to: Some(originate_to),
                        originate_period: core::time::Duration::from_secs(3),
                        event_hash: hello_event(),
                        payload: b"hello-tn".to_vec(),
                        // #18: STA unicasts r2.hb.health to the AP collector.
                        collector: Some(ap_hive_id),
                        health_event: health_event(),
                        health_every: 5,
                        role: r2_esp::health::role::STA,
                        tg: 0,
                        fw_version: FW_VER,
                        fw_sha: GIT_SHA,
                    })?;
                    // Task #17: network-OTA receiver + #18 version query (CMD_QUERY).
                    ota_tcp::start_listener();
                    // Anti-brick: boot + WiFi join + node up = healthy.
                    ota_tcp::mark_app_valid();
                    info!("[boot] OTA receiver listening (TCP 21043); image marked valid");
                }
                loop {
                    FreeRtos::delay_ms(60_000);
                }
            }
            None => {
                warn!("[boot] STA could not join \"{AP_SSID}\" — is the AP board up? idling");
                loop {
                    FreeRtos::delay_ms(60_000);
                }
            }
        }
    }
}

fn hello_event() -> u32 {
    r2_core::fnv::r2_hash("r2.tn.hello").unwrap_or(0)
}

/// #18 health event hash = fnv1a_32("r2.hb.health").
fn health_event() -> u32 {
    r2_core::fnv::r2_hash(r2_esp::health::HEALTH_EVENT_NAME).unwrap_or(0)
}

/// Read the WiFi-STA MAC as a lowercase colon-separated string.
fn read_mac_str() -> String {
    let mut mac = [0u8; 6];
    unsafe { esp_read_mac(mac.as_mut_ptr(), esp_mac_type_t_ESP_MAC_WIFI_STA) };
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Normalize a MAC for comparison: lowercase, strip separators.
fn normalize_mac(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect()
}

/// FNV-1a-32 of a MAC's normalized hex (matches both nodes' AP-id derivation).
fn fnv_hex(mac: &str) -> u32 {
    r2_core::fnv::r2_hash(&normalize_mac(mac)).unwrap_or(0)
}

fn parse_hex_u32(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16)
        .map_err(|e| anyhow!("not a hex u32 ({s:?}): {e}"))
}

/// RELAY variant (Roy #19): if both STA MACs are baked (R2_TN_STA_A_MAC /
/// R2_TN_STA_B_MAC) and my MAC is one of them, return the OTHER STA's hive id
/// so this STA originates to its peer STA (the AP relays). One image, symmetric.
/// Returns None in the default variant (no STA pair baked) → STA targets the AP.
fn relay_peer_hive_id(my_mac: &str) -> Option<u32> {
    let a = option_env!("R2_TN_STA_A_MAC")?;
    let b = option_env!("R2_TN_STA_B_MAC")?;
    let me = normalize_mac(my_mac);
    if me == normalize_mac(a) {
        Some(fnv_hex(b))
    } else if me == normalize_mac(b) {
        Some(fnv_hex(a))
    } else {
        None
    }
}
