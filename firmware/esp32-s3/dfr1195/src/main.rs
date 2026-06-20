//! r2-workshop TRUE-TN node firmware — DFR1195 / ESP32-S3 (4 MB, no PSRAM).
//!
//! Minimal board-to-board TN node: bring up WiFi STA, then run core's
//! RouteEngine over the WiFi/UDP peer transport (`r2_esp::tn`) so frames route
//! peer-to-peer — NOT through the dashboard hub. No sensor / SD / BLE.
//! See `docs/tn-routeengine-smallest-path.md`.
//!
//! Build-time config (baked via build.rs / env):
//!   * `R2_WIFI_SSID` / `R2_WIFI_PASS`  — network the node joins (wifi_config.toml).
//!   * `R2_TN_MY_ID`   (hex u32)  — this node's hive id (else derived from MAC).
//!   * `R2_TN_PEER_ID` (hex u32)  — a peer to seed + (optionally) originate to.
//!   * `R2_TN_PEER_IP` / `R2_TN_PEER_PORT` — peer address (or subnet broadcast).
//!   * `R2_TN_ORIGINATE=1` — periodically originate a hello frame to the peer.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use anyhow::{anyhow, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{
    esp_mac_type_t_ESP_MAC_WIFI_STA, esp_read_mac, link_patches,
};
use log::{info, warn};
use r2_esp::{tn, wifi_prov, wifi_sta};

fn main() -> Result<()> {
    link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("==================================================");
    info!("r2-workshop TN node (dfr1195 / ESP32-S3 4MB) v{}", env!("CARGO_PKG_VERSION"));
    info!("==================================================");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    wifi_prov::init_nvs(nvs.clone());
    let creds = wifi_prov::load_credentials(nvs.clone());

    let _wifi = match creds {
        Some(c) => {
            info!("[boot] WiFi creds (source={:?}) — joining \"{}\"", c.source, c.ssid);
            match wifi_sta::connect(peripherals.modem, sysloop.clone(), nvs.clone(), &c.ssid, &c.password) {
                Some(w) => Some(w),
                None => {
                    warn!("[boot] WiFi connect failed — TN needs a network; idling");
                    None
                }
            }
        }
        None => {
            warn!("[boot] no WiFi creds baked (set R2_WIFI_SSID/PASS) — TN needs a network; idling");
            None
        }
    };

    let local_ip = wifi_sta::get_ip().unwrap_or_default();
    info!("[boot] local IP = \"{}\"", local_ip);

    if !local_ip.is_empty() {
        match start_tn(&local_ip) {
            Ok(()) => info!("[tn] run-loop started"),
            Err(e) => warn!("[tn] not started: {e:?}"),
        }
    }

    // Keep the main thread alive; the TN loop runs on its own thread.
    loop {
        FreeRtos::delay_ms(60_000);
    }
}

/// Build the TN config from baked env + the runtime IP, then spawn the loop.
fn start_tn(local_ip: &str) -> Result<()> {
    let ip: Ipv4Addr = local_ip.parse()?;
    let my_hive_id = match option_env!("R2_TN_MY_ID") {
        Some(s) => parse_hex_u32(s)?,
        None => mac_hive_id(),
    };
    info!("[tn] my_hive_id = {:08x}", my_hive_id);

    let mut peers = Vec::new();
    let mut originate_to = None;
    if let (Some(pid), Some(pip)) = (option_env!("R2_TN_PEER_ID"), option_env!("R2_TN_PEER_IP")) {
        let peer_id = parse_hex_u32(pid)?;
        let peer_ip: Ipv4Addr = pip.parse().map_err(|e| anyhow!("R2_TN_PEER_IP: {e}"))?;
        let port = option_env!("R2_TN_PEER_PORT")
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(r2_esp::peer_wifi_udp::R2_TN_UDP_PORT);
        peers.push((peer_id, SocketAddr::V4(SocketAddrV4::new(peer_ip, port))));
        if matches!(option_env!("R2_TN_ORIGINATE"), Some("1") | Some("true")) {
            originate_to = Some(peer_id);
        }
    }

    let event_hash = r2_core::fnv::r2_hash("r2.tn.hello").unwrap_or(0);
    tn::spawn(tn::TnConfig {
        my_hive_id,
        local_ip: ip,
        peers,
        originate_to,
        originate_period: core::time::Duration::from_secs(3),
        event_hash,
        payload: b"hello-tn".to_vec(),
    })
}

/// Parse a hex u32 ("0xABCD1234" or "ABCD1234").
fn parse_hex_u32(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16)
        .map_err(|e| anyhow!("not a hex u32 ({s:?}): {e}"))
}

/// Derive a stable hive id from the WiFi-STA MAC (FNV-1a of its hex string)
/// when `R2_TN_MY_ID` isn't baked.
fn mac_hive_id() -> u32 {
    let mut mac = [0u8; 6];
    unsafe { esp_read_mac(mac.as_mut_ptr(), esp_mac_type_t_ESP_MAC_WIFI_STA) };
    let s = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    r2_core::fnv::r2_hash(&s).unwrap_or(0)
}
