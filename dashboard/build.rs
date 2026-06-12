/// Stamp git short SHA + build timestamp as compile-time env vars so the
/// dashboard can report its own version (UI footer, /api/version, log
/// banner) and decide whether a sensor's announced fw_ver is "current"
/// for OTA purposes. Also read the per-deployment sensor class file so
/// the dashboard's BLE-scan filter matches the firmware's R2-BEACON
/// advertisement.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    rerun_on_git_commit_change();
    println!("cargo:rerun-if-changed=../trust_keys/sensor_class.txt");
    println!("cargo:rerun-if-changed=../trust_keys/legacy_classes.txt");

    // Per-deployment sensor class — same file the firmware reads at
    // build time. Both sides see the same string so the firmware's
    // advertise hash matches the dashboard's scan filter.
    let class_path = std::path::Path::new("../trust_keys/sensor_class.txt");
    let raw = std::fs::read_to_string(class_path).unwrap_or_else(|e| {
        panic!(
            "trust_keys/sensor_class.txt unreadable at {}: {} \
             — run `cargo run -p r2-workshop-tg --release -- init` from the repo root to generate it",
            class_path.display(),
            e,
        )
    });
    let class = raw.trim();
    if class.is_empty() {
        panic!(
            "trust_keys/sensor_class.txt is empty — write the BLE-beacon class string this deployment owns"
        );
    }
    println!("cargo:rustc-env=R2_SENSOR_CLASS={class}");

    // Legacy classes — pre-rotation strings the dashboard's BLE scan
    // should also accept during a class-string transition so sensors
    // still carrying old-class firmware remain discoverable through
    // bootstrap until they're reflashed. One class per line; lines
    // beginning with `#` are comments; blank lines ignored. File is
    // optional — when absent, behaviour is identical to the
    // pre-rotation single-class scan filter.
    let legacy_path = std::path::Path::new("../trust_keys/legacy_classes.txt");
    let legacy_joined = std::fs::read_to_string(legacy_path)
        .ok()
        .map(|raw| {
            raw.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect::<Vec<_>>()
                .join(";")
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=R2_SENSOR_CLASS_LEGACY={legacy_joined}");

    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() { Some(o.stdout) } else { None })
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".into());

    let dirty = std::process::Command::new("git")
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);

    let sha_full = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=R2_GIT_SHA={sha_full}");

    let ts = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=R2_BUILD_TIMESTAMP={ts}");
}

/// Re-stamp the git sha whenever the checked-out commit changes.
///
/// Watching `.git/HEAD` alone is NOT enough: a fast-forward `git pull`
/// advances the *branch ref*, while `.git/HEAD` stays `ref: refs/heads/<branch>`
/// — unchanged — so the stamp would silently go stale after every pull.
/// Watch HEAD, the loose ref it resolves to (`.git/refs/...`), AND
/// `packed-refs` (where the ref lives once `git gc`/`pack-refs` has packed
/// it), so a commit change via any of those paths forces a rerun. Best-effort:
/// outside a git checkout (e.g. a release tarball) these paths just don't
/// exist and the sha falls back to "unknown".
fn rerun_on_git_commit_change() {
    let git_dir = std::path::Path::new("../.git");
    let head = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    if let Ok(content) = std::fs::read_to_string(&head) {
        if let Some(reference) = content.strip_prefix("ref:").map(str::trim) {
            println!("cargo:rerun-if-changed={}", git_dir.join(reference).display());
        }
    }
    println!("cargo:rerun-if-changed={}", git_dir.join("packed-refs").display());
}
