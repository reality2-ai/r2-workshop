//! R2 WiFi SoftAP mode — board-hosted access point for board-to-board TN.
//!
//! One board runs this SoftAP; peer boards join it as STA ([`crate::wifi_sta`]).
//! **AP↔STA has no client-isolation problem** (that only affects STA↔STA), so a
//! 2-board A↔B frame over the board-hosted AP is clean — no external router, no
//! host reconfiguration. See `docs/tn-routeengine-smallest-path.md`.

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, Configuration, EspWifi,
};
use log::{error, info};

/// Live SoftAP handle. Must be kept alive for the AP's lifetime (dropping it
/// tears the AP down).
pub struct WifiAp {
    #[allow(dead_code)]
    wifi: BlockingWifi<EspWifi<'static>>,
}

/// Start a SoftAP with the given SSID/PSK. Returns the handle plus the AP's
/// gateway IP (the address the local TN node binds its UDP socket on; ESP-IDF's
/// default SoftAP gateway is `192.168.71.1`). Empty password = open network.
pub fn start(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Option<(WifiAp, String)> {
    let esp_wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs)) {
        Ok(w) => w,
        Err(e) => {
            error!("[WIFI-AP] driver create failed: {e}");
            return None;
        }
    };
    let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop).ok()?;

    let auth_method = if password.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };
    let config = Configuration::AccessPoint(AccessPointConfiguration {
        ssid: ssid.try_into().unwrap_or_default(),
        password: password.try_into().unwrap_or_default(),
        auth_method,
        channel: 6,
        max_connections: 4,
        ..Default::default()
    });

    if let Err(e) = wifi.set_configuration(&config) {
        error!("[WIFI-AP] set_configuration failed: {e}");
        return None;
    }
    if let Err(e) = wifi.start() {
        error!("[WIFI-AP] start failed: {e}");
        return None;
    }
    if let Err(e) = wifi.wait_netif_up() {
        error!("[WIFI-AP] netif up failed: {e}");
        return None;
    }

    let ip = match wifi.wifi().ap_netif().get_ip_info() {
        Ok(info) => {
            info!("[WIFI-AP] ✅ SoftAP \"{}\" up — gateway {}", ssid, info.ip);
            format!("{}", info.ip)
        }
        Err(e) => {
            error!("[WIFI-AP] ip info failed: {e}");
            return None;
        }
    };
    Some((WifiAp { wifi }, ip))
}
