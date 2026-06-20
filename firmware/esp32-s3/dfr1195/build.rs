use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=partitions.csv");
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    println!("cargo:rerun-if-changed=wifi_config.toml");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    stage_partitions_csv(&manifest_dir);
    load_wifi_config(&manifest_dir);

    embuild::espidf::sysenv::output();
}

/// ESP-IDF resolves `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` relative to
/// esp-idf-sys's auto-generated build directory. Copy our partitions.csv there
/// so the relative path resolves. (Same trick as the devkitc/dfr1117 carriers.)
fn stage_partitions_csv(manifest_dir: &str) {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let src = PathBuf::from(manifest_dir).join("partitions.csv");
    if !src.exists() {
        return;
    }
    let _ = fs::copy(&src, Path::new(&out_dir).join("partitions.csv"));
    if let Some(build_dir) = Path::new(&out_dir).parent().and_then(Path::parent) {
        if let Ok(entries) = fs::read_dir(build_dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("esp-idf-sys-") {
                    let espidf_out = entry.path().join("out");
                    if espidf_out.is_dir() {
                        let _ = fs::copy(&src, espidf_out.join("partitions.csv"));
                    }
                }
            }
        }
    }
}

/// Bake WiFi creds (wifi_config.toml or R2_WIFI_* env) as compile-time env vars.
fn load_wifi_config(manifest_dir: &str) {
    let config_path = format!("{manifest_dir}/wifi_config.toml");
    let mut ssid = env::var("R2_WIFI_SSID").unwrap_or_default();
    let mut pass = env::var("R2_WIFI_PASS").unwrap_or_default();
    let mut gw = env::var("R2_GATEWAY_IP").unwrap_or_default();

    if Path::new(&config_path).exists() {
        if let Ok(content) = fs::read_to_string(&config_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let value = value.trim().trim_matches('"');
                    match key.trim() {
                        "ssid" => ssid = value.to_string(),
                        "password" => pass = value.to_string(),
                        "gateway_ip" => gw = value.to_string(),
                        _ => {}
                    }
                }
            }
        }
    }
    println!("cargo:rustc-env=R2_WIFI_SSID={ssid}");
    println!("cargo:rustc-env=R2_WIFI_PASS={pass}");
    println!("cargo:rustc-env=R2_GATEWAY_IP={gw}");
    if ssid.is_empty() {
        println!("cargo:warning=WiFi not configured — set R2_WIFI_SSID/R2_WIFI_PASS or wifi_config.toml");
    }
}
