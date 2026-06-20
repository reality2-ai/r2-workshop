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
    stamp_build_metadata(&manifest_dir);

    embuild::espidf::sysenv::output();
}

/// Bake fw version + git sha for telemetry (#18): R2_FW_VER = `<semver>+<sha>`,
/// R2_GIT_SHA = short sha (`-dirty` if the tree is modified). Re-runs when git
/// HEAD/index change so the stamp tracks the build.
fn stamp_build_metadata(manifest_dir: &str) {
    // Repo root is three levels above firmware/esp32-s3/dfr1195/.
    if let Some(root) = Path::new(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        for f in ["HEAD", "index"] {
            let p = root.join(".git").join(f);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
    let dirty = std::process::Command::new("git")
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    let semver = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    println!("cargo:rustc-env=R2_GIT_SHA={sha}");
    println!("cargo:rustc-env=R2_FW_VER={semver}+{sha}");
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
