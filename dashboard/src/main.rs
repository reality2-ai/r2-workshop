//! R2 Dashboard Gateway
//!
//! Receives R2-WIRE event frames over TCP from sensor nodes,
//! serves a live web dashboard, and pushes data to browsers via WebSocket.
//! Integrates r2-bootstrap to trigger sensor discovery from the browser.
//!
//! Architecture:
//!   Sensor (M10/ESP32) --TCP:21042--> Gateway --WebSocket--> Browser
//!   Browser --WebSocket--> Gateway --TCP--> Sensor (commands)
//!   Browser --POST /api/bootstrap--> Gateway --BLE--> Sensor discovery

mod access;
mod captures;
mod relay;

use axum::{
    body::Bytes,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path, State},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use sha2::{Digest, Sha256};
use clap::Parser;
use r2_bootstrap::{BootstrapConfig, BootstrapEvent};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, Mutex, RwLock};

// ── r2-workshop event hashes ────────────────────────────────────────────────
//
// Per `SPEC-R2-WORKSHOP-WIRE.md` §2. Forked from the M10 demo dashboard's
// flat names ("acceleration", "battery_status") to the `r2.sensor.*`
// namespace defined in our wire spec, so multiple R2 applications can
// coexist on a hub without hash collisions.

pub(crate) const ACCELERATION: u32 = r2_fnv::fnv1a_32(b"r2.sensor.acceleration");
const ACCELERATION_BATCH:  u32 = r2_fnv::fnv1a_32(b"r2.sensor.acceleration.batch");
const BATTERY:             u32 = r2_fnv::fnv1a_32(b"r2.sensor.battery");
const SENSOR_STATUS:       u32 = r2_fnv::fnv1a_32(b"r2.sensor.status");
const SENSOR_EVENT_LOG:    u32 = r2_fnv::fnv1a_32(b"r2.sensor.event.log");
const SENSOR_CAL_RESP:     u32 = r2_fnv::fnv1a_32(b"r2.sensor.cal.sample.resp");
const SENSOR_SYNC_PONG:    u32 = r2_fnv::fnv1a_32(b"r2.sensor.sync_pong");
const SENSOR_ANNOUNCE:     u32 = r2_fnv::fnv1a_32(b"r2.sensor.announce");
// Controller-synthesised peer-lifecycle events (BRIDGE §3.1). Today
// only `r2.peer.disconnected` is emitted; `r2.peer.connected` is
// covered by the existing announce replay and not yet a separate
// event — see SPEC-R2-WORKSHOP-VIEWER-SENTANT §6 outbound roadmap.
const PEER_DISCONNECTED:   u32 = r2_fnv::fnv1a_32(b"r2.peer.disconnected");

// Tracks B+C operator-plane status notifications. These wrap the
// R2-WIRE events on /r2 (formerly /ws/status JSON messages of the same names — removed v0.2; preserved
// for one release for backward compat with un-upgraded browsers).
// Each event is a CBOR map with small integer keys; the per-event
// shape is documented at the encode helper. Hash names align with
// SPEC-R2-WORKSHOP-BRIDGE.md §3.1 where applicable.
const DASH_OTA_PROGRESS:       u32 = r2_fnv::fnv1a_32(b"r2.dash.ota.progress");
const DASH_RESET_PROGRESS:     u32 = r2_fnv::fnv1a_32(b"r2.dash.reset.progress");
const DASH_CAPTURE_PROGRESS:   u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.progress");
const DASH_ACCESS_EVENT:       u32 = r2_fnv::fnv1a_32(b"r2.dash.access.event");
const DASH_BOOTSTRAP_PROGRESS: u32 = r2_fnv::fnv1a_32(b"r2.dash.bootstrap.progress");
const DASH_DEVICE_ALIAS_CHANGED: u32 = r2_fnv::fnv1a_32(b"r2.dash.device.alias.changed");

// Dashboard → sensor commands (SPEC-R2-WORKSHOP-WIRE §4 + SPEC-R2-WORKSHOP-TIMESYNC §4).
const DASH_ACK:               u32 = r2_fnv::fnv1a_32(b"r2.dash.ack");
const DASH_SYNC_PULSE:        u32 = r2_fnv::fnv1a_32(b"r2.dash.sync_pulse");
const DASH_SET_CLOCK_OFFSET:  u32 = r2_fnv::fnv1a_32(b"r2.dash.set_clock_offset");
const DASH_IDENTIFY_SET:      u32 = r2_fnv::fnv1a_32(b"r2.dash.identify_set");
// Capture session (SPEC-R2-WORKSHOP-CAPTURE §3).
const DASH_CAPTURE_START:     u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.start");
const DASH_CAPTURE_MARK:      u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.mark");
const DASH_CAPTURE_STOP:      u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.stop");
const SENSOR_CAPTURE_STATE:   u32 = r2_fnv::fnv1a_32(b"r2.sensor.capture.state");
// Auto-sync + event marks (SPEC-R2-WORKSHOP-CAPTURE §7.4 + §7.5,
// SPEC-R2-WORKSHOP-WIRE rows 44–47, added 2026-05-26).
const DASH_CAPTURE_SYNCED:    u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.synced");
const DASH_CAPTURE_SYNC_STARTED: u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.sync_started");
const DASH_CAPTURE_EVENT_MARK:   u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.event_mark");
const DASH_CAPTURE_EVENT_MARKED: u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.event_marked");

// Track C operator-plane events (viewer → controller). Per
// SPEC-R2-WORKSHOP-WIRE §2.1, viewer hives send these inbound on
// /r2; the dashboard validates and fans the corresponding
// downstream `r2.dash.<action>` to all sensors, then emits a
// `r2.dash.cmd.response` correlated by `req_id`.
const DASH_CMD_CAPTURE_START: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.start");
const DASH_CMD_CAPTURE_MARK:  u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.mark");
const DASH_CMD_CAPTURE_STOP:  u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.stop");
const DASH_CMD_CAPTURE_EVENT_MARK: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.event_mark");
const DASH_CMD_RESET:         u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.reset");
const DASH_CMD_IDENTIFY:      u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.identify");
const DASH_CMD_BOOTSTRAP:     u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.bootstrap");
const DASH_CMD_DEVICE_ALIAS_SET: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.device.alias.set");
const DASH_CMD_ACCESS_MEMBERS_QUERY: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.members.query");
const DASH_CMD_ACCESS_PENDING_QUERY: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.pending.query");
const DASH_CMD_ACCESS_CHECK:  u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.check");
const DASH_CMD_ACCESS_APPROVE: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.approve");
const DASH_CMD_ACCESS_DENY:   u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.deny");
const DASH_CMD_ACCESS_REVOKE: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.revoke");
const DASH_CMD_ACCESS_REQUEST: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.request");
const DASH_CMD_RESPONSE:      u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.response");

/// Map hash → human-readable name shipped to the browser.
fn event_name(hash: u32) -> &'static str {
    match hash {
        ACCELERATION              => "r2.sensor.acceleration",
        ACCELERATION_BATCH        => "r2.sensor.acceleration.batch",
        BATTERY                   => "r2.sensor.battery",
        SENSOR_STATUS             => "r2.sensor.status",
        SENSOR_EVENT_LOG          => "r2.sensor.event.log",
        SENSOR_CAL_RESP           => "r2.sensor.cal.sample.resp",
        SENSOR_SYNC_PONG          => "r2.sensor.sync_pong",
        SENSOR_ANNOUNCE           => "r2.sensor.announce",
        SENSOR_CAPTURE_STATE      => "r2.sensor.capture.state",
        DASH_CAPTURE_SYNCED       => "r2.dash.capture.synced",
        DASH_CAPTURE_SYNC_STARTED => "r2.dash.capture.sync_started",
        DASH_CAPTURE_EVENT_MARK   => "r2.dash.capture.event_mark",
        DASH_CAPTURE_EVENT_MARKED => "r2.dash.capture.event_marked",
        DASH_CMD_CAPTURE_EVENT_MARK => "r2.dash.cmd.capture.event_mark",
        _                         => "unknown",
    }
}

/// ADXL355 raw-LSB → g conversion, per the datasheet at ±2 g range.
/// Used by the server-side payload remap so the browser sees g-values
/// directly. When we add per-frame range tagging (WIRE §3.2 key 10),
/// switch to indexing this by the announced range.
const LSB_PER_G_AT_2G: f64 = 256_000.0;

/// Server-side remap of integer-keyed CBOR payloads into named-key JSON
/// per `SPEC-R2-WORKSHOP-WIRE.md`. The browser expects friendly key names
/// ({"x":42}) rather than {"2":42} so this is where the per-event
/// schema knowledge lives. For acceleration, we also scale raw ADXL355
/// LSB values to g-units here so the chart code stays simple.
fn remap_payload(event_hash: u32, raw: serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let obj = match raw {
        Value::Object(m) => m,
        other => return other, // not a map — pass through
    };
    let take = |m: &Map<String, Value>, k: &str| -> Option<Value> { m.get(k).cloned() };
    let mut out = Map::new();

    // ── Acceleration: scale + rename ─────────────────────────────────────
    if event_hash == ACCELERATION {
        let scale = |v: Option<&Value>| -> Value {
            v.and_then(|x| x.as_i64())
                .map(|raw_lsb| (raw_lsb as f64 / LSB_PER_G_AT_2G))
                .and_then(|g| serde_json::Number::from_f64(g).map(Value::Number))
                .unwrap_or(Value::Null)
        };
        if let Some(v) = take(&obj, "0") { out.insert("seq".into(), v); }
        if let Some(v) = take(&obj, "1") { out.insert("ts_ms".into(), v); }
        out.insert("x".into(), scale(obj.get("2")));
        out.insert("y".into(), scale(obj.get("3")));
        out.insert("z".into(), scale(obj.get("4")));
        if let Some(v) = take(&obj, "10") { out.insert("range".into(), v); }
        return Value::Object(out);
    }

    let map_keys: &[(&str, &str)] = match event_hash {
        BATTERY      => &[("0", "voltage_mv"), ("1", "percent"), ("2", "charging"), ("3", "ts_ms"), ("10", "temp_c")],
        SENSOR_ANNOUNCE => &[
            ("0", "device_pk"),
            ("1", "hostname"),
            ("2", "fw_ver"),
            ("3", "last_seq"),
            ("4", "boot_ts_ms"),
            ("5", "nonce"),
            ("6", "sig"),
            // Track A — KeyHolder-signed DeviceCertificate (147 bytes,
            // hex-encoded after remap). verify_announce_signature
            // reads this and switches to cert-anchored mode.
            ("8", "device_cert"),
            ("10", "mounting_role"),
        ],
        SENSOR_STATUS => &[
            ("0", "state"),
            ("1", "uptime_ms"),
            ("2", "samples_total"),
            ("3", "samples_acked"),
            ("4", "sd_pct_used"),
            ("5", "rate_hz_active"),
            ("6", "range_active"),
            ("10", "error_code"),
        ],
        _ => {
            // Unknown event — return the raw map as-is.
            return Value::Object(obj);
        }
    };
    let mut consumed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (k_int, k_named) in map_keys {
        if let Some(v) = take(&obj, k_int) {
            out.insert((*k_named).to_string(), v);
            consumed.insert(*k_int);
        }
    }
    // Preserve any unmapped keys (forwards-compat per WIRE §1.3) — but
    // skip the integer keys we already turned into named ones.
    for (k, v) in obj {
        if !consumed.contains(k.as_str()) {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

#[derive(Parser)]
#[command(name = "r2-dashboard", about = "R2 sensor dashboard gateway")]
struct Args {
    /// Unified R2 port — carries R2-WIRE events from sensors (raw TCP,
    /// length-prefixed per R2-WIRE §13.4) AND the browser-facing HTTP +
    /// WebSocket server (R2-WIRE-over-WS per R2-TRANSPORT §3.5). Per
    /// R2-WIRE §13.5, both encodings live on the canonical port 21042.
    /// Each accepted connection is peek-dispatched: HTTP-looking → axum;
    /// otherwise → raw R2-WIRE sensor handler.
    #[arg(long, default_value = "21042")]
    port: u16,

    /// Bind address
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,

    /// Phase 5 / SPEC-R2-WORKSHOP-ACCESS §3.4 — optional R2 relay URL
    /// embedded in invite tokens for off-network viewer enrolment.
    /// When unset, only the same-WiFi enrolment path is advertised.
    #[arg(long)]
    relay_url: Option<String>,

    /// Path to the rocker's WiFi config TOML (auto-generated by
    /// `tools/setup-hotspot.sh`). When set, the Link tab's invite
    /// modal shows a second QR encoding the hotspot's SSID + PSK
    /// in the standard `WIFI:T:WPA;...` form so a phone can join
    /// the hotspot before scanning the invite QR. Default
    /// `firmware/esp32-s3/devkitc/wifi_config.toml` — set explicitly
    /// to override or skip.
    #[arg(long, default_value = "firmware/esp32-s3/devkitc/wifi_config.toml")]
    wifi_config: String,
}

/// Build-stamped version string. Reported via /api/version, the startup
/// banner, and used by sensors / OTA logic to decide if an update is
/// needed (compare against `r2.sensor.announce.fw_ver`).
const DASHBOARD_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("R2_GIT_SHA"),
);

/// The ensemble class this dashboard belongs to (SPEC-R2-WORKSHOP-ENSEMBLE
/// §2.1). Same string the firmware bakes in and the BLE scan filters on —
/// `trust_keys/sensor_class.txt`, surfaced at build time by build.rs.
/// Class strings are lowercase-canonical, so the FNV hash below matches the
/// value the firmware advertises in its R2-BEACON payload.
const ENSEMBLE_CLASS: &str = env!("R2_SENSOR_CLASS");

/// FNV-1a-32 of the class string — the ensemble's wire identity, computed at
/// compile time (e.g. `nz.ac.auckland.rocker` → 0x624c47bc).
const ENSEMBLE_CLASS_HASH: u32 = r2_fnv::fnv1a_32(ENSEMBLE_CLASS.as_bytes());

/// R2-DEF §7 score-schema version this deployment's `ensemble/ensemble.yaml`
/// conforms to. Distinct from the ensemble's own semver (= CARGO_PKG_VERSION).
const ENSEMBLE_SCHEMA_VERSION: &str = "0.1";

/// The ensemble's user-facing name: the leaf segment of the class string
/// (`nz.ac.auckland.rocker` → `rocker`). Derived rather than hard-coded so a
/// re-class keeps a single source of truth (SPEC-R2-WORKSHOP-ENSEMBLE §2.2).
fn ensemble_name() -> &'static str {
    ENSEMBLE_CLASS.rsplit('.').next().unwrap_or(ENSEMBLE_CLASS)
}

/// The class-slug used in firmware release filenames: the reverse-DNS class
/// string with dots replaced by hyphens (`nz.ac.auckland.rocker` →
/// `nz-ac-auckland-rocker`). Per SPEC-R2-WORKSHOP-DASHBOARD §13.3 this is the
/// `<class-slug>` segment of `r2-workshop-firmware-<class-slug>-<carrier>-…`.
fn class_slug() -> String {
    ENSEMBLE_CLASS.replace('.', "-")
}

/// JSON for /api/version.
#[derive(Serialize)]
struct VersionInfo {
    version:   &'static str,
    git_sha:   &'static str,
    built_at:  &'static str,
    component: &'static str,
}

async fn version_handler() -> axum::Json<VersionInfo> {
    axum::Json(VersionInfo {
        version:   env!("CARGO_PKG_VERSION"),
        git_sha:   env!("R2_GIT_SHA"),
        built_at:  env!("R2_BUILD_TIMESTAMP"),
        component: "r2-workshop-dashboard",
    })
}

/// JSON for /api/ensemble — the dashboard's R2-ENSEMBLE identity
/// (SPEC-R2-WORKSHOP-ENSEMBLE §2.1). Lets the webapp + any tooling read the
/// running deployment's class/version without recompiling assumptions in.
#[derive(Serialize)]
struct EnsembleInfo {
    ensemble:         &'static str,
    class:            &'static str,
    class_hash:       String,
    ensemble_version: &'static str,
    build:            &'static str,
    built_at:         &'static str,
}

async fn ensemble_handler() -> axum::Json<EnsembleInfo> {
    axum::Json(EnsembleInfo {
        ensemble:         ensemble_name(),
        class:            ENSEMBLE_CLASS,
        class_hash:       format!("0x{ENSEMBLE_CLASS_HASH:08x}"),
        ensemble_version: ENSEMBLE_SCHEMA_VERSION,
        build:            DASHBOARD_VERSION,
        built_at:         env!("R2_BUILD_TIMESTAMP"),
    })
}

/// JSON message sent to browser via WebSocket
#[derive(Serialize, Clone, Debug)]
struct DashboardEvent {
    event: String,
    hash: String,
    timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
}

/// Connected sensor peer
#[derive(Debug)]
struct SensorPeer {
    #[allow(dead_code)]
    addr: SocketAddr,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    name: Option<String>,
    /// 64-hex-char Ed25519 public key from the most recent announce.
    /// Used as the alias-map lookup key in `/api/data/merged` and
    /// anywhere else we want to address a sensor by *device* identity
    /// rather than its (ephemeral) IP. Pulled out of the announce
    /// payload at decode time so downstream code doesn't have to
    /// re-parse the cached CBOR frame.
    device_pk: Option<String>,
    /// Most-recent `r2.sensor.announce` raw R2-WIRE frame bytes,
    /// cached so a freshly-connected /r2 viewer can be replayed
    /// the announce — otherwise it never sees `fw_ver`, `device_pk`,
    /// or `boot_ts_ms` because the announce only fires on TCP
    /// (re)connect, which already happened before the viewer arrived.
    last_announce: Option<Vec<u8>>,
    /// Most-recent `r2.sensor.capture.state` raw frame, cached for the
    /// same reason: capture.state only fires on transitions
    /// (start/mark/stop), so a viewer that hard-refreshes mid-recording
    /// would see the Run-Control buttons reset to the IDLE defaults.
    /// Replaying the cached state on /r2 open re-syncs the UI
    /// without needing a round-trip to the sensor.
    last_capture_state: Option<Vec<u8>>,
    /// Decoded form of `last_capture_state`, kept in lockstep with the
    /// raw frame. Used by the auto-sync engine (SPEC-R2-WORKSHOP-
    /// CAPTURE §7.4) to detect `Recording → Idle` transitions and the
    /// filename that just got finalised. `None` if we've never seen a
    /// state event for this peer.
    last_capture_decoded: Option<CaptureStateSnapshot>,
    /// Per-peer time-sync state per SPEC-R2-WORKSHOP-TIMESYNC §3.
    /// Updated by both the sync_pulse-sender task and the sync_pong
    /// handler in the read loop, hence Mutex-wrapped.
    sync: Arc<Mutex<PeerSyncState>>,
}

/// Decoded snapshot of `r2.sensor.capture.state` (WIRE row 20). Cached
/// per-peer so the auto-sync engine can detect transitions without
/// re-decoding the raw frame on every event. State values match the
/// firmware's `CaptureState` enum: 0 = Idle, 1 = Calibrating, 2 =
/// Recording. `filename` is the open file when `state == 2`.
#[derive(Debug, Clone)]
struct CaptureStateSnapshot {
    state: u8,
    filename: Option<String>,
}

/// Cristian's-algorithm time-sync state, per peer. The dashboard sends
/// `r2.dash.sync_pulse` on a schedule and processes incoming
/// `r2.sensor.sync_pong` to refine an exponentially-smoothed offset
/// estimate. When the estimate stabilises (or drifts past a threshold)
/// the dashboard pushes `r2.dash.set_clock_offset` so the sensor's
/// emitted `ts_ms` snaps onto the wall-clock timeline.
#[derive(Debug)]
struct PeerSyncState {
    connected_at: Instant,
    /// req_id → dashboard wall-clock at send time. Lookup on pong arrival
    /// gives us T1 for Cristian's math.
    pending: HashMap<u32, u64>,
    /// Recent offset_estimate values (in ms, as f64). Used for the
    /// stability check at calibration time.
    estimates: VecDeque<f64>,
    /// Exponential-smoothed residual offset, in ms. None until the
    /// first pong has been processed. Reset to 0 after each
    /// set_clock_offset push so it represents the residual on top of
    /// what the sensor has already applied.
    smoothed_offset_ms: Option<f64>,
    /// Total delta_ms pushed to this peer so far. Logged in timesync.log
    /// so analysis can reconstruct the boundary timing.
    cumulative_pushed_ms: i64,
    /// Has the initial calibration push happened yet?
    baseline_pushed: bool,
    /// Monotonically increasing req_id (wraps at u32 — irrelevant for
    /// our purposes since pending is rotated every sync round).
    next_req_id: u32,
}

impl PeerSyncState {
    fn new() -> Self {
        Self {
            connected_at: Instant::now(),
            pending: HashMap::new(),
            estimates: VecDeque::with_capacity(5),
            smoothed_offset_ms: None,
            cumulative_pushed_ms: 0,
            baseline_pushed: false,
            next_req_id: 1,
        }
    }
}

/// Build a dashboard → sensor R2-WIRE compact frame, TCP-framed (2-byte
/// length prefix). Mirrors the firmware's `wire::frame_for_tcp`, minus
/// the `mcu_origin` flag (we're the controller).
/// Build a sensor-bound TCP frame: the R2-WIRE compact body, prefixed
/// with a u16 BE length per the TCP framing convention. Suitable for
/// `peer.tx.send(...)` (sensor sockets).
fn build_dash_frame(event_hash: u32, msg_id: u16, payload: &[u8]) -> Vec<u8> {
    let frame_len = 12 + payload.len();
    let mut out = Vec::with_capacity(2 + frame_len);
    out.extend_from_slice(&(frame_len as u16).to_be_bytes());
    out.extend_from_slice(&build_dash_frame_body(event_hash, msg_id, payload));
    out
}

/// Build a bare R2-WIRE compact body (no leading TCP length prefix).
/// Suitable for storing in `RawFrame.frame` — the envelope's own
/// `frame_len` field provides framing for /r2 consumers, and the
/// webapp's `decode_compact_frame` reads from byte 0 of this body
/// (version/type/flags). Putting a TCP-style length prefix here
/// corrupts the decode: the first two bytes would be parsed as the
/// header byte, leaving event_hash off by two and silently dropped
/// by the viewer sentant.
fn build_dash_frame_body(event_hash: u32, msg_id: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    out.push(0x00); // version=0, msg_type=Event=0, flags=0
    out.push((5 << 4) | (3 & 0x0F)); // ttl=5, k=3
    out.extend_from_slice(&msg_id.to_be_bytes());
    out.extend_from_slice(&event_hash.to_be_bytes());
    out.extend_from_slice(&[0u8; 4]); // target = broadcast
    out.extend_from_slice(payload);
    out
}

/// Encode `r2.dash.ack` payload `{0: through_seq, 1: dash_ts_ms}` per WIRE §4.1.
fn encode_dash_ack(through_seq: u32, dash_ts_ms: u64) -> Vec<u8> {
    let mut buf = [0u8; 32];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(2);
        let _ = enc.kv(0, &r2_cbor::Value::UInt(through_seq as u64));
        let _ = enc.kv(1, &r2_cbor::Value::UInt(dash_ts_ms));
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.sync_pulse` payload `{0: req_id, 1: dash_ts_ms}`.
fn encode_sync_pulse(req_id: u32, dash_ts_ms: u64) -> Vec<u8> {
    let mut buf = [0u8; 32];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(2);
        let _ = enc.kv(0, &r2_cbor::Value::UInt(req_id as u64));
        let _ = enc.kv(1, &r2_cbor::Value::UInt(dash_ts_ms));
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.capture.mark` payload per SPEC-R2-WORKSHOP-CAPTURE §3.
///   `{0: ts_ms i64, 1: name str, 2: prefix str}` when a date prefix
///   like `"2026-05-18_13-35-00"` is supplied; otherwise the prefix
///   key is omitted and firmware falls back to `{ts_ms:016}` as the
///   filename stem.
fn encode_capture_mark(ts_ms: i64, name: &str, prefix: Option<&str>) -> Vec<u8> {
    let prefix_len = prefix.map(|p| p.len() + 4).unwrap_or(0);
    let mut buf = vec![0u8; 8 + 8 + name.len() + prefix_len + 8];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(if prefix.is_some() { 3 } else { 2 });
        let v_ts = if ts_ms >= 0 {
            r2_cbor::Value::UInt(ts_ms as u64)
        } else {
            r2_cbor::Value::NegInt(ts_ms)
        };
        let _ = enc.kv(0, &v_ts);
        let _ = enc.kv(1, &r2_cbor::Value::Text(name));
        if let Some(p) = prefix {
            let _ = enc.kv(2, &r2_cbor::Value::Text(p));
        }
        enc.len()
    };
    buf.truncate(used);
    buf
}

/// Encode `r2.dash.capture.start` / `r2.dash.capture.stop` empty payload (`{}`).
fn encode_empty_map() -> Vec<u8> {
    let mut buf = [0u8; 4];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(0);
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.identify_set` payload `{0: u8 on}`.
fn encode_identify_set(on: bool) -> Vec<u8> {
    let mut buf = [0u8; 8];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(1);
        let _ = enc.kv(0, &r2_cbor::Value::UInt(if on { 1 } else { 0 }));
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.set_clock_offset` payload `{0: delta_ms}` (i64 signed).
fn encode_set_clock_offset(delta_ms: i64) -> Vec<u8> {
    let mut buf = [0u8; 16];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(1);
        let v = if delta_ms >= 0 {
            r2_cbor::Value::UInt(delta_ms as u64)
        } else {
            r2_cbor::Value::NegInt(delta_ms)
        };
        let _ = enc.kv(0, &v);
        enc.len()
    };
    buf[..used].to_vec()
}

/// Decode `r2.sensor.sync_pong` payload `{0: req_id, 1: sensor_ts_ms}`.
/// Returns `(req_id, sensor_ts_ms)` on success.
fn decode_sync_pong(payload: &[u8]) -> Option<(u32, u64)> {
    let val = decode_cbor_payload(payload)?;
    let req_id = val.get("0").and_then(|v| v.as_u64())? as u32;
    let sensor_ts_ms = val.get("1").and_then(|v| v.as_u64())?;
    Some((req_id, sensor_ts_ms))
}

/// Current wall-clock ms since UNIX epoch — the dashboard's reference
/// timeline for sync_pulse / set_clock_offset math.
fn dash_wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// SPEC-R2-WORKSHOP-TIMESYNC §3.1 + §3.2 — process an inbound sync_pong,
/// update the peer's smoothed offset, and push `r2.dash.set_clock_offset`
/// on the calibration or drift-threshold triggers.
async fn handle_sync_pong(
    addr: SocketAddr,
    req_id: u32,
    sensor_ts_ms: u64,
    state: &Arc<AppState>,
) {
    // Look up the matching pending pulse (T1) and the peer's mutable
    // sync state in one step. Then unconditionally drop the peers read
    // lock before pushing further frames so we don't hold it across an
    // await that could re-enter the same peer map.
    let (sync_arc, cmd_tx_opt) = {
        let peers = state.peers.read().await;
        match peers.get(&addr) {
            Some(p) => (p.sync.clone(), Some(p.tx.clone())),
            None    => return, // peer disappeared between read loop and here
        }
    };
    let mut s = sync_arc.lock().await;

    let t1 = match s.pending.remove(&req_id) {
        Some(t) => t,
        None => {
            // Stale or unexpected req_id — pruned by the 120 s window in
            // the sender task, or duplicated pong. Either way ignore.
            return;
        }
    };
    let t3 = dash_wall_ms();
    let rtt = t3.saturating_sub(t1) as f64;
    // Cristian's: offset = T1 + RTT/2 - T2
    let offset_estimate = (t1 as f64) + rtt / 2.0 - (sensor_ts_ms as f64);

    // Exponential smoothing per spec §3.1 (α = 0.2).
    const ALPHA: f64 = 0.2;
    let smoothed = match s.smoothed_offset_ms {
        Some(prev) => ALPHA * offset_estimate + (1.0 - ALPHA) * prev,
        None       => offset_estimate,
    };
    s.smoothed_offset_ms = Some(smoothed);

    // Track recent raw estimates for the stability check.
    if s.estimates.len() == 5 {
        s.estimates.pop_front();
    }
    s.estimates.push_back(offset_estimate);

    let elapsed = s.connected_at.elapsed();
    eprintln!(
        "[time-sync] {} rtt={:.1}ms est={:+.1}ms smoothed={:+.1}ms (round {}, {:.0}s since connect)",
        addr,
        rtt,
        offset_estimate,
        smoothed,
        s.estimates.len(),
        elapsed.as_secs_f64()
    );

    // Decide whether to push a correction (SPEC-R2-WORKSHOP-TIMESYNC §3.2).
    //
    // Normal-case baseline waits ≥ 5 rounds + std-dev of the last 3
    // estimates < 5 ms so RTT jitter doesn't get baked into the offset.
    // But when the sensor's clock is grossly out (cold boot with no
    // NVS offset, or NVS-stale-by-minutes after an OTA), the smoothed
    // estimate is ≫ any plausible RTT jitter. Push that immediately —
    // the wall-clock-driven LED animation (and SD-card mtimes) read
    // wrong until baseline lands, and waiting 5+ rounds at that scale
    // is just operator confusion ("my LEDs are out of sync").
    const BASELINE_FAST_PATH_MS: f64 = 500.0;
    let push_decision: Option<(i64, &'static str)> = if !s.baseline_pushed {
        if smoothed.abs() >= BASELINE_FAST_PATH_MS {
            Some((smoothed.round() as i64, "baseline (fast)"))
        } else if s.estimates.len() >= 5 && std_dev_last_n(&s.estimates, 3) < 5.0 {
            Some((smoothed.round() as i64, "baseline"))
        } else {
            None
        }
    } else if smoothed.abs() >= 10.0 {
        // Drift correction.
        Some((smoothed.round() as i64, "drift"))
    } else {
        None
    };

    if let Some((delta_ms, reason)) = push_decision {
        s.cumulative_pushed_ms = s.cumulative_pushed_ms.wrapping_add(delta_ms);
        s.baseline_pushed = true;
        // After pushing, the residual is zero by construction.
        s.smoothed_offset_ms = Some(0.0);
        s.estimates.clear();
        let cumulative = s.cumulative_pushed_ms;
        drop(s); // release the per-peer lock before awaiting the cmd send

        let payload = encode_set_clock_offset(delta_ms);
        let frame = build_dash_frame(
            DASH_SET_CLOCK_OFFSET,
            (req_id & 0xFFFF) as u16, // reuse the pong's req_id for trace
            &payload,
        );
        if let Some(tx) = cmd_tx_opt {
            if tx.send(frame).await.is_err() {
                eprintln!("[time-sync] {} push failed — cmd channel closed", addr);
            } else {
                eprintln!(
                    "[time-sync] {} pushed set_clock_offset delta={:+} ms ({}); cumulative={}",
                    addr, delta_ms, reason, cumulative
                );
                append_timesync_log(addr, delta_ms, reason, cumulative);
            }
        }
    }
}

fn std_dev_last_n(estimates: &VecDeque<f64>, n: usize) -> f64 {
    let take = estimates.len().min(n);
    if take < 2 {
        return f64::INFINITY;
    }
    let slice: Vec<f64> = estimates.iter().rev().take(take).copied().collect();
    let mean = slice.iter().sum::<f64>() / (take as f64);
    let var = slice.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (take as f64);
    var.sqrt()
}

/// Append one line to the per-process timesync log per SPEC §3.3.
/// JSON-per-line at the path under /tmp; later we'll move it into
/// `<data_root>/<experiment_id>/timesync.log` once data-root config
/// lands.
fn append_timesync_log(addr: SocketAddr, delta_ms: i64, reason: &str, cumulative_ms: i64) {
    use std::io::Write;
    let line = serde_json::json!({
        "ts_ms": dash_wall_ms(),
        "peer": addr.to_string(),
        "delta_ms": delta_ms,
        "cumulative_ms": cumulative_ms,
        "reason": reason,
    }).to_string();
    let path = "/tmp/r2-workshop-timesync.log";
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => { let _ = writeln!(f, "{}", line); }
        Err(e)    => eprintln!("[time-sync] failed to write {}: {}", path, e),
    }
}

/// One R2-WIRE frame as it arrived on the TCP listener, plus metadata
/// needed by the WASM viewer to know which peer it came from. This is
/// the message shape pushed on the `/r2` WebSocket — the WASM hive
/// in the browser parses the envelope, then hands the inner frame to
/// `decode_compact_frame()`.
#[derive(Clone)]
pub(crate) struct RawFrame {
    /// Source socket address (e.g. "10.42.0.103:57768"), UTF-8.
    src: String,
    /// Wall-clock arrival time at the controller (ms since epoch).
    ts_ms: u64,
    /// The R2-WIRE compact frame bytes — same bytes the existing
    /// JSON-decoding path is fed (no length prefix).
    frame: Vec<u8>,
}

/// Shared application state
struct AppState {
    /// Broadcast channel for dashboard events → all (legacy) WebSocket clients
    event_tx: broadcast::Sender<DashboardEvent>,
    /// Phase 5d: broadcast channel for RAW R2-WIRE frames → WASM viewers.
    /// Same source frames, different output: raw bytes wrapped in a small
    /// envelope so the browser's WASM hive can decode in-process.
    raw_frame_tx: broadcast::Sender<RawFrame>,
    /// Connected sensor peers (for sending commands back)
    peers: RwLock<HashMap<SocketAddr, SensorPeer>>,
    /// Bootstrap state
    bootstrap_running: Arc<AtomicBool>,
    /// Handle to the running bootstrap task — aborted on re-press
    bootstrap_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Cached snapshot of the latest available firmware (GitHub
    /// Releases tag + asset URLs, with local releases dir as fallback).
    /// Refreshed lazily by `firmware_available_handler` when older
    /// than `FIRMWARE_CACHE_TTL_SECS`.
    firmware_cache: Mutex<Option<FirmwareAvailable>>,
    /// Phase 5 — SPEC-R2-WORKSHOP-ACCESS state: TrustGroup, invite
    /// tokens, member side-cache. `None` when the operator hasn't
    /// generated a KeyHolder key yet; the /api/access/* routes
    /// return 503 in that case so the dashboard still boots.
    access: Option<access::AccessHandle>,
    /// Outbound JSON text frames bound for the R2 relay session.
    /// Anyone (e.g. access::approve_request) pushes a string here;
    /// relay.rs subscribes and forwards each string verbatim as a
    /// WS text frame. None when the dashboard isn't running with
    /// `--relay-url`.
    /// Broadcasts JOIN_RESPONSE frames (notekeeper wire format —
    /// `[0xFF, 0x02, devicePk(32), tgPk(32), encrypted]`) for the
    /// relay session to forward to the joining device. `Some` only
    /// when `--relay-url` is configured.
    relay_binary_tx: Option<broadcast::Sender<Vec<u8>>>,
    /// Operator-assigned device aliases (device_pk hex → friendly
    /// name). Persisted to `~/.config/r2-workshop/device_aliases.json`
    /// so renames survive dashboard restarts and propagate to every
    /// dashboard browser session. v0.1 limitation: the sensor's own
    /// hostname / SD-card filename still uses its hardware-derived
    /// name — pushing aliases into firmware NVS is a follow-up task
    /// (see project memory `heterogeneous-fleet-open-question.md`).
    device_aliases: Arc<Mutex<HashMap<String, String>>>,
    /// Controller-local capture store + auto-sync bookkeeping per
    /// SPEC-R2-WORKSHOP-CAPTURE §7.4. Lazy-fetched files land under
    /// `$XDG_DATA_HOME/r2-workshop/captures/`; the `r2.dash.capture.
    /// synced` event broadcasts each successful write.
    captures: Arc<captures::CapturesStore>,
}

const FIRMWARE_CACHE_TTL_SECS: u64 = 300;
const GITHUB_OWNER_REPO: &str = "reality2-ai/r2-workshop";

#[derive(Clone, serde::Serialize)]
struct FirmwareAsset {
    /// Reverse-DNS class string the binary targets (sidecar-authoritative;
    /// filename-parsed when the sidecar is absent). Always equal to the
    /// dashboard's own `ENSEMBLE_CLASS` — foreign-class assets are filtered
    /// out before they reach this list (SPEC-R2-WORKSHOP-DASHBOARD §13.3).
    class: String,
    carrier: String,    // "devkitc", "xiao", "dfr1117", …
    version: String,    // exact fw_ver string baked in the .bin
    url: String,        // proxy URL the webapp fetches from (/api/firmware/...)
    /// SHA-256 hex of the `.bin`, lifted from the meta sidecar. `None` for
    /// pre-v0.3 releases that shipped no sidecar. Feeds the (future) signed-
    /// OTA verify path (Phase 9-secure).
    sha256: Option<String>,
    size: Option<u64>,
}

#[derive(Clone, serde::Serialize)]
struct FirmwareAvailable {
    /// "github" if the GitHub query succeeded; "local" if only the
    /// on-disk releases directory had hits; "none" if neither.
    source: String,
    /// The dashboard's own configured class — every asset is matched to
    /// (this class, carrier). Echoed so the webapp can show what it filtered to.
    class: String,
    /// Common version string across the assets — typically the
    /// GitHub release tag, or the highest-mtime fw_ver in the local
    /// releases dir.
    version: String,
    /// One entry per carrier.
    assets: Vec<FirmwareAsset>,
    /// Optional error/warning when GitHub was tried but failed.
    note: Option<String>,
    /// Unix-ms when this snapshot was taken, for cache age display.
    fetched_at_ms: u64,
}

/// Authoritative `(class, carrier, version, sha256)` tuple parsed from a
/// firmware meta sidecar (`*.bin.meta.json`), per SPEC-R2-WORKSHOP-DASHBOARD
/// §13.3. The sidecar wins over the filename on any disagreement.
#[derive(Clone)]
struct FirmwareMeta {
    class: String,
    carrier: String,
    version: String,
    sha256: Option<String>,
}

/// Parse the canonical release filename
/// `r2-workshop-firmware-<class-slug>-<carrier>-<version>+<git>.bin` into
/// `(class-slug, carrier, version)`. Returns `None` if the name doesn't fit
/// the convention. The class-slug itself contains hyphens, so we anchor on
/// *this dashboard's* slug as a known prefix — which doubles as the
/// class filter (foreign-class binaries simply don't match).
fn parse_release_filename(name: &str, slug: &str) -> Option<(String, String, String)> {
    let stem = name.strip_suffix(".bin")?;
    let prefix = format!("r2-workshop-firmware-{}-", slug);
    let rest = stem.strip_prefix(&prefix)?; // "<carrier>-<version>+<git>"
    let (carrier, ver_git) = rest.split_once('-')?;
    if carrier.is_empty() { return None; }
    // Drop the "+<git>" build-metadata suffix if present.
    let version = ver_git.split('+').next().unwrap_or(ver_git);
    if version.is_empty() { return None; }
    Some((slug.to_string(), carrier.to_string(), version.to_string()))
}

/// Decode a meta-sidecar JSON body into a `FirmwareMeta`. Tolerant of the
/// pre-v0.3 absence of `sha256`.
fn parse_meta_json(body: &str) -> Option<FirmwareMeta> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let class = v.get("class").and_then(|x| x.as_str())?.to_string();
    let carrier = v.get("carrier").and_then(|x| x.as_str())?.to_string();
    let version = v.get("version").and_then(|x| x.as_str())?.to_string();
    let sha256 = v.get("sha256").and_then(|x| x.as_str()).map(|s| s.to_string());
    Some(FirmwareMeta { class, carrier, version, sha256 })
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let (event_tx, _) = broadcast::channel::<DashboardEvent>(256);
    let (raw_frame_tx, _) = broadcast::channel::<RawFrame>(1024);
    // Relay outbound-binary channel for JOIN_RESPONSE frames
    // (notekeeper wire format `[0xFF, 0x02, ...]`). Only allocated
    // when --relay-url is set so the access handlers can branch on
    // `state.relay_binary_tx.is_some()`.
    let relay_binary_tx: Option<broadcast::Sender<Vec<u8>>> = if args.relay_url.is_some() {
        let (tx, _) = broadcast::channel::<Vec<u8>>(256);
        Some(tx)
    } else { None };

    // Phase 5: try to load the KeyHolder signing key. A successful load
    // unlocks /api/access/*; a failure logs + leaves Access disabled.
    // local_origin is what we'll embed in `url_local` per
    // SPEC-R2-WORKSHOP-ACCESS §4.1 step 4 — same host:port the webapp is
    // served on.
    let local_origin = format!("http://{}:{}", args.bind, args.port);
    let wifi_config_path = if args.wifi_config.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&args.wifi_config))
    };
    let access_handle = access::maybe_load(
        local_origin,
        args.relay_url.clone(),
        wifi_config_path,
    ).await;

    // Load persisted device aliases (renames survive dashboard restarts).
    let device_aliases = Arc::new(Mutex::new(load_device_aliases()));

    // SPEC-R2-WORKSHOP-CAPTURE §7.4: controller-local capture store.
    // Scans `$XDG_DATA_HOME/r2-workshop/captures/` on startup so files
    // synced in a previous run are immediately visible — restart-safe
    // by design (the directory itself is the source of truth).
    let captures = match captures::CapturesStore::load().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[captures] failed to initialise store: {e} — auto-sync disabled");
            // Fail soft: keep the dashboard runnable; sync engine will
            // no-op when the directory is unwritable. Use an in-memory
            // fallback so the rest of AppState stays simple. (Rebuilding
            // the dir later requires a restart.)
            captures::CapturesStore::load().await.expect("captures fallback")
        }
    };

    let state = Arc::new(AppState {
        event_tx: event_tx.clone(),
        raw_frame_tx: raw_frame_tx.clone(),
        peers: RwLock::new(HashMap::new()),
        bootstrap_running: Arc::new(AtomicBool::new(false)),
        bootstrap_task: Mutex::new(None),
        firmware_cache: Mutex::new(None),
        access: access_handle.clone(),
        relay_binary_tx: relay_binary_tx.clone(),
        device_aliases,
        captures,
    });

    // SPEC-R2-WORKSHOP-CAPTURE §7.4 — reconciliation poll for auto-
    // sync. Every 60 s, for each connected peer: LIST via data_tcp,
    // diff against the CapturesStore index, fetch anything missing.
    // The primary trigger is the Recording → Idle transition watcher
    // wired into the per-peer dispatch loop; this loop catches files
    // missed during dashboard downtime, late sensor reconnects, etc.
    {
        let recon_state = state.clone();
        tokio::spawn(async move {
            // First pass after 5 s so newly-connected sensors have a
            // chance to settle; thereafter every 60 s.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            loop {
                reconcile_captures_pass(&recon_state).await;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }

    // Phase 5 / SPEC-R2-WORKSHOP-ACCESS §5.2 — off-network viewer path
    // via the R2 relay. Only spawn when both --relay-url is set AND
    // the KeyHolder loaded; viewers need both to be useful.
    if let (Some(url), Some(handle), Some(tx)) = (args.relay_url.clone(), access_handle, relay_binary_tx) {
        let (sk, pk) = {
            let a = handle.lock().await;
            (a.tg_signing_key(), a.tg_pk_bytes())
        };
        relay::spawn_relay_session(
            url.clone(),
            sk,
            pk,
            raw_frame_tx.clone(),
            tx,
            state.clone(),
        );
        eprintln!("[relay] session spawned → {url}");
    }

    // R2-WIRE §13.5: port 21042 carries R2-WIRE events in both raw-TCP
    // (sensor side, length-prefixed) and WebSocket (browser side) form.
    // Single listener with peek-based protocol detection unifies both —
    // see the accept loop below.

    // HTTP server with WASM viewer + WebSocket + bootstrap API.
    // The legacy `/` HTML dashboard and `/ws` bidirectional channel were
    // removed once the WASM viewer at the repo's webapp/ became feature-
    // complete. The WASM viewer consumes /r2 only (since v0.2).
    let mut app = Router::new()
        // R2-WIRE frame channel for browser-side hives. Path
        // `/r2` is the spec-conformant convention per R2-INTERNET
        // §5 ("wss://relay.reality2.ai/r2", "wss://localhost:4005/r2").
        // The relay we talk to upstream uses the same path —
        // a viewer connecting through the relay vs directly to the
        // dashboard sees the same WS URL shape.
        .route("/r2", get({
            let ws_state = state.clone();
            move |ws, connect_info| ws_raw_handler(ws, ws_state, connect_info)
        }))
        // Per-sensor live log tail. Opens a TCP connection to the sensor's
        // log_tcp listener (port 21046) and pipes lines back as WS text
        // frames. Used by the per-card "↓ Logs" panel in the webapp.
        .route("/ws/logs/{addr}", get(ws_logs_handler))
        // Phase 5d: TG public key + KeyHolder enrolment endpoints.
        .route("/api/keyholder/tg-pub", get(tg_pub_handler))
        // SPEC-R2-WORKSHOP-ACCESS §4 — viewer enrolment lifecycle.
        // ACCESS v0.3 §8 — operator-only helper that returns the
        // pair of QR payloads for the "Onboard a visitor" modal.
        .route("/api/access/onboard",     get(access_onboard_handler))
        // Self-heal: a paired viewer calls this on every load with
        // its own device_pk to confirm it's still a known member.
        // 404 → stale cert → webapp wipes IndexedDB and re-prompts.
        .route("/api/access/whoami/{device_pk}", get(access_whoami_handler))
        // Legacy stubs from before the ACCESS spec landed — still
        // marked DEPRECATED in SPEC-R2-WORKSHOP-DASHBOARD §5.1.
        .route("/api/enrol-init", post(enrol_init_handler))
        .route("/api/enrol-complete", post(enrol_complete_handler))
        // Phase 9-light: stream a firmware .bin to a sensor's OTA listener.
        // Not migrated to a cmd event because the body is a multi-MB
        // binary; per-frame WS is the wrong shape. Progress events DO
        // ride R2-WIRE (r2.dash.ota.progress, SPEC-WIRE row 23).
        .route("/api/ota/{addr}", post(ota_push_handler))
        // Firmware availability: returns the latest release per
        // carrier (GitHub Releases primary, local releases/ dir
        // fallback). 5-minute cache. Webapp diffs against each peer's
        // announce fw_ver for the "needs update" dot.
        .route("/api/firmware/available", get(firmware_available_handler))
        .route("/api/firmware/{carrier}/binary", get(firmware_binary_handler))
        // SPEC-R2-WORKSHOP-CAPTURE — capture-file listing / fetch / delete.
        // Each route opens a fresh TCP connection to <addr>:21047 on
        // the sensor and proxies the data_tcp wire protocol.
        .route("/api/data/{addr}/list",        get(data_list_handler))
        .route("/api/data/{addr}/file/{name}", get(data_get_handler).delete(data_delete_handler))
        .route("/api/data/{addr}/all",         axum::routing::delete(data_delete_all_handler))
        .route("/api/data/merged",             get(data_merged_handler))
        .route("/api/data/zip",                get(data_zip_handler))
        // SPEC-R2-WORKSHOP-CAPTURE §7.4 + SPEC-R2-WORKSHOP-DASHBOARD §5.1:
        // controller-local capture store. Sessions-first index + single-
        // file download served straight from `$XDG_DATA_HOME/r2-workshop/
        // captures/` — works while sensors are offline.
        .route("/api/data/local/list",         get(data_local_list_handler))
        .route("/api/data/local/file/{name}",  get(data_local_file_handler))
        .route("/api/data/local/all",          axum::routing::delete(data_local_delete_all_handler))
        .route("/api/data/session/{stem}",     axum::routing::delete(data_delete_session_handler))
        // Operator-assigned device aliases. Persisted to
        // ~/.config/r2-workshop/device_aliases.json. Read by every
        // dashboard browser session on load + applied on top of the
        // sensor's self-reported hostname. The POST form (`/alias`)
        // was migrated to r2.dash.cmd.device.alias.set in Track C;
        // only the bulk-fetch GET stays.
        .route("/api/devices/aliases",         get(device_aliases_get_handler))
        .route("/api/version", get(version_handler))
        .route("/api/ensemble", get(ensemble_handler));
        // v0.2 cleanup: the following legacy /api/* routes were
        // dropped now that their cmd-event equivalents on /r2 have
        // been bench-validated (capture / reset / identify /
        // bootstrap / device.alias.set / 5 access actions + 2 reads).
        // Webapp no longer calls any of them. Removed handlers:
        //   POST /api/capture/{start,mark,stop}
        //   POST /api/sensor/{addr}/reset
        //   POST /api/sensor/{addr}/identify
        //   POST /api/bootstrap   +  GET /api/bootstrap/status
        //   POST /api/devices/alias
        //   GET  /api/access/members
        //   GET  /api/access/pending
        //   POST /api/access/{approve,deny,revoke}/{device_pk}
        //   POST /api/access/request
        //   GET  /api/access/check/{device_pk}

    // Serve the WASM viewer (webapp/) as the dashboard root if the
    // directory exists. Same-origin with the dashboard's WS endpoints
    // means no CORS dance for the browser. fallback_service ensures
    // the explicit /api/ and /ws/ routes win; everything else falls
    // through to the static asset server.
    let viewer_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("webapp"));
    if let Some(dir) = viewer_dir.as_ref().filter(|d| d.is_dir()) {
        app = app.fallback_service(tower_http::services::ServeDir::new(dir));
        eprintln!("[webapp] mounted webapp/ at /  ({})", dir.display());
    } else {
        eprintln!("[webapp] webapp/ not found — UI disabled");
    }

    let app = app.with_state(state.clone());

    let bind_addr: SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .expect("valid bind address");

    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║              r2-workshop dashboard                              ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║  ensemble:   {:<48}║",
        format!("{} · {} (0x{:08x})", ensemble_name(), ENSEMBLE_CLASS, ENSEMBLE_CLASS_HASH));
    eprintln!("║  version:    {:<48}║", DASHBOARD_VERSION);
    eprintln!("║  built:      {:<48}║", env!("R2_BUILD_TIMESTAMP"));
    eprintln!("║  R2 port:    {:<48}║", format!("{} (raw R2-WIRE TCP + HTTP/WS)", bind_addr));
    eprintln!("║  dashboard:  http://{:<41}║", bind_addr.to_string());
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    let listener = tokio::net::TcpListener::bind(bind_addr).await
        .unwrap_or_else(|e| {
            eprintln!("ERROR: Cannot bind R2 port {} — {}", bind_addr, e);
            eprintln!("Is another r2-dashboard already running? Kill it first: pkill r2-dashboard");
            std::process::exit(1);
        });
    eprintln!("[r2-port] listening on {}", bind_addr);

    run_unified_listener(listener, app, state).await;
}

/// Single accept loop on the unified R2 port (R2-WIRE §13.5 — port
/// 21042 carries R2-WIRE events in both raw-TCP and WebSocket form).
/// Each accepted connection is peeked: HTTP-looking → driven via hyper
/// with the axum router; otherwise → handed to the existing sensor
/// TCP handler. Sensor frames are length-prefixed (R2-WIRE §13.4), so
/// the first byte is always the high byte of a u16 BE length — for
/// our compact frames (< 256 bytes) that's `0x00`. HTTP request lines
/// start with ASCII `[A-Z]`. The two never collide.
async fn run_unified_listener(
    listener: tokio::net::TcpListener,
    app: axum::Router<()>,
    state: Arc<AppState>,
) {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use tower::ServiceExt;

    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[r2-port] accept error: {}", e);
                continue;
            }
        };
        let app_for_conn = app.clone();
        let state_for_conn = state.clone();
        tokio::spawn(async move {
            // Peek the first byte. 5 s is generous — even slow sensors
            // emit their announce within hundreds of ms of TCP connect.
            // Browsers send HTTP request lines well under that too.
            let mut first = [0u8; 1];
            let peek = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                stream.peek(&mut first),
            ).await;
            let n = match peek {
                Ok(Ok(n)) => n,
                _ => return, // timeout or read error
            };
            if n == 0 { return; }

            if first[0].is_ascii_uppercase() {
                // HTTP path — drive axum via hyper. Attach ConnectInfo
                // to every Request so /api/access/* handlers (which
                // extract `ConnectInfo<SocketAddr>` for the loopback
                // KeyHolder gate) work as they did under axum::serve.
                let svc = ServiceExt::<hyper::Request<Incoming>>::map_request(
                    app_for_conn,
                    move |mut req: hyper::Request<Incoming>| {
                        req.extensions_mut().insert(axum::extract::ConnectInfo(addr));
                        req
                    },
                );
                let hyper_svc = TowerToHyperService::new(svc);
                let io = TokioIo::new(stream);
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, hyper_svc)
                    .with_upgrades()
                    .await
                {
                    // Don't log normal client-side closes (broken pipe
                    // / connection reset) as errors. The hyper error
                    // type doesn't expose `kind`; match on text.
                    let msg = format!("{}", e);
                    if !msg.contains("user code") && !msg.contains("closed") {
                        eprintln!("[r2-port http] {}: {}", addr, msg);
                    }
                }
            } else {
                // Raw R2-WIRE TCP — sensor connection. TCP keepalive
                // catches zombie connections within ~60 s rather than
                // waiting for the 2-hour OS default. Borrow the FD via
                // socket2::SockRef so we don't have to convert tokio →
                // std → tokio (that round-trip plus the preceding peek
                // was causing sensors to cycle every ~20 s).
                eprintln!("[events] sensor connected: {}", addr);
                apply_tcp_keepalive(&stream);
                handle_sensor_connection(stream, addr, state_for_conn).await;
            }
        });
    }
}

/// Apply 15 s/5 s TCP keepalive to a freshly-accepted sensor socket.
/// Uses `socket2::SockRef` to set the sockopts on the borrowed FD —
/// avoids the `tokio → std → tokio` FD round-trip that the pre-v0.2
/// code did (sensor connections began cycling every ~20 s when the
/// round-trip was paired with a `stream.peek(...)` call earlier in
/// the accept handler — bench-confirmed 2026-05-23).
fn apply_tcp_keepalive(stream: &tokio::net::TcpStream) {
    let sock = socket2::SockRef::from(stream);
    sock.set_keepalive(true).ok();
    let ka = socket2::TcpKeepalive::new()
        .with_time(std::time::Duration::from_secs(15))
        .with_interval(std::time::Duration::from_secs(5));
    sock.set_tcp_keepalive(&ka).ok();
}

/// Shared bootstrap core. Aborts any running discovery task, clears
/// the log, cycles the AP, and spawns a fresh discovery cycle.
/// Returns immediately after scheduling — discovery progress streams
/// via `r2.dash.bootstrap.progress`. Fire-and-forget by design; the
/// only synchronous failure mode is task-spawn refusal which doesn't
/// happen in practice on tokio.
async fn do_bootstrap(state: &Arc<AppState>) {
    // Abort any existing bootstrap task and wait for it to clean up
    {
        let mut task = state.bootstrap_task.lock().await;
        if let Some(handle) = task.take() {
            handle.abort();
            // Small delay so the task drops cleanly before we restart
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        }
    }

    state.bootstrap_running.store(true, Ordering::SeqCst);

    // Reset event so the browser clears its log panel.
    emit_bootstrap_reset(state);

    let config = BootstrapConfig {
        ssid: None,
        psk: None,
        // Longer scan window than the prior 10s default: missed-on-first-pass
        // sensors (BLE advertise interval / RSSI variance / sensor reboot
        // timing right after cycle_hotspot) get caught in the same pass
        // instead of waiting another full retry cycle to be picked up.
        // Pair this with the shorter RETRY_INTERVAL_SECS in r2-bootstrap.
        scan_secs: 20,
        // Reverse-DNS class identifier (R2-BEACON §4); read from
        // `trust_keys/sensor_class.txt` at build time (see build.rs).
        // The firmware reads the same file via its own build.rs, so
        // dashboard and sensor always agree on the on-air identity.
        // See SPEC-R2-WORKSHOP-DASHBOARD §6.3.
        target_class: env!("R2_SENSOR_CLASS").to_string(),
        // Legacy classes from `trust_keys/legacy_classes.txt` (one per
        // semicolon in the build-time env var). Empty string → no
        // legacy entries; behaviour identical to the single-class
        // scan filter. Used during a class-string rotation transition
        // so sensors still carrying pre-rotation firmware remain
        // discoverable until they're reflashed.
        legacy_classes: env!("R2_SENSOR_CLASS_LEGACY")
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        // Always cycle the hotspot on a fresh bootstrap press. Sensors
        // currently joined to the existing hotspot will lose WiFi for
        // a few seconds and fall back to BLE advertising, which is the
        // only path through which `run_bootstrap` can re-push
        // credentials. Without this, pressing "Connect Sensors" while
        // a sensor is already streaming does nothing for that sensor.
        cycle_hotspot: true,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<BootstrapEvent>(64);
    let state_for_relay = state.clone();
    let running_flag = state.bootstrap_running.clone();

    // Spawn the event relay task — forwards each BootstrapEvent from
    // r2_bootstrap's progress channel out to viewers as an R2-WIRE
    // `r2.dash.bootstrap.progress` event on /r2 (SPEC-WIRE row 27).
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            emit_bootstrap_progress(&state_for_relay, &event);
        }
        running_flag.store(false, Ordering::SeqCst);
    });

    // Spawn the bootstrap task and store the handle for cancellation
    let bootstrap_handle = tokio::spawn(async move {
        if let Err(e) = r2_bootstrap::run_bootstrap(config, tx.clone()).await {
            let _ = tx.send(BootstrapEvent::Error(format!("{}", e))).await;
        }
        // Drop tx to signal the relay task to finish
    });
    *state.bootstrap_task.lock().await = Some(bootstrap_handle);
}

/// POST /api/ota/{addr} — Phase 9-light, push a firmware binary to a sensor's
/// OTA listener (TCP 21043). Body is the raw `.bin`. Returns JSON describing
/// the result: bytes sent, sha256 hex, the receiver's status code + message.
///
/// `addr` may be either an IP ("10.42.0.103") or `ip:port` from the connected-
/// peers list (the port is replaced with 21043 in either case).
async fn ota_push_handler(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    use std::net::ToSocketAddrs;

    // Strip any sensor TCP port if the caller pasted in `ip:port` from
    // the peers list — OTA always lands on the well-known port.
    let ip_only: &str = addr.split(':').next().unwrap_or(&addr);
    let ota_target = format!("{}:21043", ip_only);

    if body.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "empty body"})),
        );
    }

    eprintln!("[ota] push to {} ({} bytes)", ota_target, body.len());
    emit_ota_progress(&state, "uploading", &ota_target, Some(body.len()), None);

    // Resolve so DNS errors fail fast (we expect numeric IPs but be safe).
    let socket = match ota_target.to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None    => return ota_err(&state, &ota_target, "no addr resolved"),
        },
        Err(e) => return ota_err(&state, &ota_target, &format!("resolve: {e}")),
    };

    // Pre-compute the SHA-256 over the full firmware blob.
    let sha: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(&body);
        h.finalize().into()
    };

    // 60 s should be ample for a ~1.4 MB blob over 802.11 + write into flash.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        push_firmware(socket, &body, &sha),
    )
    .await;

    match result {
        Ok(Ok((status_byte, msg))) => {
            let ok = status_byte == 0x00; // STATUS_OK in r2-esp::ota_tcp
            let phase = if ok { "applied" } else { "rejected" };
            emit_ota_progress(&state, phase, &ota_target, None, Some(&msg));
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "ok": ok,
                    "size": body.len(),
                    "sha256": hex::encode(&sha),
                    "status_byte": status_byte,
                    "message": msg,
                })),
            )
        }
        Ok(Err(e))  => ota_err(&state, &ota_target, &format!("push: {e}")),
        Err(_)      => ota_err(&state, &ota_target, "timed out after 60 s"),
    }
}

fn ota_err(state: &Arc<AppState>, target: &str, msg: &str) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    eprintln!("[ota] {} — {}", target, msg);
    emit_ota_progress(state, "error", target, None, Some(msg));
    (
        axum::http::StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({"ok": false, "error": msg})),
    )
}

/// Drives the OTA-receive protocol from `r2-esp::ota_tcp` (R2-OTA TCP):
///   START preamble: cmd(1) + size_le(4) + sha256(32)
///   firmware bytes
///   half-close (write shutdown) → receiver flushes + writes partition
///   response: status(1) + len_le(2) + utf-8 message
async fn push_firmware(
    target: SocketAddr,
    body: &[u8],
    sha: &[u8; 32],
) -> std::io::Result<(u8, String)> {
    const CMD_START: u8 = 0x01;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        TcpStream::connect(target),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;

    // Preamble
    let mut preamble = Vec::with_capacity(37);
    preamble.push(CMD_START);
    preamble.extend_from_slice(&(body.len() as u32).to_le_bytes());
    preamble.extend_from_slice(sha);
    stream.write_all(&preamble).await?;

    // Stream in 64 KiB chunks.
    for chunk in body.chunks(65536) {
        stream.write_all(chunk).await?;
    }
    stream.flush().await?;

    // Half-close write side; receiver uses this as EOF for the firmware
    // stream, then writes the partition + sends a response.
    let _ = stream.shutdown().await;

    // Response: status(1) + len(2 LE) + message
    let mut hdr = [0u8; 3];
    stream.read_exact(&mut hdr).await?;
    let status = hdr[0];
    let msg_len = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;
    let mut msg = vec![0u8; msg_len];
    if msg_len > 0 {
        stream.read_exact(&mut msg).await?;
    }
    Ok((status, String::from_utf8_lossy(&msg).into_owned()))
}

/// POST /api/sensor/{addr}/reset — per SPEC-R2-WORKSHOP-SENSOR-REMOTE-RESET.
/// Sends a single CMD_RESET (0x10) byte to the sensor's reset listener
/// (TCP 21044) and returns the receiver's status + message. The sensor
/// reboots ~100 ms after responding.
///
/// `addr` may be `ip` or `ip:port`; the streaming port is stripped and
/// 21044 is always used.
/// Shared reset core. Returns `Ok((status_byte, message))` on a clean
/// TCP round-trip (where `status_byte == 0x00` means the sensor
/// accepted the reset), or `Err(message)` for connect / timeout /
/// network errors. Either way, `r2.dash.reset.progress` is fired at
/// each phase boundary.
async fn do_reset(state: &Arc<AppState>, addr: &str) -> Result<(u8, String), String> {
    use std::net::ToSocketAddrs;

    let ip_only: &str = addr.split(':').next().unwrap_or(addr);
    let reset_target = format!("{}:21044", ip_only);

    eprintln!("[reset] push to {}", reset_target);
    emit_reset_progress(state, "requested", &reset_target, None);

    let socket = match reset_target.to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None    => {
                let msg = "no addr resolved".to_string();
                emit_reset_progress(state, "error", &reset_target, Some(&msg));
                return Err(msg);
            }
        },
        Err(e) => {
            let msg = format!("resolve: {e}");
            emit_reset_progress(state, "error", &reset_target, Some(&msg));
            return Err(msg);
        }
    };

    // 8 s is generous — a healthy sensor responds in <100 ms.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        push_reset(socket),
    )
    .await;

    match result {
        Ok(Ok((status_byte, msg))) => {
            let ok = status_byte == 0x00; // STATUS_OK in r2-esp::reset_tcp
            let phase = if ok { "applied" } else { "error" };
            emit_reset_progress(state, phase, &reset_target, Some(&msg));
            Ok((status_byte, msg))
        }
        Ok(Err(e)) => {
            let msg = format!("push: {e}");
            emit_reset_progress(state, "error", &reset_target, Some(&msg));
            Err(msg)
        }
        Err(_) => {
            let msg = "timed out after 8 s".to_string();
            emit_reset_progress(state, "error", &reset_target, Some(&msg));
            Err(msg)
        }
    }
}

/// POST /api/sensor/{addr}/identify  body `{on: bool}` — toggle the
/// operator-identify overlay (solid white LED) on the named sensor.
/// Used to pick a specific board out of a busy rack for a battery
/// swap or similar. Frame goes out via the streaming-TCP peer
/// command channel (same path as set_clock_offset / sync_pulse).
/// Shared identify core. Queues a `r2.dash.identify_set` frame on
/// the named peer's streaming TCP channel. Fire-and-forget — returns
/// `Ok(())` iff the queue accepted; sensor's own ACK (LED actually
/// toggled) is not awaited.
async fn do_identify(state: &Arc<AppState>, addr: &str, on: bool) -> Result<(), String> {
    let ip_only: &str = addr.split(':').next().unwrap_or(addr);

    // peers is keyed by SocketAddr (ip:port); the path/event addr is
    // typically just the IP (or ip:port). Match on the IP portion.
    let tx = {
        let peers = state.peers.read().await;
        peers.iter()
            .find(|(sa, _)| sa.ip().to_string() == ip_only)
            .map(|(_, p)| p.tx.clone())
    };
    let Some(tx) = tx else {
        return Err("no such connected peer".to_string());
    };

    let frame = build_dash_frame(DASH_IDENTIFY_SET, 0, &encode_identify_set(on));
    if tx.send(frame).await.is_err() {
        return Err("peer queue closed".to_string());
    }
    eprintln!("[identify] {} on={}", ip_only, on);
    Ok(())
}

/// Drives the reset protocol from `r2-esp::reset_tcp`:
///   CMD_RESET(1) → status(1) + len_le(2) + message
async fn push_reset(target: SocketAddr) -> std::io::Result<(u8, String)> {
    const CMD_RESET: u8 = 0x10;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        TcpStream::connect(target),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;

    stream.write_all(&[CMD_RESET]).await?;
    stream.flush().await?;

    let mut hdr = [0u8; 3];
    stream.read_exact(&mut hdr).await?;
    let status = hdr[0];
    let msg_len = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;
    let mut msg = vec![0u8; msg_len];
    if msg_len > 0 {
        stream.read_exact(&mut msg).await?;
    }
    Ok((status, String::from_utf8_lossy(&msg).into_owned()))
}

// run_event_listener was a separate TCP listener on port 21042 for
// sensors, paired with axum::serve on port 8080 for browsers. Replaced
// by run_unified_listener (above) which serves both on the canonical
// R2 port 21042 with peek-based protocol detection — R2-WIRE §13.5.

/// Handle a single sensor TCP connection
async fn handle_sensor_connection(stream: TcpStream, addr: SocketAddr, state: Arc<AppState>) {
    let (mut reader, mut writer) = stream.into_split();

    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

    let sync_state = Arc::new(Mutex::new(PeerSyncState::new()));
    {
        let mut peers = state.peers.write().await;
        peers.insert(addr, SensorPeer {
            addr,
            tx: cmd_tx.clone(),
            name: None,
            device_pk: None,
            last_announce: None,
            last_capture_state: None,
            last_capture_decoded: None,
            sync: sync_state.clone(),
        });
    }

    // Per-peer sync_pulse task. Per SPEC-R2-WORKSHOP-TIMESYNC §3.1 cadence:
    // 1 Hz for the first 30 s after this TCP connect, then 30 s thereafter.
    // Exits when the cmd_tx send fails (peer disconnected, channel closed).
    let sync_tx = cmd_tx.clone();
    let sync_state_for_task = sync_state.clone();
    let sync_addr = addr;
    let _sync_handle = tokio::spawn(async move {
        let fast_until = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let interval = if Instant::now() < fast_until {
                std::time::Duration::from_secs(1)
            } else {
                std::time::Duration::from_secs(30)
            };
            // Acquire a req_id and record the dashboard-side T1 before
            // sending, so the pong handler can look it up by req_id.
            let (req_id, dash_ts) = {
                let mut s = sync_state_for_task.lock().await;
                let id = s.next_req_id;
                s.next_req_id = s.next_req_id.wrapping_add(1);
                let t1 = dash_wall_ms();
                s.pending.insert(id, t1);
                // Prune very old entries (>120 s) to avoid leaking
                // memory if pongs are persistently dropped.
                let cutoff = t1.saturating_sub(120_000);
                s.pending.retain(|_, t| *t >= cutoff);
                (id, t1)
            };
            let payload = encode_sync_pulse(req_id, dash_ts);
            let frame = build_dash_frame(
                DASH_SYNC_PULSE,
                (req_id & 0xFFFF) as u16,
                &payload,
            );
            if sync_tx.send(frame).await.is_err() {
                // cmd_rx side closed — peer is gone.
                eprintln!("[time-sync] {} cmd channel closed; sync task exiting", sync_addr);
                return;
            }
            tokio::time::sleep(interval).await;
        }
    });

    let _timestamp_start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let read_state = state.clone();
    let read_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        let mut frame_buf = Vec::new();
        // Decimate live acceleration to ~10 Hz for the browser per
        // SPEC-R2-WORKSHOP-DASHBOARD §5.2 ("at most 10 samples/sec per
        // peer per browser tab on the live wire, with the rest
        // decimated"). Source rate is 100 Hz from the firmware, so
        // we push every 10th. The full stream lands in the SD ring
        // when Phase 3 is implemented; until then dropped samples
        // are simply not displayed (gaps are harmless for a sine
        // wave demo).
        const ACCEL_DECIMATION: u32 = 10;
        let mut accel_n: u32 = 0;

        // ACK tracking per SPEC-R2-WORKSHOP-WIRE §4.1. We emit a
        // `r2.dash.ack {through_seq, dash_ts_ms}` to the sensor at
        // most every ACK_PERIOD_MS or every ACK_SAMPLES received
        // acceleration frames, whichever first. The firmware uses
        // `through_seq` to free SD ring segments
        // (SPEC-R2-WORKSHOP-SENSOR §7.4); without these acks the ring
        // fills up. We track max_seq_seen locally so a stuck/
        // out-of-order frame can't cause us to ack the wrong
        // through_seq.
        const ACK_PERIOD_MS: u64 = 200;
        const ACK_SAMPLES: u32 = 100;
        let mut max_seq_seen: u32 = 0;
        let mut samples_since_ack: u32 = 0;
        let mut next_ack_at = tokio::time::Instant::now()
            + std::time::Duration::from_millis(ACK_PERIOD_MS);
        let mut ack_msg_id: u16 = 1;

        loop {
            // 15 s read deadline. The sensor sends `r2.sensor.status`
            // every 2 s plus continuous 10 Hz acceleration; a healthy
            // peer transmits dozens of frames per second, so 15 s of
            // silence still reliably catches genuinely-gone peers
            // (chip reset / WiFi drop / hard crash) without flapping on
            // transient stalls. Bumped from 5 s after a 2026-05-28
            // bench observation where freshly-flashed sensors went
            // silent for >5 s in the post-announce window, getting
            // hung up on every reconnect cycle.
            let read_result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                reader.read(&mut buf),
            ).await;
            let read_outcome = match read_result {
                Ok(r) => r,
                Err(_) => {
                    eprintln!("[events] read timeout from {} (no traffic in 15 s) — closing", addr);
                    break;
                }
            };
            match read_outcome {
                Ok(0) => break,
                Ok(n) => {
                    frame_buf.extend_from_slice(&buf[..n]);

                    while frame_buf.len() >= 2 {
                        let frame_len = ((frame_buf[0] as usize) << 8) | (frame_buf[1] as usize);
                        if frame_buf.len() < 2 + frame_len {
                            break;
                        }

                        let frame = frame_buf[2..2 + frame_len].to_vec();
                        frame_buf.drain(..2 + frame_len);

                        // R2-WIRE compact frame (SPEC-R2-WORKSHOP-WIRE §1.4):
                        // byte 0:    version|msg_type|flags
                        // byte 1:    ttl|k
                        // bytes 2-3: msg_id (BE u16)
                        // bytes 4-7: event_hash (BE u32)
                        // bytes 8-11: target (BE u32)
                        // bytes 12+: payload
                        let event_hash = if frame.len() >= 8 {
                            Some(((frame[4] as u32) << 24)
                                | ((frame[5] as u32) << 16)
                                | ((frame[6] as u32) << 8)
                                | (frame[7] as u32))
                        } else {
                            None
                        };

                        // SPEC-R2-WORKSHOP-DASHBOARD §5.2 — server-side
                        // acceleration decimation. Originally only applied
                        // to the legacy /ws/status path (since removed v0.2); left /r2
                        // running at the full firmware rate (100 Hz × N
                        // sensors) on the assumption the WASM hive could
                        // self-throttle. Pi5 deployment proved otherwise —
                        // the WebSocket + browser-side per-frame work
                        // saturated. Decimating at the source for both
                        // transports keeps the live wire at the spec's
                        // ~10 Hz/peer; full fidelity remains on the SD
                        // ring (+ `/api/data/*` retrieval). Task #68.
                        let is_accel = event_hash == Some(ACCELERATION);
                        let emit_live = if is_accel {
                            let due = accel_n == 0;
                            accel_n = (accel_n + 1) % ACCEL_DECIMATION;
                            due
                        } else {
                            true
                        };

                        // /r2 viewers — Phase 5d. Push every
                        // non-acceleration event verbatim, and one in
                        // ACCEL_DECIMATION acceleration frames.
                        if emit_live {
                            let _ = read_state.raw_frame_tx.send(RawFrame {
                                src: addr.to_string(),
                                ts_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0),
                                frame: frame.clone(),
                            });
                        }

                        // SPEC-R2-WORKSHOP-WIRE §4.1 — observe ACCELERATION frames
                        // to track max_seq_seen for periodic r2.dash.ack
                        // emission. Triggers a send when ACK_SAMPLES (or
                        // ACK_PERIOD_MS, below) has passed.
                        if event_hash == Some(ACCELERATION) && frame.len() > 12 {
                            if let Some(payload) = decode_cbor_payload(&frame[12..]) {
                                if let Some(seq) = payload.get("0").and_then(|v| v.as_u64()) {
                                    let seq32 = seq as u32;
                                    if seq32 > max_seq_seen {
                                        max_seq_seen = seq32;
                                    }
                                    samples_since_ack = samples_since_ack.saturating_add(1);
                                }
                            }
                        }
                        if event_hash == Some(ACCELERATION_BATCH) && frame.len() > 12 {
                            // For batched frames we'd ideally walk the
                            // inner records to pick up the LAST seq. v0.1
                            // sensors don't emit batches yet (catch-up
                            // mode is deferred); leave a TODO once they do.
                        }

                        // Cache the latest capture.state per peer so a
                        // viewer that connects (or hard-refreshes) mid-
                        // recording can have its Run-Control buttons re-sync
                        // to the actual sensor state without waiting for the
                        // next start/mark/stop transition (sensors only emit
                        // capture.state on transitions, not periodically).
                        //
                        // ALSO — SPEC-R2-WORKSHOP-CAPTURE §7.4 — decode the
                        // payload so the auto-sync engine can detect
                        // `Recording → Idle` transitions and spawn a fetch
                        // for the file that just got finalised on the
                        // sensor's SD.
                        if event_hash == Some(SENSOR_CAPTURE_STATE) && frame.len() > 12 {
                            let decoded = decode_capture_state(&frame[12..]);
                            // Snapshot the previous decoded state, then
                            // write the new one. Done under the same
                            // lock so we don't lose a transition under
                            // concurrent state events from one peer.
                            let prev = {
                                let mut peers = read_state.peers.write().await;
                                if let Some(peer) = peers.get_mut(&addr) {
                                    peer.last_capture_state = Some(frame.clone());
                                    let prev = peer.last_capture_decoded.clone();
                                    peer.last_capture_decoded = decoded.clone();
                                    prev
                                } else {
                                    None
                                }
                            };
                            // Recording (2) → Idle (0) transition: the
                            // sensor just fsync'd and closed the file
                            // named in the PREVIOUS state. Spawn a
                            // detached fetch so we don't block the
                            // per-peer dispatch loop on network I/O.
                            if let (Some(prev), Some(new)) = (prev.as_ref(), decoded.as_ref()) {
                                if prev.state == 2 && new.state == 0 {
                                    if let Some(fname) = prev.filename.clone() {
                                        let state_clone = Arc::clone(&read_state);
                                        let addr_str = addr.to_string();
                                        tokio::spawn(async move {
                                            sync_capture_from_sensor(state_clone, addr_str, fname).await;
                                        });
                                    }
                                }
                            }
                        }

                        // SPEC-R2-WORKSHOP-TIMESYNC §3 — handle sync_pong inline,
                        // update peer's smoothed offset, push set_clock_offset
                        // when stable or when drift threshold exceeded.
                        if event_hash == Some(SENSOR_SYNC_PONG) && frame.len() > 12 {
                            if let Some((req_id, sensor_ts_ms)) = decode_sync_pong(&frame[12..]) {
                                handle_sync_pong(
                                    addr,
                                    req_id,
                                    sensor_ts_ms,
                                    &read_state,
                                ).await;
                            }
                        }

                        if event_hash == Some(SENSOR_ANNOUNCE) {
                            let payload = if frame.len() > 12 {
                                decode_cbor_payload(&frame[12..])
                                    .map(|p| remap_payload(SENSOR_ANNOUNCE, p))
                            } else {
                                None
                            };
                            // Our spec calls the friendly label "hostname" (per
                            // SPEC-R2-WORKSHOP-WIRE §3.1 key 1); the legacy M10
                            // schema used "name". Try both.
                            let device_name = payload.as_ref()
                                .and_then(|p| p.get("hostname").or_else(|| p.get("name")))
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string());

                            // Track A — verify the announce signature, with
                            // cert-chain check when CBOR key 8 is present.
                            // TOFU policy retained for legacy announces
                            // (log-only; don't reject yet — see
                            // SPEC-R2-WORKSHOP-SENSOR §3.4).
                            //
                            // Tg_pk loaded once per announce. Cheap (32-byte
                            // copy out of the Access handle); not held across
                            // any await in the verify call.
                            let tg_pk_bytes: Option<[u8; 32]> = match read_state.access.as_ref() {
                                Some(h) => Some(h.lock().await.tg_pk_bytes()),
                                None => None,
                            };
                            let sig_ok = match (&payload, &tg_pk_bytes) {
                                (Some(p), Some(tg_pk)) => verify_announce_signature(p, tg_pk),
                                (Some(_), None) => SigStatus::Malformed, // no TG loaded — treat as legacy
                                (None, _) => SigStatus::NoPayload,
                            };

                            eprintln!(
                                "[events] sensor.announce from {}: name={:?} sig={:?} payload={:?}",
                                addr, device_name, sig_ok, payload
                            );

                            // Pull device_pk out of the parsed announce payload
                            // so downstream consumers (data_merged_handler's
                            // alias lookup, the Track-A cert issuance below) don't
                            // have to re-decode the cached CBOR.
                            let device_pk_hex = payload.as_ref()
                                .and_then(|p| p.get("device_pk"))
                                .and_then(|v| v.as_str())
                                .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
                                .map(|s| s.to_string());

                            // Track A — cert issuance. When the sensor's
                            // announce passes signature verification but
                            // carries no cert (legacy TOFU mode), issue a
                            // fresh KeyHolder-signed DeviceCertificate and
                            // push it down the same TCP socket as
                            // r2.dash.enrol. The sensor persists it to NVS
                            // and the NEXT announce will carry the cert at
                            // CBOR key 8 (post-cert mode). One-shot per
                            // session — idempotent across sensor reconnects.
                            if matches!(sig_ok, SigStatus::Valid) {
                                let tx_opt = read_state.peers.read().await.get(&addr).map(|p| p.tx.clone());
                                if let (Some(pk_hex), Some(handle), Some(tx)) = (
                                    device_pk_hex.clone(),
                                    read_state.access.as_ref(),
                                    tx_opt,
                                ) {
                                    if let Ok(pk_bytes) = hex::decode(&pk_hex) {
                                        if let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) {
                                            let cert_bytes = {
                                                let access = handle.lock().await;
                                                access.issue_sensor_cert(pk_arr)
                                            };
                                            let frame = build_dash_frame(
                                                r2_fnv::fnv1a_32(b"r2.dash.enrol"),
                                                0,
                                                &cert_bytes,
                                            );
                                            if tx.send(frame).await.is_err() {
                                                eprintln!(
                                                    "[enrol] {} peer.tx closed; cert push skipped",
                                                    addr
                                                );
                                            } else {
                                                eprintln!(
                                                    "[enrol] issued + pushed cert ({} bytes) to {} (pk first 8: {})",
                                                    cert_bytes.len(),
                                                    addr,
                                                    &pk_hex[..16]
                                                );
                                            }
                                        }
                                    }
                                }
                            }

                            // Cache the announce frame bytes per peer so a
                            // /r2 viewer that connects later can be
                            // replayed — otherwise it misses `fw_ver` /
                            // `device_pk` / `boot_ts_ms` until the next
                            // sensor reboot.
                            {
                                let mut peers = read_state.peers.write().await;
                                if let Some(peer) = peers.get_mut(&addr) {
                                    if let Some(ref name_str) = device_name {
                                        peer.name = Some(name_str.clone());
                                    }
                                    if let Some(ref pk) = device_pk_hex {
                                        peer.device_pk = Some(pk.clone());
                                    }
                                    peer.last_announce = Some(frame.clone());
                                }
                            }

                            // SPEC-R2-WORKSHOP-CAPTURE §7.4 — immediate
                            // reconciliation pass for this peer the
                            // moment its announce verifies. Eliminates
                            // the 0-60 s blind window where a sensor
                            // that just reset (mid-experiment power
                            // glitch, reboot, etc.) has files on its SD
                            // that the fleet-wide poll hasn't yet seen.
                            // Spawned detached so the per-peer dispatch
                            // loop isn't blocked on network I/O.
                            if let Some(pk) = device_pk_hex.clone() {
                                let ip_only = addr.ip().to_string();
                                let alias = {
                                    let g = read_state.device_aliases.lock().await;
                                    g.get(&pk).cloned()
                                };
                                let raw_name = alias.unwrap_or_else(|| ip_only.replace('.', "_"));
                                let device_safe: String = raw_name.chars()
                                    .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                                    .collect();
                                let recon_state = Arc::clone(&read_state);
                                tokio::spawn(async move {
                                    // Small settle delay — the announce
                                    // arrives before the firmware is
                                    // necessarily ready to serve the
                                    // data_tcp listener (port 21047 is
                                    // a separate task that comes up
                                    // shortly after the streaming TCP).
                                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                    reconcile_single_peer(&recon_state, &ip_only, &pk, &device_safe).await;
                                });
                            }

                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            let event = DashboardEvent {
                                event: "sensor.connected".to_string(),
                                hash: format!("0x{:08X}", SENSOR_ANNOUNCE),
                                timestamp_ms: now,
                                // Pass the announce payload through so the browser
                                // can display fw_ver / device_pk / boot_ts_ms.
                                // Required for OTA decision logic later.
                                payload: payload.clone(),
                                source_addr: Some(addr.to_string()),
                                device_name,
                            };
                            let _ = read_state.event_tx.send(event);
                        } else if let Some(mut event) = decode_event_frame(&frame, &addr) {
                            {
                                let peers = read_state.peers.read().await;
                                if let Some(peer) = peers.get(&addr) {
                                    event.device_name = peer.name.clone();
                                }
                            }

                            // Acceleration decimation already decided at the
                            // top of the frame-loop (see `emit_live`) — same
                            // gate covers /r2 so a viewer
                            // sees consistent per-peer rates regardless of
                            // transport. Per-frame logging removed long ago;
                            // frames are observable via /r2 (binary) or
                            // R2-WIRE event on /r2.
                            if emit_live {
                                let _ = read_state.event_tx.send(event);
                            }
                        }

                        // Per WIRE §4.1: send r2.dash.ack at the
                        // earlier of ACK_PERIOD_MS or ACK_SAMPLES
                        // received. Frees the firmware's SD ring
                        // (SPEC-R2-WORKSHOP-SENSOR §7.4). No-op if we
                        // haven't observed any acceleration frames
                        // yet (max_seq_seen still 0).
                        let now = tokio::time::Instant::now();
                        let should_ack = max_seq_seen > 0
                            && (samples_since_ack >= ACK_SAMPLES || now >= next_ack_at);
                        if should_ack {
                            let dash_ts = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let payload = encode_dash_ack(max_seq_seen, dash_ts);
                            let frame_bytes = build_dash_frame(
                                DASH_ACK,
                                ack_msg_id,
                                &payload,
                            );
                            ack_msg_id = ack_msg_id.wrapping_add(1);
                            // Send via the peer's writer mpsc. Don't
                            // hold the peers lock across await; collect
                            // the tx once if available.
                            let tx = {
                                let peers = read_state.peers.read().await;
                                peers.get(&addr).map(|p| p.tx.clone())
                            };
                            if let Some(tx) = tx {
                                if tx.send(frame_bytes).await.is_err() {
                                    // Writer half died — peer is gone.
                                    // The session will tear down via the
                                    // top-level select on read/write handles.
                                }
                            }
                            samples_since_ack = 0;
                            next_ack_at = now
                                + std::time::Duration::from_millis(ACK_PERIOD_MS);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[events] read error from {}: {}", addr, e);
                    break;
                }
            }
        }
    });

    let write_handle = tokio::spawn(async move {
        while let Some(frame) = cmd_rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = read_handle => {}
        _ = write_handle => {}
    }

    // Capture the peer's `device_pk` BEFORE removal so we can include
    // it in the r2.peer.disconnected event payload — the
    // DashboardViewerSentant keys by pk and needs it to drop the
    // sensor from its snapshot.
    let disconnected_pk_hex: Option<String> = {
        let peers = state.peers.read().await;
        peers.get(&addr).and_then(|p| p.device_pk.clone())
    };
    {
        let mut peers = state.peers.write().await;
        peers.remove(&addr);
    }
    eprintln!("[events] sensor disconnected: {}", addr);
    // Tracks B+C — start the migration from the now-removed /ws/status JSON to R2-WIRE
    // events. The first event picked is `r2.peer.disconnected` because
    // (a) it's purely synthesised by the controller (no sensor side to
    // touch), (b) its payload is tiny, and (c) BRIDGE §3.1 already
    // pre-defines the name + shape, so a future Track E doesn't force
    // a wire break.
    //
    // The frame goes out via raw_frame_tx (same channel as the
    // sensor-originated frames on /r2); the webapp's rocker hive
    // already forwards every /r2 event into the
    // DashboardViewerSentant, so this slot lands in the sentant
    // automatically. The legacy JSON message on /ws/status stays for
    // one release so the existing JS handler (which clears the
    // virtual LED) keeps working until UI rendering moves through
    // the hive snapshot.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let addr_str = addr.to_string();
    let payload = encode_peer_disconnected(
        &addr_str,
        now_ms,
        "tcp_close",
        disconnected_pk_hex.as_deref(),
    );
    let frame = build_dash_frame_body(PEER_DISCONNECTED, 0, &payload);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: addr_str.clone(),
        ts_ms: now_ms,
        frame,
    });
}

/// Broadcast a target-scoped progress notification to viewers — the
/// shared shape behind OTA / reset status events. SPEC-R2-WORKSHOP-WIRE
/// rows 23 (ota.progress) and 24 (reset.progress).
///
/// CBOR payload: `{0: target (text), 1: phase (text),
///                 2: size (uint, optional), 3: message (text, optional)}`.
fn emit_target_progress(
    state: &Arc<AppState>,
    event_hash: u32,
    phase: &str,
    target: &str,
    size: Option<usize>,
    message: Option<&str>,
) {
    // R2-WIRE event on /r2 — picked up by the rocker viewer hive.
    let mut buf = vec![0u8; 64 + target.len() + phase.len() + message.map(|m| m.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 2 + size.is_some() as usize + message.is_some() as usize;
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(target));
    let _ = enc.kv(1, &r2_cbor::Value::Text(phase));
    if let Some(s) = size { let _ = enc.kv(2, &r2_cbor::Value::UInt(s as u64)); }
    if let Some(m) = message { let _ = enc.kv(3, &r2_cbor::Value::Text(m)); }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(event_hash, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: target.to_string(),
        ts_ms: now_ms,
        frame,
    });
}

fn emit_ota_progress(
    state: &Arc<AppState>,
    phase: &str,
    target: &str,
    size: Option<usize>,
    message: Option<&str>,
) {
    emit_target_progress(state, DASH_OTA_PROGRESS, phase, target, size, message);
}

fn emit_reset_progress(
    state: &Arc<AppState>,
    phase: &str,
    target: &str,
    message: Option<&str>,
) {
    emit_target_progress(state, DASH_RESET_PROGRESS, phase, target, None, message);
}

/// Capture-state progress event (fleet-scoped, not per-sensor like the
/// reset/OTA ones). CBOR payload:
///   `{0: phase (text), 1: peers (uint), 2: name (text, optional),
///     3: prefix (text, optional), 4: ts_ms (uint, optional)}`.
fn emit_capture_progress(
    state: &Arc<AppState>,
    phase: &str,
    peers: usize,
    name: Option<&str>,
    prefix: Option<&str>,
    ts_ms: Option<i64>,
) {
    // R2-WIRE event.
    let mut buf = vec![0u8; 64 + phase.len() + name.map(|s| s.len()).unwrap_or(0) + prefix.map(|s| s.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 2 + name.is_some() as usize + prefix.is_some() as usize + ts_ms.is_some() as usize;
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(phase));
    let _ = enc.kv(1, &r2_cbor::Value::UInt(peers as u64));
    if let Some(n) = name   { let _ = enc.kv(2, &r2_cbor::Value::Text(n)); }
    if let Some(p) = prefix { let _ = enc.kv(3, &r2_cbor::Value::Text(p)); }
    if let Some(t) = ts_ms  { let _ = enc.kv(4, &r2_cbor::Value::UInt(t as u64)); }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CAPTURE_PROGRESS, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Access-event broadcast — payload mirrors the JSON shape that
/// `request_pending` / `request_approved` / `request_denied` /
/// `revoked` send today. CBOR payload:
///   `{0: subtype (text), 1: device_pk (text),
///     2: name (text, optional), 3: hint (text, optional)}`.
fn emit_access_event(
    state: &Arc<AppState>,
    subtype: &str,
    device_pk: &str,
    name: Option<&str>,
    hint: Option<&str>,
) {
    let mut buf = vec![0u8; 64 + subtype.len() + device_pk.len()
        + name.map(|s| s.len()).unwrap_or(0)
        + hint.map(|s| s.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 2 + name.is_some() as usize + hint.is_some() as usize;
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(subtype));
    let _ = enc.kv(1, &r2_cbor::Value::Text(device_pk));
    if let Some(n) = name { let _ = enc.kv(2, &r2_cbor::Value::Text(n)); }
    if let Some(h) = hint { let _ = enc.kv(3, &r2_cbor::Value::Text(h)); }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_ACCESS_EVENT, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Device-alias change broadcast. CBOR payload:
///   `{0: device_pk (text), 1: name (text)}` — empty name means alias cleared.
fn emit_device_alias_changed(state: &Arc<AppState>, device_pk: &str, name: &str) {
    let mut buf = vec![0u8; 32 + device_pk.len() + name.len()];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let _ = enc.map(2);
    let _ = enc.kv(0, &r2_cbor::Value::Text(device_pk));
    let _ = enc.kv(1, &r2_cbor::Value::Text(name));
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_DEVICE_ALIAS_CHANGED, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Bootstrap progress — the shape is `{"event": ...}` with a nested
/// kind/data pair. R2-WIRE payload encodes it as
///   `{0: kind (text), 1: data (text, optional)}` where kind is one of
///   "Reset", "Log", "SensorFound", "SensorConnected", "Done", "Error".
/// Broadcast a `r2.dash.cmd.response` correlated to a viewer-issued
/// operator command (SPEC-R2-WORKSHOP-WIRE §2.1). Payload shape:
///   `{0: req_id (u32), 1: status (text),
///     2: message (text, optional), 3: kind (text)}`
///
/// `kind` is the command's name suffix without the `r2.dash.cmd.`
/// prefix (e.g. `"capture.start"`). Sent on `raw_frame_tx`, so every
/// connected viewer sees the reply; viewers correlate by `req_id`.
fn emit_cmd_response(
    state: &Arc<AppState>,
    req_id: u32,
    status: &str,
    message: Option<&str>,
    kind: &str,
) {
    emit_cmd_response_with_extras(state, req_id, status, message, kind, &[]);
}

/// Variant that appends kind-specific text pairs after the standard
/// four keys (SPEC §2.1 "Kind-specific response data"). Used by
/// snapshot/query responses where the payload is one or two JSON-
/// serialised strings — keeps the CBOR-translation surface small at
/// the cost of one JSON.parse on the viewer side.
fn emit_cmd_response_with_extras(
    state: &Arc<AppState>,
    req_id: u32,
    status: &str,
    message: Option<&str>,
    kind: &str,
    extras: &[(u64, &str)],
) {
    let extras_bytes: usize = extras.iter().map(|(_, v)| v.len() + 8).sum();
    let mut buf = vec![0u8; 64 + status.len() + kind.len()
                       + message.map(|m| m.len()).unwrap_or(0)
                       + extras_bytes];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 3 + message.is_some() as usize + extras.len();
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::UInt(req_id as u64));
    let _ = enc.kv(1, &r2_cbor::Value::Text(status));
    if let Some(m) = message { let _ = enc.kv(2, &r2_cbor::Value::Text(m)); }
    let _ = enc.kv(3, &r2_cbor::Value::Text(kind));
    for (k, v) in extras {
        let _ = enc.kv(*k, &r2_cbor::Value::Text(v));
    }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CMD_RESPONSE, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Decode an inbound operator-command frame received on /r2.
/// Returns `(event_hash, req_id, payload_json)` on success.
///
/// The frame body is the bare R2-WIRE compact shape (12-byte header +
/// CBOR payload) — viewers SHOULD send the same shape that
/// `build_dash_frame_body` produces; the /r2 envelope does not
/// apply to viewer-emitted frames because the WebSocket layer already
/// provides message boundaries.
fn decode_cmd_frame(body: &[u8]) -> Option<(u32, u32, serde_json::Value)> {
    if body.len() < 12 {
        return None;
    }
    // Bytes 4..8 = event_hash (BE).
    let event_hash = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let payload = decode_cbor_payload(&body[12..])?;
    let req_id = payload.get("0").and_then(|v| v.as_u64())? as u32;
    Some((event_hash, req_id, payload))
}

/// Broadcast a R2-WIRE `r2.dash.bootstrap.progress` event preserving
/// the BootstrapEvent variant's full field set. Payload shape per
/// SPEC-R2-WORKSHOP-WIRE §2 row 27:
///   {0: kind (text),
///    1: message (text, optional — Log + Error),
///    2: addr    (text, optional — SensorFound + SensorConnected),
///    3: name    (text, optional — SensorFound + SensorConnected),
///    4: ip      (text, optional — SensorConnected),
///    5: count   (uint, optional — Done)}
fn emit_bootstrap_progress(state: &Arc<AppState>, event: &BootstrapEvent) {
    let mut buf = vec![0u8; 256];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    match event {
        BootstrapEvent::Log(s) => {
            let _ = enc.map(2);
            let _ = enc.kv(0, &r2_cbor::Value::Text("Log"));
            let _ = enc.kv(1, &r2_cbor::Value::Text(s));
        }
        BootstrapEvent::SensorFound { addr, name } => {
            let _ = enc.map(3);
            let _ = enc.kv(0, &r2_cbor::Value::Text("SensorFound"));
            let _ = enc.kv(2, &r2_cbor::Value::Text(addr));
            let _ = enc.kv(3, &r2_cbor::Value::Text(name));
        }
        BootstrapEvent::SensorConnected { addr, name, ip } => {
            let _ = enc.map(4);
            let _ = enc.kv(0, &r2_cbor::Value::Text("SensorConnected"));
            let _ = enc.kv(2, &r2_cbor::Value::Text(addr));
            let _ = enc.kv(3, &r2_cbor::Value::Text(name));
            let _ = enc.kv(4, &r2_cbor::Value::Text(ip));
        }
        BootstrapEvent::Done { count } => {
            let _ = enc.map(2);
            let _ = enc.kv(0, &r2_cbor::Value::Text("Done"));
            let _ = enc.kv(5, &r2_cbor::Value::UInt(*count as u64));
        }
        BootstrapEvent::Error(s) => {
            let _ = enc.map(2);
            let _ = enc.kv(0, &r2_cbor::Value::Text("Error"));
            let _ = enc.kv(1, &r2_cbor::Value::Text(s));
        }
        BootstrapEvent::ForeignSensor { addr, class_hash, class_name, rbid } => {
            // Reuse the kind/text/addr keys from SensorFound where it
            // makes sense; add class_hash (key 6) + class_name (key 7)
            // + rbid (key 8) for the foreign-specific fields. Webapp
            // dispatches on `kind` so this won't be confused with a
            // normal SensorFound.
            let n_keys = 4 + if class_name.is_some() { 1 } else { 0 };
            let _ = enc.map(n_keys);
            let _ = enc.kv(0, &r2_cbor::Value::Text("ForeignSensor"));
            let _ = enc.kv(2, &r2_cbor::Value::Text(addr));
            let _ = enc.kv(6, &r2_cbor::Value::UInt(*class_hash as u64));
            if let Some(name) = class_name {
                let _ = enc.kv(7, &r2_cbor::Value::Text(name));
            }
            let _ = enc.kv(8, &r2_cbor::Value::Text(rbid));
        }
    }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_BOOTSTRAP_PROGRESS, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Synthetic Reset event — emitted by the dashboard when the operator
/// (re)triggers bootstrap. Not a BootstrapEvent variant because it's
/// dashboard-side, not from r2_bootstrap. Payload `{0: "Reset"}`; the
/// webapp matches on kind and clears its log panel + sensor cards.
fn emit_bootstrap_reset(state: &Arc<AppState>) {
    let mut buf = [0u8; 16];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(1);
        let _ = enc.kv(0, &r2_cbor::Value::Text("Reset"));
        enc.len()
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_BOOTSTRAP_PROGRESS, 0, &buf[..used]);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Encode the `r2.peer.disconnected` payload per BRIDGE §3.1:
/// `{0: addr (text), 1: ts_ms (uint), 2: reason (text)}`, plus a
/// rocker-specific extension `{3: device_pk_hex (text)}` when the
/// disconnecting peer had identified itself via announce. The hex
/// form (64 chars) matches the announce's `device_pk` field so the
/// DashboardViewerSentant can look up + drop the sensor by pk
/// without keeping its own addr→pk map.
fn encode_peer_disconnected(addr: &str, ts_ms: u64, reason: &str, device_pk_hex: Option<&str>) -> Vec<u8> {
    let mut buf = vec![0u8; 64 + addr.len() + reason.len() + device_pk_hex.map(|s| s.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = if device_pk_hex.is_some() { 4 } else { 3 };
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(addr));
    let _ = enc.kv(1, &r2_cbor::Value::UInt(ts_ms));
    let _ = enc.kv(2, &r2_cbor::Value::Text(reason));
    if let Some(pk) = device_pk_hex {
        let _ = enc.kv(3, &r2_cbor::Value::Text(pk));
    }
    let used = enc.len();
    buf.truncate(used);
    buf
}

/// Decode an R2-WIRE event frame into a DashboardEvent
fn decode_event_frame(frame: &[u8], addr: &SocketAddr) -> Option<DashboardEvent> {
    if frame.len() < 7 {
        return None;
    }

    // R2-WIRE compact frame (12-byte fixed header, SPEC-R2-WORKSHOP-WIRE §1.4):
    //   byte 0:    version|msg_type|flags
    //   byte 1:    ttl|k
    //   bytes 2-3: msg_id (BE u16)
    //   bytes 4-7: event_hash (BE u32)
    //   bytes 8-11: target (BE u32)
    //   bytes 12+: payload
    if frame.len() < 12 {
        return None;
    }
    let _byte0 = frame[0];
    let _byte1 = frame[1];
    let _msg_id = ((frame[2] as u16) << 8) | (frame[3] as u16);
    let event_hash = ((frame[4] as u32) << 24)
        | ((frame[5] as u32) << 16)
        | ((frame[6] as u32) << 8)
        | (frame[7] as u32);
    // bytes 8-11 = target (broadcast 0 for r2-workshop — see firmware/src/wire.rs)

    let payload_bytes = &frame[12..];

    let payload = if !payload_bytes.is_empty() {
        decode_cbor_payload(payload_bytes).map(|p| remap_payload(event_hash, p))
    } else {
        None
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Some(DashboardEvent {
        event: event_name(event_hash).to_string(),
        hash: format!("0x{:08X}", event_hash),
        timestamp_ms: now,
        payload,
        source_addr: Some(addr.to_string()),
        device_name: None,
    })
}

/// Result of `verify_announce_signature` — `Valid` is the only "good"
/// state. Other variants log loudly so misconfiguration is visible.
#[derive(Debug, Clone, Copy)]
enum SigStatus {
    /// Announce sig verifies AND a cert at CBOR key 8 verifies under the
    /// dashboard's TG_PUB_KEY, with `cert.device_public_key` matching the
    /// announce's `device_pk`. This is the post-Track-A normative mode.
    ValidWithCert,
    /// Announce sig verifies; no cert present (legacy TOFU mode).
    Valid,
    /// Signature bytes don't verify against the announced device_pk.
    /// Means either the firmware is buggy, the network is forging
    /// announces, or the canonical CBOR re-encoding doesn't match.
    BadSignature,
    /// Cert at CBOR key 8 either fails to verify under TG_PUB_KEY, or
    /// the cert's `device_public_key` doesn't match the announce's
    /// `device_pk`, or the cert is expired. The announce signature
    /// itself may still be well-formed; we reject because the
    /// cert-anchored chain is broken (per SPEC-R2-WORKSHOP-SENSOR §3.4
    /// post-cert mode).
    BadCert,
    /// Required field missing / wrong type. Often a legacy M10 announce
    /// (no signature field at all) — log-and-accept under TOFU for now.
    Malformed,
    /// No payload at all — same as legacy.
    NoPayload,
}

/// Phase 5b — re-encode the canonical body (keys 0..5) per
/// SPEC-R2-WORKSHOP-WIRE §3.1 and Ed25519-verify the signature at key 6.
///
/// The firmware signs over the canonical CBOR encoding of keys 0..5.
/// Both sides use deterministic CBOR (smallest-form heads, ascending
/// integer keys), so a fresh encode here MUST match the firmware's
/// signed bytes exactly.
///
/// Track A — if the announce includes CBOR key 8 (`device_cert`,
/// 147 bytes), the dashboard ALSO verifies the cert chain under
/// `tg_pk` and checks that the cert's `device_public_key` matches
/// the announce's `device_pk`. Returns `ValidWithCert` on success,
/// `BadCert` on chain failure. Legacy announces (no key 8) fall
/// back to plain `Valid` (TOFU mode).
fn verify_announce_signature(payload: &serde_json::Value, tg_pk: &[u8; 32]) -> SigStatus {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let obj = match payload.as_object() {
        Some(o) => o,
        None => return SigStatus::Malformed,
    };

    let hex_field = |key: &str, len: usize| -> Option<Vec<u8>> {
        let s = obj.get(key)?.as_str()?;
        let b = hex::decode(s).ok()?;
        if b.len() == len { Some(b) } else { None }
    };
    let hex_field_any = |key: &str| -> Option<Vec<u8>> {
        obj.get(key)?.as_str().and_then(|s| hex::decode(s).ok())
    };
    let text_field = |key: &str| -> Option<&str> {
        obj.get(key)?.as_str()
    };
    let uint_field = |key: &str| -> Option<u64> {
        obj.get(key)?.as_u64()
    };

    let (Some(device_pk), Some(hostname), Some(fw_ver), Some(last_seq), Some(boot_ts_ms), Some(nonce), Some(sig)) = (
        hex_field("device_pk", 32),
        text_field("hostname"),
        text_field("fw_ver"),
        uint_field("last_seq"),
        uint_field("boot_ts_ms"),
        hex_field("nonce", 16),
        hex_field("sig", 64),
    ) else {
        return SigStatus::Malformed;
    };

    // Refuse the all-zero placeholder sig that pre-Phase-5a firmware emits.
    if sig.iter().all(|b| *b == 0) {
        return SigStatus::Malformed;
    }

    // Re-encode the canonical body bytes. Keys MUST be in ascending order
    // (we write 0..5 directly) and integer-keyed for byte-identical output
    // with the firmware's inline encoder.
    let mut body_buf = vec![0u8; 256 + hostname.len() + fw_ver.len()];
    let mut enc = r2_cbor::Encoder::new(&mut body_buf);
    if enc.map(6).is_err()
        || enc.kv(0, &r2_cbor::Value::Bytes(&device_pk)).is_err()
        || enc.kv(1, &r2_cbor::Value::Text(hostname)).is_err()
        || enc.kv(2, &r2_cbor::Value::Text(fw_ver)).is_err()
        || enc.kv(3, &r2_cbor::Value::UInt(last_seq)).is_err()
        || enc.kv(4, &r2_cbor::Value::UInt(boot_ts_ms)).is_err()
        || enc.kv(5, &r2_cbor::Value::Bytes(&nonce)).is_err()
    {
        return SigStatus::Malformed;
    }
    let body = enc.as_bytes();

    let pk_arr: [u8; 32] = device_pk.as_slice().try_into().unwrap();
    let sig_arr: [u8; 64] = sig.as_slice().try_into().unwrap();
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_arr) else {
        return SigStatus::Malformed;
    };
    let signature = Signature::from_bytes(&sig_arr);
    if verifying_key.verify(body, &signature).is_err() {
        return SigStatus::BadSignature;
    }

    // Announce sig OK. Check for a cert at key 8 (Track A). The CBOR
    // decoder writes bytes(N) fields as hex strings into our JSON
    // intermediate; same accessor as `device_pk` / `nonce`. Length is
    // 147 (DEVICE_CERT_LEN) when present.
    let Some(cert_bytes) = hex_field_any("device_cert") else {
        // Legacy / pre-cert announce — TOFU accept per SPEC-R2-WORKSHOP-SENSOR §3.4.
        return SigStatus::Valid;
    };
    if cert_bytes.len() != 147 {
        return SigStatus::BadCert;
    }
    // 1. Verify the cert's trailing 64-byte signature over the leading
    //    83 bytes under the dashboard's TG_PUB_KEY.
    let signed = &cert_bytes[..83];
    let Ok(cert_sig_arr) = <[u8; 64]>::try_from(&cert_bytes[83..]) else {
        return SigStatus::BadCert;
    };
    let Ok(tg_vk) = VerifyingKey::from_bytes(tg_pk) else {
        return SigStatus::BadCert;
    };
    let cert_sig = Signature::from_bytes(&cert_sig_arr);
    if tg_vk.verify(signed, &cert_sig).is_err() {
        return SigStatus::BadCert;
    }
    // 2. Cert's device_public_key (bytes 2..34) must match announce's device_pk.
    if &cert_bytes[2..34] != device_pk.as_slice() {
        return SigStatus::BadCert;
    }
    // 3. Expiry check — cert.expires_at at bytes 75..83 (big-endian u64).
    let expires_at = u64::from_be_bytes(cert_bytes[75..83].try_into().unwrap_or([0u8; 8]));
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now_secs >= expires_at {
        return SigStatus::BadCert;
    }
    SigStatus::ValidWithCert
}

/// Decode CBOR payload into JSON
fn decode_cbor_payload(data: &[u8]) -> Option<serde_json::Value> {
    let mut decoder = r2_cbor::Decoder::new(data);
    cbor_to_json(&mut decoder).ok()
}

/// Recursively convert CBOR items to serde_json::Value
fn cbor_to_json(decoder: &mut r2_cbor::Decoder) -> Result<serde_json::Value, ()> {
    match decoder.next().map_err(|_| ())? {
        r2_cbor::Item::UInt(v) => Ok(serde_json::Value::Number(v.into())),
        r2_cbor::Item::NegInt(v) => Ok(serde_json::Value::Number(v.into())),
        r2_cbor::Item::Bytes(b) => {
            Ok(serde_json::Value::String(hex::encode(b)))
        }
        r2_cbor::Item::Text(s) => {
            Ok(serde_json::Value::String(String::from_utf8_lossy(s).into_owned()))
        }
        r2_cbor::Item::Array(n) => {
            let mut arr = Vec::new();
            for _ in 0..n {
                arr.push(cbor_to_json(decoder)?);
            }
            Ok(serde_json::Value::Array(arr))
        }
        r2_cbor::Item::Map(n) => {
            let mut map = serde_json::Map::new();
            for _ in 0..n {
                let key = cbor_to_json(decoder)?;
                let val = cbor_to_json(decoder)?;
                let key_str = match key {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                map.insert(key_str, val);
            }
            Ok(serde_json::Value::Object(map))
        }
        r2_cbor::Item::Bool(b) => Ok(serde_json::Value::Bool(b)),
        r2_cbor::Item::Null => Ok(serde_json::Value::Null),
        r2_cbor::Item::Float16Raw(bits) => {
            let f = f32::from_bits(half_to_f32_bits(bits)) as f64;
            Ok(serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
        r2_cbor::Item::Float32(f) => {
            Ok(serde_json::Number::from_f64(f as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
        r2_cbor::Item::Float64(f) => {
            Ok(serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
    }
}

/// Convert IEEE 754 half-precision (16-bit) to single-precision (32-bit) bits
fn half_to_f32_bits(h: u16) -> u32 {
    let sign = (h >> 15) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;

    if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            let mut e = 0u32;
            let mut m = mant;
            while (m & 0x400) == 0 {
                m <<= 1;
                e += 1;
            }
            (sign << 31) | ((127 - 15 - e) << 23) | ((m & 0x3FF) << 13)
        }
    } else if exp == 31 {
        (sign << 31) | (0xFF << 23) | (mant << 13)
    } else {
        (sign << 31) | ((exp + 112) << 23) | (mant << 13)
    }
}

/// Hex helpers (kept local to avoid pulling in the external crate just
/// for these two functions; the `hex` 0.4 dep IS in Cargo.toml for
/// other consumers but this local module shadows it inside main.rs).
mod hex {
    pub fn encode(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Decode a hex string to bytes. Accepts lowercase or uppercase. Returns
    /// `Err(())` on any non-hex character or odd length.
    pub fn decode(s: &str) -> Result<Vec<u8>, ()> {
        let bytes = s.as_bytes();
        if bytes.len() % 2 != 0 { return Err(()); }
        let nibble = |c: u8| -> Result<u8, ()> {
            match c {
                b'0'..=b'9' => Ok(c - b'0'),
                b'a'..=b'f' => Ok(c - b'a' + 10),
                b'A'..=b'F' => Ok(c - b'A' + 10),
                _ => Err(()),
            }
        };
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks_exact(2) {
            out.push((nibble(chunk[0])? << 4) | nibble(chunk[1])?);
        }
        Ok(out)
    }
}

// ── Phase 5d — endpoints for the WASM viewer ──────────────────────────────

/// `/r2` — push raw R2-WIRE frame bytes to a connected WASM viewer.
///
/// Each WS binary message is one frame, wrapped in a small TLV envelope:
///
/// ```
///   [u16 BE: src_addr length n]
///   [n bytes UTF-8: src_addr]
///   [u32 BE: ts_ms_low32]
///   [u16 BE: frame length m]
///   [m bytes:  R2-WIRE compact frame]
/// ```
///
/// Source addr lets the browser key per-peer state. ts_ms is the
/// controller's wall-clock arrival time (low 32 bits — wraps every
/// ~49 days, matches the firmware's ts_ms field width). Frame is the
/// raw R2-WIRE compact frame: header + payload, no transport prefix.
/// Dispatch a viewer-emitted operator-command frame received on
/// /r2. Per SPEC-R2-WORKSHOP-WIRE §2.1, malformed frames and
/// unknown event hashes are dropped silently; everything else hits
/// the shared do_* core and yields a `r2.dash.cmd.response` reply
/// correlated by `req_id`.
async fn dispatch_cmd_frame(state: &Arc<AppState>, peer_addr: SocketAddr, body: &[u8]) {
    let (event_hash, req_id, payload) = match decode_cmd_frame(body) {
        Some(t) => t,
        None => {
            eprintln!("[r2 inbound] malformed frame (len={}) — ignoring", body.len());
            return;
        }
    };
    eprintln!("[r2 inbound] event_hash=0x{:08x} req_id={} from {}",
              event_hash, req_id, peer_addr);
    match event_hash {
        DASH_CMD_CAPTURE_START => {
            let _peers = do_capture_start(state).await;
            emit_cmd_response(state, req_id, "ok", None, "capture.start");
        }
        DASH_CMD_CAPTURE_MARK => {
            let name = payload.get("1").and_then(|v| v.as_str()).map(|s| s.to_string());
            let prefix = payload.get("2").and_then(|v| v.as_str()).map(|s| s.to_string());
            let name = match name {
                Some(n) => n,
                None => {
                    emit_cmd_response(state, req_id, "err", Some("missing name (key 1)"), "capture.mark");
                    return;
                }
            };
            match do_capture_mark(state, &name, prefix.as_deref()).await {
                Ok(_) => emit_cmd_response(state, req_id, "ok", None, "capture.mark"),
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "capture.mark"),
            }
        }
        DASH_CMD_CAPTURE_STOP => {
            let _peers = do_capture_stop(state).await;
            emit_cmd_response(state, req_id, "ok", None, "capture.stop");
        }
        DASH_CMD_CAPTURE_EVENT_MARK => {
            // SPEC-R2-WORKSHOP-CAPTURE §7.5: operator-paced annotation
            // injected into the active capture's sidecar. Label
            // defaults to "mark" if omitted/empty.
            let label_raw = payload.get("1").and_then(|v| v.as_str()).unwrap_or("");
            let label_owned;
            let label: &str = if label_raw.is_empty() {
                "mark"
            } else if label_raw.len() > 64 {
                // RFC-4180 escaping is sensor-side; we just gate length
                // before letting the bytes loose on the wire.
                label_owned = label_raw[..64].to_string();
                &label_owned
            } else {
                label_raw
            };
            match do_capture_event_mark(state, label).await {
                Ok((sent, _ts_ms, _mark_id)) if sent > 0 => {
                    emit_cmd_response(state, req_id, "ok", None, "capture.event_mark");
                }
                Ok(_) => {
                    emit_cmd_response(state, req_id, "err", Some("no active capture"), "capture.event_mark");
                }
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "capture.event_mark"),
            }
        }
        DASH_CMD_RESET => {
            let addr = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    emit_cmd_response(state, req_id, "err", Some("missing addr (key 1)"), "reset");
                    return;
                }
            };
            match do_reset(state, &addr).await {
                Ok((status_byte, msg)) if status_byte == 0x00 => {
                    emit_cmd_response(state, req_id, "ok", Some(&msg), "reset");
                }
                Ok((_status_byte, msg)) => {
                    emit_cmd_response(state, req_id, "err", Some(&msg), "reset");
                }
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "reset"),
            }
        }
        DASH_CMD_IDENTIFY => {
            let addr = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    emit_cmd_response(state, req_id, "err", Some("missing addr (key 1)"), "identify");
                    return;
                }
            };
            let on = payload.get("2").and_then(|v| v.as_bool()).unwrap_or(false);
            match do_identify(state, &addr, on).await {
                Ok(()) => emit_cmd_response(state, req_id, "ok", None, "identify"),
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "identify"),
            }
        }
        DASH_CMD_BOOTSTRAP => {
            eprintln!("[r2 cmd] bootstrap: calling do_bootstrap req_id={}", req_id);
            do_bootstrap(state).await;
            eprintln!("[r2 cmd] bootstrap: do_bootstrap returned, emitting response req_id={}", req_id);
            emit_cmd_response(state, req_id, "ok", Some("started"), "bootstrap");
            eprintln!("[r2 cmd] bootstrap: response emitted req_id={}", req_id);
        }
        DASH_CMD_DEVICE_ALIAS_SET => {
            let device_pk = payload.get("1").and_then(|v| v.as_str()).unwrap_or("");
            let name = payload.get("2").and_then(|v| v.as_str()).unwrap_or("");
            match do_device_alias_set(state, device_pk, name).await {
                Ok(_) => emit_cmd_response(state, req_id, "ok", None, "device.alias.set"),
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "device.alias.set"),
            }
        }
        // ── Access bundle ──────────────────────────────────────────
        //
        // KeyHolder-only ops (members/pending/approve/deny/revoke) use the
        // same loopback gate as the HTTP /api/access/* handlers (ACCESS
        // §11.1). request + check are open since they're how a new viewer
        // enters the system. The cert-handshake variant of this gate lands
        // with ACCESS v1.0.
        DASH_CMD_ACCESS_MEMBERS_QUERY => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.members.query"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.members.query");
                return;
            }
            let rows = { handle.lock().await.members() };
            let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            emit_cmd_response_with_extras(state, req_id, "ok", None, "access.members.query", &[(4, &json)]);
        }
        DASH_CMD_ACCESS_PENDING_QUERY => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.pending.query"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.pending.query");
                return;
            }
            let rows = { handle.lock().await.pending_requests() };
            let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            emit_cmd_response_with_extras(state, req_id, "ok", None, "access.pending.query", &[(4, &json)]);
        }
        DASH_CMD_ACCESS_CHECK => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.check"); return; }
            };
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.check"); return; }
            };
            let outcome = { handle.lock().await.check_request(&device_pk) };
            use access::CheckOutcome::*;
            match outcome {
                Approved(body) => {
                    let body_json = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
                    emit_cmd_response_with_extras(state, req_id, "ok", None, "access.check",
                        &[(4, "approved"), (5, &body_json)]);
                }
                Pending => emit_cmd_response_with_extras(state, req_id, "ok", None, "access.check", &[(4, "pending")]),
                Denied  => emit_cmd_response_with_extras(state, req_id, "ok", None, "access.check", &[(4, "denied")]),
                NotFound => emit_cmd_response(state, req_id, "err", Some("no such request"), "access.check"),
                BadRequest => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.check"),
            }
        }
        DASH_CMD_ACCESS_APPROVE => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.approve"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.approve");
                return;
            }
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.approve"); return; }
            };
            let (outcome, response_body) = {
                let mut access = handle.lock().await;
                let o = access.approve_request(&device_pk);
                let body = access.peek_response(&device_pk);
                (o, body)
            };
            use access::ApproveOutcome::*;
            match outcome {
                Approved(pk) => {
                    let pk_hex = hex::encode(&pk[..]);
                    emit_access_event(state, "request_approved", &pk_hex, None, None);
                    // Push the JOIN_RESPONSE binary frame onto the relay's
                    // outbound channel so off-network viewers receive their
                    // bundle without polling — identical to the HTTP path.
                    if let (Some(tx), Some(body)) = (state.relay_binary_tx.as_ref(), response_body) {
                        use base64::Engine as _;
                        let tg_pk_hex = body.get("tg_pk_hex").and_then(|v| v.as_str());
                        let enc_b64   = body.get("encrypted_b64").and_then(|v| v.as_str());
                        if let (Some(tg_pk_hex), Some(enc_b64)) = (tg_pk_hex, enc_b64) {
                            let tg_pk_vec = hex::decode(tg_pk_hex).unwrap_or_default();
                            let encrypted = base64::engine::general_purpose::STANDARD
                                .decode(enc_b64).unwrap_or_default();
                            if tg_pk_vec.len() == 32 && !encrypted.is_empty() {
                                let mut tg_pk = [0u8; 32];
                                tg_pk.copy_from_slice(&tg_pk_vec);
                                let frame = relay::build_join_response(&pk, &tg_pk, &encrypted);
                                let _ = tx.send(frame);
                            } else {
                                eprintln!("[access] approve: malformed response body, can't build JOIN_RESPONSE");
                            }
                        }
                    }
                    emit_cmd_response(state, req_id, "ok", None, "access.approve");
                }
                NotFound        => emit_cmd_response(state, req_id, "err", Some("no such pending request"), "access.approve"),
                AlreadyApproved => emit_cmd_response(state, req_id, "err", Some("already approved"), "access.approve"),
                Denied          => emit_cmd_response(state, req_id, "err", Some("request was already denied"), "access.approve"),
                BadRequest      => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.approve"),
                Failed(e)       => emit_cmd_response(state, req_id, "err", Some(&e), "access.approve"),
            }
        }
        DASH_CMD_ACCESS_DENY => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.deny"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.deny");
                return;
            }
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.deny"); return; }
            };
            let outcome = { handle.lock().await.deny_request(&device_pk) };
            use access::DenyOutcome::*;
            match outcome {
                Denied(pk) => {
                    emit_access_event(state, "request_denied", &hex::encode(&pk[..]), None, None);
                    emit_cmd_response(state, req_id, "ok", None, "access.deny");
                }
                NotFound   => emit_cmd_response(state, req_id, "err", Some("no such pending request"), "access.deny"),
                BadRequest => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.deny"),
            }
        }
        DASH_CMD_ACCESS_REVOKE => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.revoke"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.revoke");
                return;
            }
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.revoke"); return; }
            };
            let outcome = { handle.lock().await.revoke(&device_pk) };
            use access::RevokeOutcome::*;
            match outcome {
                Revoked(pk) => {
                    emit_access_event(state, "revoked", &hex::encode(&pk[..]), None, None);
                    emit_cmd_response(state, req_id, "ok", None, "access.revoke");
                }
                NotFound   => emit_cmd_response(state, req_id, "err", Some("no such member (already revoked, or never paired)"), "access.revoke"),
                BadRequest => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.revoke"),
                Other(e)   => emit_cmd_response(state, req_id, "err", Some(&e), "access.revoke"),
            }
        }
        DASH_CMD_ACCESS_REQUEST => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.request"); return; }
            };
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.request"); return; }
            };
            let name = payload.get("2").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // Default hint to the WS peer's IP if absent — mirrors the
            // HTTP handler's behaviour, which derives hint from the
            // request socket.
            let hint = payload.get("3").and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| peer_addr.ip().to_string());
            let outcome = { handle.lock().await.submit_request(&device_pk, &name, &hint) };
            use access::RequestOutcome::*;
            match outcome {
                Submitted(pk) => {
                    let pk_hex = hex::encode(&pk[..]);
                    emit_access_event(state, "request_pending", &pk_hex, Some(&name), Some(&hint));
                    emit_cmd_response(state, req_id, "ok", None, "access.request");
                }
                BadRequest(msg) => emit_cmd_response(state, req_id, "err", Some(msg), "access.request"),
            }
        }
        _ => {
            // Unknown hash — log and drop per WIRE §2 "non-actionable".
            // No response emitted, per §2.1's failure-modes table.
            eprintln!("[r2 inbound] unknown event hash 0x{:08x} — ignoring", event_hash);
        }
    }
}

async fn ws_raw_handler(
    ws: WebSocketUpgrade,
    state: Arc<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_raw(socket, state, addr))
}

async fn handle_ws_raw(mut socket: WebSocket, state: Arc<AppState>, peer_addr: SocketAddr) {
    let mut rx = state.raw_frame_tx.subscribe();
    eprintln!("[r2 ws] viewer connected from {}", peer_addr);

    // Replay cached announce frames per peer so a freshly-connected
    // viewer sees `fw_ver` / `device_pk` / `boot_ts_ms` immediately,
    // not "after the next sensor reboot." The announce only fires on
    // TCP (re)connect, so without replay a viewer that arrives mid-
    // session never learns these fields. Use the actual reception
    // timestamp where we have it; fall back to "now" otherwise.
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let peers = state.peers.read().await;
        for (addr, peer) in peers.iter() {
            if let Some(ref frame) = peer.last_announce {
                let envelope = encode_raw_frame_envelope(&RawFrame {
                    src: addr.to_string(),
                    ts_ms: now_ms,
                    frame: frame.clone(),
                });
                if socket.send(Message::Binary(envelope.into())).await.is_err() {
                    return;
                }
            }
            // Replay the last capture.state too so the Run-Control bar
            // reflects the actual recording state, not the IDLE default,
            // when the operator refreshes mid-session.
            if let Some(ref frame) = peer.last_capture_state {
                let envelope = encode_raw_frame_envelope(&RawFrame {
                    src: addr.to_string(),
                    ts_ms: now_ms,
                    frame: frame.clone(),
                });
                if socket.send(Message::Binary(envelope.into())).await.is_err() {
                    return;
                }
            }
        }

        // SPEC-R2-WORKSHOP-CAPTURE §7.4: replay the controller-local
        // capture index as `r2.dash.capture.synced` events so the Data
        // tab is populated on first open — no `/api/data/local/list`
        // round-trip, no HTTP coupling. Same path works LAN + relay:
        // remote viewers see the index over their WS even though the
        // file-blob `/api` routes aren't reachable from their origin.
        let sessions = state.captures.list_sessions().await;
        for session in &sessions {
            for entry in &session.files {
                let kind_str = match entry.kind {
                    captures::CaptureKind::Data => "data",
                    captures::CaptureKind::Marks => "marks",
                };
                let mut buf = vec![0u8; 64 + entry.device_pk.len() + entry.sensor_filename.len() + kind_str.len()];
                let mut enc = r2_cbor::Encoder::new(&mut buf);
                let _ = enc.map(5);
                let _ = enc.kv(1, &r2_cbor::Value::Text(&entry.device_pk));
                let _ = enc.kv(2, &r2_cbor::Value::Text(&entry.sensor_filename));
                let _ = enc.kv(3, &r2_cbor::Value::UInt(entry.size));
                let _ = enc.kv(4, &r2_cbor::Value::UInt(entry.fetched_at_ms));
                let _ = enc.kv(5, &r2_cbor::Value::Text(kind_str));
                let used = enc.len();
                buf.truncate(used);
                let frame = build_dash_frame_body(DASH_CAPTURE_SYNCED, 0, &buf);
                let envelope = encode_raw_frame_envelope(&RawFrame {
                    src: "dash".to_string(),
                    ts_ms: now_ms,
                    frame,
                });
                if socket.send(Message::Binary(envelope.into())).await.is_err() {
                    return;
                }
            }
        }
    }

    loop {
        tokio::select! {
            // Inbound: operator-plane commands per SPEC-R2-WORKSHOP-WIRE
            // §2.1. Viewer hives emit r2.dash.cmd.* events as bare
            // R2-WIRE compact bodies (no length prefix; WebSocket
            // provides message boundaries). We decode, dispatch to the
            // shared do_* core, and emit a r2.dash.cmd.response back
            // on raw_frame_tx (broadcast to all viewers; correlated by
            // req_id).
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(Message::Binary(bytes))) => {
                        dispatch_cmd_frame(&state, peer_addr, &bytes).await;
                    }
                    _ => {} // text / ping / pong — ignore
                }
            }
            // Outbound: a fresh raw frame from the TCP listener.
            frame_msg = rx.recv() => {
                match frame_msg {
                    Ok(rf) => {
                        // Surface cmd.response and unknown low-volume events
                        // for the Track C migration triage. Acceleration (~10
                        // Hz × N peers) is too noisy; suppress it explicitly.
                        if rf.frame.len() >= 8 {
                            let h = u32::from_be_bytes([rf.frame[4], rf.frame[5], rf.frame[6], rf.frame[7]]);
                            if h != ACCELERATION {
                                eprintln!("[r2 outbound] event_hash=0x{:08x} src={} to {}",
                                          h, rf.src, peer_addr);
                            }
                        }
                        let envelope = encode_raw_frame_envelope(&rf);
                        if socket.send(Message::Binary(envelope.into())).await.is_err() {
                            eprintln!("[r2 outbound] socket.send FAILED for {} — viewer gone", peer_addr);
                            break;
                        }
                    }
                    // Lagged — viewer fell behind. Skip the gap; live data
                    // is preferred over backfill on the live wire (the
                    // SD ring is the durability layer).
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[r2 outbound] viewer {} LAGGED by {} frames", peer_addr, n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    eprintln!("[r2 ws] viewer disconnected");
}

/// `/ws/logs/{addr}` — per-sensor live log tail.
///
/// Opens a TCP socket to `<addr>:21046` (the firmware's `log_tcp`
/// listener) and pipes each newline-terminated line back to the WS
/// client as a text frame. Closes when either side disconnects.
///
/// `addr` may be either a bare IP or `ip:port`; the sensor port suffix
/// is stripped since the log listener is on the well-known port.
async fn ws_logs_handler(
    Path(addr): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_logs(socket, addr))
}

async fn handle_ws_logs(mut socket: WebSocket, addr: String) {
    let ip_only: &str = addr.split(':').next().unwrap_or(&addr);
    let target = format!("{}:21046", ip_only);
    eprintln!("[ws/logs] viewer requested tail of {}", target);

    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        TcpStream::connect(&target),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let _ = socket
                .send(Message::Text(
                    format!("[ws/logs] connect to {} failed: {}\n", target, e).into(),
                ))
                .await;
            return;
        }
        Err(_) => {
            let _ = socket
                .send(Message::Text(
                    format!("[ws/logs] connect to {} timed out\n", target).into(),
                ))
                .await;
            return;
        }
    };

    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        tokio::select! {
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // ignore client → server messages
                }
            }
            n = reader.read_line(&mut line) => {
                match n {
                    Ok(0) => break, // sensor closed the socket
                    Ok(_) => {
                        if socket.send(Message::Text(line.clone().into())).await.is_err() {
                            break;
                        }
                        line.clear();
                    }
                    Err(_) => break,
                }
            }
        }
    }
    eprintln!("[ws/logs] tail of {} closed", target);
}

/// `GET /api/firmware/available` — latest firmware snapshot.
///
/// Tries GitHub Releases first (latest non-draft release on the
/// `reality2-ai/r2-workshop` repo); falls back to the highest-mtime
/// .bin in `firmware/esp32-s3/<carrier>/releases/`. Cached for
/// `FIRMWARE_CACHE_TTL_SECS` so the webapp can poll every few
/// seconds without hammering the GitHub API rate limit (60/hr
/// unauthenticated per IP).
async fn firmware_available_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    {
        let cache = state.firmware_cache.lock().await;
        if let Some(ref entry) = *cache {
            let age_s = (now_ms.saturating_sub(entry.fetched_at_ms)) / 1000;
            if age_s < FIRMWARE_CACHE_TTL_SECS {
                return (axum::http::StatusCode::OK, Json(serde_json::to_value(entry).unwrap_or(serde_json::json!({})))).into_response();
            }
        }
    }

    let snapshot = build_firmware_snapshot(now_ms).await;

    {
        let mut cache = state.firmware_cache.lock().await;
        *cache = Some(snapshot.clone());
    }

    (axum::http::StatusCode::OK, Json(serde_json::to_value(&snapshot).unwrap_or(serde_json::json!({})))).into_response()
}

/// `GET /api/firmware/{carrier}/binary` — fetch the matching .bin.
///
/// If the cached snapshot was sourced from GitHub, redirects (302) to
/// the release asset URL — the browser then fetches the bytes from
/// GitHub's CDN directly. If sourced from a local releases dir, the
/// dashboard streams the file from disk.
async fn firmware_binary_handler(
    State(state): State<Arc<AppState>>,
    Path(carrier): Path<String>,
) -> impl IntoResponse {
    let snapshot = {
        let cache = state.firmware_cache.lock().await;
        cache.clone()
    };
    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            // No cache yet — synthesise one. Webapp normally hits
            // /available before /binary, so this is a corner case.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let snap = build_firmware_snapshot(now_ms).await;
            let mut cache = state.firmware_cache.lock().await;
            *cache = Some(snap.clone());
            snap
        }
    };

    let asset = snapshot.assets.iter().find(|a| a.carrier == carrier);
    let asset = match asset {
        Some(a) => a,
        None => return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("no firmware available for carrier {}", carrier) })),
        ).into_response(),
    };

    if snapshot.source == "github" {
        // Proxy the asset through the dashboard rather than 302-ing
        // the browser to GitHub's CDN — GitHub release-download URLs
        // don't include `Access-Control-Allow-Origin`, so a redirect
        // from a webapp `fetch()` gets blocked by CORS. Streaming
        // via curl here keeps the request same-origin from the
        // browser's perspective.
        let asset_url = asset.url.clone();
        let output = tokio::process::Command::new("curl")
            .args([
                "-sSL",                // follow redirects (GH issues a redirect to S3)
                "--max-time", "60",
                "-H", "User-Agent: r2-workshop-dashboard",
                &asset_url,
            ])
            .output()
            .await;
        return match output {
            Ok(out) if out.status.success() => (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                out.stdout,
            ).into_response(),
            Ok(out) => (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("curl proxy of {} failed: status {}", asset_url, out.status),
                    "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
                })),
            ).into_response(),
            Err(e) => (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("curl spawn failed: {}", e) })),
            ).into_response(),
        };
    }

    // Local source — read the file from disk and stream it back.
    let path = std::path::PathBuf::from(&asset.url); // "url" is the local path for local source
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        ).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("read {:?}: {}", path, e) })),
        ).into_response(),
    }
}

/// Build a fresh FirmwareAvailable snapshot by querying GitHub then
/// falling back to the local releases dir.
async fn build_firmware_snapshot(now_ms: u64) -> FirmwareAvailable {
    // Query GitHub Releases AND scan the local releases/ dir in
    // parallel, then pick whichever is newer. v0.1 of this endpoint
    // preferred GitHub unconditionally, which broke the day-to-day
    // dev loop: every fresh local build was ignored in favour of the
    // stale GitHub Release tag. v0.2 compares the local newest-mtime
    // against the GitHub release's `published_at` and picks the
    // newer one, so a freshly-built local .bin always wins until the
    // operator cuts a fresh tag.

    let local = local_firmware_snapshot();
    let github = github_firmware_snapshot().await;

    let prefer_local = match (&local, &github) {
        (Some((_, l_mtime, _)), Some((_, g_secs, _))) => *l_mtime > *g_secs,
        (Some(_), None)  => true,
        (None,    Some(_)) => false,
        (None, None) => return FirmwareAvailable {
            source: "none".to_string(),
            class: ENSEMBLE_CLASS.to_string(),
            version: String::new(),
            assets: Vec::new(),
            note: Some("No firmware found on GitHub or in local releases/".to_string()),
            fetched_at_ms: now_ms,
        },
    };

    if prefer_local {
        let (assets, _, latest_version) = local.expect("checked above");
        FirmwareAvailable {
            source: "local".to_string(),
            class: ENSEMBLE_CLASS.to_string(),
            version: latest_version,
            assets,
            note: github.as_ref().map(|(tag, _, _)| {
                format!("Local build is newer than GitHub release {} — preferring local.", tag)
            }),
            fetched_at_ms: now_ms,
        }
    } else {
        let (tag, _, assets) = github.expect("checked above");
        FirmwareAvailable {
            source: "github".to_string(),
            class: ENSEMBLE_CLASS.to_string(),
            version: tag,
            assets,
            note: None,
            fetched_at_ms: now_ms,
        }
    }
}

/// `firmware/<soc-family>/<carrier>/releases/` trees the local fallback
/// walks (SPEC-R2-WORKSHOP-DASHBOARD §13.3, source 2). One entry per known
/// carrier; the carrier is fixed by the directory, not parsed from the file.
const LOCAL_CARRIER_DIRS: &[(&str, &str)] = &[
    ("esp32-s3", "devkitc"),
    ("esp32-s3", "xiao"),
    ("esp32-c6", "dfr1117"),
];

/// Pick the newest `.bin` per carrier under
/// `firmware/<soc-family>/<carrier>/releases/`. The carrier comes from the
/// directory; class + version + sha256 come from the `<bin>.meta.json`
/// sidecar when present (authoritative, SPEC §13.3) and fall back to the
/// dashboard's own class + a filename-parsed version otherwise (pre-v0.3
/// local archives carry no sidecar). Foreign-class sidecars are skipped —
/// the operator's dashboard never offers a foreign-class binary. Returns
/// `(assets, max_mtime_unix_secs, version_string)` or `None` if no carrier
/// has a local build.
fn local_firmware_snapshot() -> Option<(Vec<FirmwareAsset>, i64, String)> {
    let mut assets = Vec::new();
    let mut latest_version = String::new();
    let mut max_mtime_secs: i64 = i64::MIN;

    for (soc, carrier) in LOCAL_CARRIER_DIRS {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("firmware").join(soc).join(carrier).join("releases"));
        let Some(dir) = dir else { continue };
        if !dir.is_dir() { continue; }

        let mut best: Option<(std::time::SystemTime, std::path::PathBuf, u64)> = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("bin") { continue; }
                let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                let size = meta.len();
                let pick = match &best {
                    Some((t, _, _)) => mtime > *t,
                    None => true,
                };
                if pick { best = Some((mtime, path, size)); }
            }
        }
        let Some((mtime, path, size)) = best else { continue };

        // Sidecar is authoritative when present. Read it synchronously —
        // it sits next to the .bin on local disk, so no network cost.
        let sidecar = path.with_extension("bin.meta.json");
        let meta = std::fs::read_to_string(&sidecar).ok()
            .and_then(|s| parse_meta_json(&s));

        let (class, carrier_out, version, sha256) = match meta {
            Some(m) => {
                // Skip foreign-class local archives outright.
                if m.class != ENSEMBLE_CLASS { continue; }
                if m.carrier != *carrier {
                    eprintln!(
                        "[firmware] WARN: sidecar {:?} declares carrier '{}' but lives in the '{}' dir — trusting sidecar",
                        sidecar, m.carrier, carrier
                    );
                }
                (m.class, m.carrier, m.version, m.sha256)
            }
            None => {
                // No sidecar (pre-v0.3 archive). Carrier = directory;
                // class = ours; version best-effort from the filename.
                let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let version = fname
                    .strip_prefix("r2-workshop-firmware-")
                    .and_then(|s| s.strip_suffix(".bin"))
                    .unwrap_or(fname)
                    .to_string();
                (ENSEMBLE_CLASS.to_string(), carrier.to_string(), version, None)
            }
        };

        if version > latest_version { latest_version = version.clone(); }
        let mtime_secs = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if mtime_secs > max_mtime_secs { max_mtime_secs = mtime_secs; }
        assets.push(FirmwareAsset {
            class,
            carrier: carrier_out,
            version,
            url: path.to_string_lossy().into_owned(),
            sha256,
            size: Some(size),
        });
    }

    if assets.is_empty() { None } else { Some((assets, max_mtime_secs, latest_version)) }
}

/// Query GitHub Releases. Returns `(tag, published_at_unix_secs,
/// assets)` or `None` if the request failed / the latest release has
/// no `.bin` assets matching this dashboard's (class, carrier).
///
/// #90: matching is driven by the canonical release filename
/// (`r2-workshop-firmware-<class-slug>-<carrier>-<version>+<git>.bin`,
/// anchored on *our* class-slug so foreign-class assets fall away) and
/// then the `.bin.meta.json` sidecar, which is authoritative for
/// (class, carrier, version, sha256). Replaces the old hardcoded
/// `name.contains("devkitc"|"xiao"|"dfr1117")` substring matcher.
async fn github_firmware_snapshot() -> Option<(String, i64, Vec<FirmwareAsset>)> {
    // Walk the releases LIST (newest first), not just `/releases/latest`.
    // A later release that ships no firmware — e.g. a server-bundle-only
    // release (`r2-workshop-server-…`, SPEC §13.5) — must NOT hide the
    // most recent firmware. Return the first (newest) release that yields
    // ≥1 matching firmware asset.
    let gh_url = format!(
        "https://api.github.com/repos/{}/releases?per_page=20",
        GITHUB_OWNER_REPO,
    );
    let body = fetch_url_text(&gh_url, 5).await?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let releases = json.as_array()?;
    let slug = class_slug();

    for rel in releases {
        if rel.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(tag) = rel.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let published_secs = rel
            .get("published_at")
            .and_then(|v| v.as_str())
            .and_then(iso_to_unix_secs)
            .unwrap_or(0);

        let assets = github_firmware_assets_for_release(rel, &slug).await;
        if !assets.is_empty() {
            return Some((tag.to_string(), published_secs, assets));
        }
    }
    None
}

/// Extract a single release's firmware assets matching our (class, carrier)
/// per §13.3 — sidecar-authoritative, foreign-class filtered. Returns an
/// empty Vec when the release carries no matching firmware (e.g. a
/// server-bundle-only release), so the caller skips on to the next release.
async fn github_firmware_assets_for_release(
    rel: &serde_json::Value,
    slug: &str,
) -> Vec<FirmwareAsset> {
    let Some(arr) = rel.get("assets").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    // First pass: index every sidecar asset by its filename so each
    // surviving .bin can find its `<name>.meta.json` partner in O(1).
    let mut sidecar_urls: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for a in arr {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.ends_with(".bin.meta.json") {
            if let Some(url) = a.get("browser_download_url").and_then(|v| v.as_str()) {
                sidecar_urls.insert(name, url);
            }
        }
    }

    // Second pass: each .bin whose filename matches our class-slug + the
    // canonical convention. Foreign-class binaries don't match the prefix,
    // so they're filtered out exactly as §13.3 requires.
    let mut assets = Vec::new();
    for a in arr {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = a.get("browser_download_url").and_then(|v| v.as_str()).unwrap_or("");
        let size = a.get("size").and_then(|v| v.as_u64());
        if !name.ends_with(".bin") { continue; }

        let Some((_, fn_carrier, fn_version)) = parse_release_filename(name, slug) else {
            continue; // non-canonical or foreign-class → not auto-surfaced
        };

        // Sidecar is authoritative. Fetch `<name>.meta.json` if the release
        // shipped one; fall back to the filename parse for pre-v0.3 tags.
        let sidecar_name = format!("{}.meta.json", name);
        let meta = if let Some(murl) = sidecar_urls.get(sidecar_name.as_str()) {
            fetch_url_text(murl, 5).await.and_then(|b| parse_meta_json(&b))
        } else {
            None
        };

        let (class, carrier, version, sha256) = match meta {
            Some(m) => {
                if m.class != ENSEMBLE_CLASS {
                    // A sidecar that disagrees with its own filename's
                    // class-slug. Trust the JSON and drop it — it isn't ours.
                    eprintln!(
                        "[firmware] WARN: GitHub asset {} sidecar declares class '{}' ≠ ours '{}' — skipping",
                        name, m.class, ENSEMBLE_CLASS
                    );
                    continue;
                }
                if m.carrier != fn_carrier {
                    eprintln!(
                        "[firmware] WARN: GitHub asset {} filename carrier '{}' ≠ sidecar '{}' — trusting sidecar",
                        name, fn_carrier, m.carrier
                    );
                }
                (m.class, m.carrier, m.version, m.sha256)
            }
            None => (ENSEMBLE_CLASS.to_string(), fn_carrier, fn_version, None),
        };

        assets.push(FirmwareAsset {
            class,
            carrier,
            version,
            url: url.to_string(),
            sha256,
            size,
        });
    }
    assets
}

/// `curl -sS` a URL and return the body as a String, or `None` on any
/// transport/HTTP failure. Shared by the release-list query and the
/// per-asset sidecar fetches.
async fn fetch_url_text(url: &str, max_time_secs: u32) -> Option<String> {
    let output = tokio::process::Command::new("curl")
        .args([
            "-sSL",
            "--max-time", &max_time_secs.to_string(),
            "-H", "Accept: application/vnd.github+json",
            "-H", "User-Agent: r2-workshop-dashboard",
            url,
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() { return None; }
    String::from_utf8(output.stdout).ok()
}

/// Tiny ISO-8601 ("2026-05-18T07:36:42Z") → unix seconds parser.
/// We pull this in rather than adding chrono just for one date
/// field. Returns `None` on malformed input.
fn iso_to_unix_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 { return None; }
    let y  = std::str::from_utf8(&b[0..4]).ok()?.parse::<i32>().ok()?;
    let mo = std::str::from_utf8(&b[5..7]).ok()?.parse::<u32>().ok()?;
    let d  = std::str::from_utf8(&b[8..10]).ok()?.parse::<u32>().ok()?;
    let h  = std::str::from_utf8(&b[11..13]).ok()?.parse::<u32>().ok()?;
    let mi = std::str::from_utf8(&b[14..16]).ok()?.parse::<u32>().ok()?;
    let se = std::str::from_utf8(&b[17..19]).ok()?.parse::<u32>().ok()?;
    if mo < 1 || mo > 12 { return None; }
    // Howard Hinnant's days-from-civil algorithm.
    let y_adj = y - if mo <= 2 { 1 } else { 0 };
    let era = if y_adj >= 0 { y_adj / 400 } else { (y_adj - 399) / 400 };
    let yoe = (y_adj - era * 400) as u32;
    let m_num = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * m_num + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era as i64) * 146097 + doe as i64 - 719468;
    Some(days * 86400 + (h as i64) * 3600 + (mi as i64) * 60 + se as i64)
}

#[cfg(test)]
mod firmware_snapshot_tests {
    use super::{iso_to_unix_secs, parse_release_filename, parse_meta_json};
    #[test]
    fn iso_epoch() { assert_eq!(iso_to_unix_secs("1970-01-01T00:00:00Z"), Some(0)); }
    #[test]
    fn iso_known() {
        // 2026-05-18T07:36:42Z = 1779089802 (verified via `date -u -d`).
        // The previous constant (1779435402) was 2026-05-22 — a stale
        // off-by-4-days value that failed the suite.
        assert_eq!(iso_to_unix_secs("2026-05-18T07:36:42Z"), Some(1779089802));
    }

    const SLUG: &str = "nz-ac-auckland-rocker";

    #[test]
    fn filename_canonical_release() {
        // The exact shape of the v0.3.0 GitHub asset names.
        let got = parse_release_filename(
            "r2-workshop-firmware-nz-ac-auckland-rocker-devkitc-v0.3.0.bin", SLUG);
        assert_eq!(got, Some((SLUG.to_string(), "devkitc".to_string(), "v0.3.0".to_string())));
    }

    #[test]
    fn filename_strips_git_build_metadata() {
        // Dev-build name: <version>+<git>; the "+sha" suffix is dropped.
        let got = parse_release_filename(
            "r2-workshop-firmware-nz-ac-auckland-rocker-dfr1117-0.3.0+abc1234.bin", SLUG);
        assert_eq!(got, Some((SLUG.to_string(), "dfr1117".to_string(), "0.3.0".to_string())));
    }

    #[test]
    fn filename_class_anchor_rejects_foreign_class() {
        // A different class-slug must NOT match our dashboard's slug —
        // this is the (class)-filter half of the §13.3 match rule.
        let got = parse_release_filename(
            "r2-workshop-firmware-com-example-other-devkitc-v0.3.0.bin", SLUG);
        assert_eq!(got, None);
    }

    #[test]
    fn filename_rejects_non_canonical() {
        // Pre-v0.3 local archive name (no class/carrier segment) and a
        // plain non-matching string both fail the canonical parse.
        assert_eq!(parse_release_filename("r2-workshop-firmware-0.3.0.bin", SLUG), None);
        assert_eq!(parse_release_filename("not-our-asset.bin", SLUG), None);
        assert_eq!(parse_release_filename(
            "r2-workshop-firmware-nz-ac-auckland-rocker-devkitc-v0.3.0.bin.meta.json", SLUG), None);
    }

    #[test]
    fn filename_carrier_may_contain_hyphen_suffix_version() {
        // carrier is the first token after the slug; everything after the
        // next '-' is the version (which itself carries dots, not hyphens).
        let got = parse_release_filename(
            "r2-workshop-firmware-nz-ac-auckland-rocker-xiao-v1.2.3.bin", SLUG);
        assert_eq!(got, Some((SLUG.to_string(), "xiao".to_string(), "v1.2.3".to_string())));
    }

    #[test]
    fn meta_full_tuple() {
        let m = parse_meta_json(r#"{
            "class": "nz.ac.auckland.rocker", "carrier": "xiao",
            "version": "0.3.0", "git": "a1b2c3d",
            "sha256": "deadbeef", "built": "2026-05-28T07:00:00Z"
        }"#).expect("parses");
        assert_eq!(m.class, "nz.ac.auckland.rocker");
        assert_eq!(m.carrier, "xiao");
        assert_eq!(m.version, "0.3.0");
        assert_eq!(m.sha256.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn meta_tolerates_missing_sha256() {
        // Pre-v0.3 sidecars (if any) may omit sha256 — parse must survive.
        let m = parse_meta_json(
            r#"{"class":"nz.ac.auckland.rocker","carrier":"devkitc","version":"0.2.9"}"#)
            .expect("parses");
        assert_eq!(m.sha256, None);
        assert_eq!(m.carrier, "devkitc");
    }

    #[test]
    fn meta_rejects_incomplete() {
        // Missing a required key → None (caller falls back to filename parse).
        assert!(parse_meta_json(r#"{"class":"x","carrier":"y"}"#).is_none());
        assert!(parse_meta_json("not json").is_none());
    }
}

// ── SPEC-R2-WORKSHOP-CAPTURE handlers ───────────────────────────────────

/// Fan a frame out to every connected peer's tx channel. Returns the
/// count of peers reached. Failures (channel full / closed) are
/// logged but do not abort the fan-out — fleet ops are best-effort.
async fn fan_out_dash_frame(
    state: &AppState,
    event_hash: u32,
    msg_id: u16,
    payload: Vec<u8>,
) -> usize {
    let frame = build_dash_frame(event_hash, msg_id, &payload);
    let peers = state.peers.read().await;
    let mut sent = 0;
    for (addr, peer) in peers.iter() {
        match peer.tx.send(frame.clone()).await {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[capture] fan-out to {} failed: {}", addr, e),
        }
    }
    sent
}

// ── Capture core logic (shared by HTTP + /r2 operator events) ────
//
// Per SPEC-R2-WORKSHOP-WIRE §2.1, the legacy POST /api/capture/* routes
// and the new `r2.dash.cmd.capture.*` events on /r2 produce
// identical side-effects. Extracting the core into `do_capture_*`
// keeps both call sites in lockstep and makes the migration a pure
// wire-shape swap.

async fn do_capture_start(state: &Arc<AppState>) -> usize {
    // Fire an immediate sync_pulse round to every peer so capture
    // timestamps in the upcoming session share a tightly-refreshed
    // baseline. See SPEC-R2-WORKSHOP-CAPTURE §7.1.
    let dash_ts_ms = dash_wall_ms();
    {
        let peers = state.peers.read().await;
        for (_addr, peer) in peers.iter() {
            let req_id = (dash_ts_ms & 0xFFFF_FFFF) as u32;
            let payload = encode_sync_pulse(req_id, dash_ts_ms);
            let frame = build_dash_frame(
                DASH_SYNC_PULSE,
                (req_id & 0xFFFF) as u16,
                &payload,
            );
            let _ = peer.tx.send(frame).await;
        }
    }

    let payload = encode_empty_map();
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_START, 0x0001, payload).await;
    emit_capture_progress(state, "start", sent, None, None, None);
    sent
}

async fn do_capture_mark(
    state: &Arc<AppState>,
    name: &str,
    prefix: Option<&str>,
) -> Result<(usize, i64), String> {
    if !is_valid_capture_name(name) {
        return Err("invalid name (use [A-Za-z0-9_-]{1,32})".to_string());
    }
    if let Some(p) = prefix {
        if !is_valid_capture_prefix(p) {
            return Err("invalid prefix (use [0-9_-]{1,32})".to_string());
        }
    }
    let ts_ms = dash_wall_ms() as i64;
    let payload = encode_capture_mark(ts_ms, name, prefix);
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_MARK, 0x0002, payload).await;
    emit_capture_progress(state, "mark", sent, Some(name), prefix, Some(ts_ms));
    Ok((sent, ts_ms))
}

async fn do_capture_stop(state: &Arc<AppState>) -> usize {
    let payload = encode_empty_map();
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_STOP, 0x0003, payload).await;
    emit_capture_progress(state, "stop", sent, None, None, None);
    sent
}

/// SPEC-R2-WORKSHOP-CAPTURE §7.5: controller-stamped event-mark
/// fan-out. Returns `(peers_sent, ts_ms, mark_id)`. `peers_sent` is
/// the count of currently-Recording peers we reached; if zero, the
/// caller responds with "no active capture".
async fn do_capture_event_mark(
    state: &Arc<AppState>,
    label: &str,
) -> Result<(usize, i64, u32), String> {
    let ts_ms = dash_wall_ms() as i64;
    let mark_id = state.captures.next_mark_id();

    // Identify the active session_stem by reading any one peer's
    // last_capture_decoded — every Recording peer is on the same
    // session (controller fans out one filename per Mark). Snapshot
    // under the read lock; release before encoding/sending.
    let (session_stem, recording_peers) = {
        let peers = state.peers.read().await;
        let mut stem: Option<String> = None;
        let mut count = 0usize;
        for (_, p) in peers.iter() {
            if let Some(s) = &p.last_capture_decoded {
                if s.state == 2 {
                    count += 1;
                    if stem.is_none() {
                        // Strip the .csv suffix to match the wire
                        // payload's session_stem convention (CAPTURE
                        // §7.5 row 46 key 4).
                        if let Some(fname) = &s.filename {
                            stem = Some(fname.strip_suffix(".csv").unwrap_or(fname).to_string());
                        }
                    }
                }
            }
        }
        (stem, count)
    };

    if recording_peers == 0 {
        return Ok((0, ts_ms, mark_id));
    }

    // Fan out the per-sensor event_mark frame.
    let payload = encode_capture_event_mark(ts_ms as u64, label, mark_id);
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_EVENT_MARK, (mark_id & 0xFFFF) as u16, payload).await;

    // Status broadcast on /r2 so every viewer renders the marker
    // immediately, regardless of whether the sidecar has synced.
    broadcast_event_marked(state, ts_ms as u64, label, mark_id, session_stem.as_deref().unwrap_or("")).await;
    Ok((sent, ts_ms, mark_id))
}

/// Encode `r2.dash.capture.event_mark` payload (SPEC-R2-WORKSHOP-WIRE
/// row 45): `{1: u64 ts_ms, 2: str label, 3: u32 mark_id}`.
fn encode_capture_event_mark(ts_ms: u64, label: &str, mark_id: u32) -> Vec<u8> {
    let mut buf = vec![0u8; 32 + label.len()];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(3);
        let _ = enc.kv(1, &r2_cbor::Value::UInt(ts_ms));
        let _ = enc.kv(2, &r2_cbor::Value::Text(label));
        let _ = enc.kv(3, &r2_cbor::Value::UInt(mark_id as u64));
        enc.len()
    };
    buf[..used].to_vec()
}

/// Emit `r2.dash.capture.event_marked` (WIRE row 46) on `/r2`.
async fn broadcast_event_marked(
    state: &Arc<AppState>,
    ts_ms: u64,
    label: &str,
    mark_id: u32,
    session_stem: &str,
) {
    let mut buf = vec![0u8; 64 + label.len() + session_stem.len()];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let _ = enc.map(4);
    let _ = enc.kv(1, &r2_cbor::Value::UInt(ts_ms));
    let _ = enc.kv(2, &r2_cbor::Value::Text(label));
    let _ = enc.kv(3, &r2_cbor::Value::UInt(mark_id as u64));
    let _ = enc.kv(4, &r2_cbor::Value::Text(session_stem));
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CAPTURE_EVENT_MARKED, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

fn is_valid_capture_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.bytes().all(|b| matches!(
        b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'
    ))
}

fn is_valid_capture_prefix(p: &str) -> bool {
    !p.is_empty() && p.len() <= 32 && p.bytes().all(|b| matches!(
        b, b'0'..=b'9' | b'_' | b'-'
    ))
}

// ── data_tcp client (port 21047) ──────────────────────────────────────

const DATA_PORT: u16 = 21047;
const ST_OK: u8 = 0x00;
const ST_ERROR: u8 = 0x01;
const ST_BUSY: u8 = 0x02;

/// Open a fresh TCP connection to <ip>:21047 on the named peer.
/// Strips any trailing port suffix from `addr` (the webapp keys by IP
/// alone but tolerates `ip:port`).
async fn dial_data_tcp(addr: &str) -> std::io::Result<TcpStream> {
    let ip_only: &str = addr.split(':').next().unwrap_or(addr);
    let target = format!("{}:{}", ip_only, DATA_PORT);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(&target),
    ).await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "data_tcp connect timeout"))?
}

/// `GET /api/data/{addr}/list` — proxy a LIST opcode to the sensor.
async fn data_list_handler(Path(addr): Path<String>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("connect: {}", e)})),
        ).into_response(),
    };
    if let Err(e) = s.write_all(&[0x01u8]).await {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("write LIST: {}", e)})),
        ).into_response();
    }
    let mut status = [0u8; 1];
    if let Err(e) = s.read_exact(&mut status).await {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("read status: {}", e)})),
        ).into_response();
    }
    if status[0] != ST_OK {
        let err_msg = read_err_msg(&mut s).await.unwrap_or_default();
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": err_msg, "status_byte": status[0]})),
        ).into_response();
    }
    let mut count_buf = [0u8; 4];
    if let Err(e) = s.read_exact(&mut count_buf).await {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("read count: {}", e)})),
        ).into_response();
    }
    let count = u32::from_be_bytes(count_buf) as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let mut nl = [0u8; 2];
        if s.read_exact(&mut nl).await.is_err() { break; }
        let nlen = u16::from_be_bytes(nl) as usize;
        let mut name_buf = vec![0u8; nlen];
        if s.read_exact(&mut name_buf).await.is_err() { break; }
        let mut size_buf = [0u8; 8];
        if s.read_exact(&mut size_buf).await.is_err() { break; }
        let mut mtime_buf = [0u8; 8];
        if s.read_exact(&mut mtime_buf).await.is_err() { break; }
        let name = String::from_utf8_lossy(&name_buf).into_owned();
        let size = u64::from_be_bytes(size_buf);
        let mtime = i64::from_be_bytes(mtime_buf);
        entries.push(serde_json::json!({
            "name": name, "size": size, "mtime_ms": mtime,
        }));
    }
    (axum::http::StatusCode::OK, Json(serde_json::json!({"files": entries}))).into_response()
}

/// `GET /api/data/{addr}/file/{name}` — proxy a GET opcode and stream
/// the file bytes back to the client. Splices in a CSV header AND
/// stamps the device's display-name into both the filename and the
/// x/y/z column titles so multi-sensor captures stay distinguishable
/// after they leave the dashboard (e.g. when the operator opens half
/// a dozen of them in pandas and tries to remember which was which).
async fn data_get_handler(
    State(state): State<Arc<AppState>>,
    Path((addr, name)): Path<(String, String)>,
) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x02);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    if s.write_all(&req).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "write GET".to_string()).into_response();
    }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "read status".to_string()).into_response();
    }
    if status[0] != ST_OK {
        let err_msg = read_err_msg(&mut s).await.unwrap_or_default();
        let code = match status[0] {
            ST_BUSY => axum::http::StatusCode::CONFLICT,
            _ => axum::http::StatusCode::NOT_FOUND,
        };
        return (code, err_msg).into_response();
    }
    let mut size_buf = [0u8; 8];
    if s.read_exact(&mut size_buf).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "read size".to_string()).into_response();
    }
    let size = u64::from_be_bytes(size_buf) as usize;
    let mut body = vec![0u8; size];
    if s.read_exact(&mut body).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "read body".to_string()).into_response();
    }

    // Resolve the device's display-name the same way data_merged_handler
    // does: operator-assigned alias keyed by device_pk first; fall back
    // to the IP with dots-to-underscores so column names + filenames
    // stay shell-safe.
    let ip_only: &str = addr.split(':').next().unwrap_or(&addr);
    let device_pk_opt = {
        let peers = state.peers.read().await;
        peers.iter()
            .find(|(sa, _)| sa.ip().to_string() == ip_only)
            .and_then(|(_, p)| p.device_pk.clone())
    };
    let alias_opt = if let Some(pk) = device_pk_opt.as_ref() {
        let g = state.device_aliases.lock().await;
        g.get(pk).cloned()
    } else {
        None
    };
    let raw_name = alias_opt.unwrap_or_else(|| ip_only.replace('.', "_"));
    // Filesystem- and CSV-header-safe: keep alphanumeric / - / _,
    // collapse everything else (spaces from aliases, punctuation, etc)
    // to '_'. Same charset on both sides so columns and filename agree.
    let device_safe: String = raw_name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();

    // CSV header — splice device name into the x/y/z column titles
    // so concatenating multiple files in pandas keeps the per-sensor
    // disambiguation that the merged-CSV already provides.
    let header_line = format!("seq,ts_ms,{0}_x,{0}_y,{0}_z\n", device_safe);
    let mut out = Vec::with_capacity(header_line.len() + body.len());
    out.extend_from_slice(header_line.as_bytes());
    out.extend_from_slice(&body);

    // Download filename: append __<device> before the .csv extension.
    // Double-underscore as delimiter so the original name's prefix-name
    // hyphens / underscores don't get confused with the new suffix.
    let download_name = {
        let stem = name.strip_suffix(".csv").unwrap_or(&name);
        format!("{}__{}.csv", stem, device_safe)
    };

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", download_name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, out).into_response()
}

/// `DELETE /api/data/{addr}/file/{name}` — proxy a DEL opcode.
async fn data_delete_handler(Path((addr, name)): Path<(String, String)>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    };
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x03);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    if s.write_all(&req).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "write"}))).into_response();
    }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "read status"}))).into_response();
    }
    if status[0] == ST_OK {
        return (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response();
    }
    let msg = read_err_msg(&mut s).await.unwrap_or_default();
    let code = if status[0] == ST_BUSY { axum::http::StatusCode::CONFLICT } else { axum::http::StatusCode::BAD_GATEWAY };
    (code, Json(serde_json::json!({"ok": false, "error": msg, "status_byte": status[0]}))).into_response()
}

/// `DELETE /api/data/{addr}/all` — proxy a DEL_ALL opcode.
async fn data_delete_all_handler(Path(addr): Path<String>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    };
    if s.write_all(&[0x04u8]).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "write"}))).into_response();
    }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "read status"}))).into_response();
    }
    if status[0] != ST_OK {
        let msg = read_err_msg(&mut s).await.unwrap_or_default();
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": msg}))).into_response();
    }
    let mut count_buf = [0u8; 4];
    let _ = s.read_exact(&mut count_buf).await;
    let count = u32::from_be_bytes(count_buf);
    (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "deleted": count}))).into_response()
}

/// `GET /api/data/merged?file=<basename>[&bin_ms=N]` — fetch the named
/// file from every connected peer and emit a wide-format CSV.
///
/// Without `bin_ms`: one row per unique `ts_ms` across the fleet, three
/// columns per sensor (`<ip>_x, <ip>_y, <ip>_z`). Cells are blank when
/// that sensor has no sample at that ts_ms — handy when sample
/// timestamps don't line up across the fleet (clock-sync jitter,
/// dropped samples). This is the raw merge.
///
/// With `bin_ms=N` (10 / 100 / 1000 / …): per-sensor samples are
/// bucketed into N-ms windows (`ts_ms = bucket_start_ms`), each
/// bucket's x/y/z averaged, then merged across sensors. Result: one
/// row per bucket per sensor — with N chosen above the sample period
/// (samples land at ~10 ms), every bucket has an entry for every
/// sensor and the timestamps line up across columns.
/// Tiny RFC-4180 unescape — used by the merged-CSV mark column to
/// recover the operator's original label from the sidecar file's
/// quoted-and-doubled form. Conservative: anything that isn't a
/// `"…"`-wrapped field passes through unchanged.
fn unquote_csv(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].replace("\"\"", "\"")
    } else {
        s.to_string()
    }
}

async fn data_merged_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(name) = q.get("file").cloned() else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing ?file= parameter"})),
        ).into_response();
    };
    let bin_ms: Option<i64> = q.get("bin_ms")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n > 0 && n <= 60_000);

    // v0.2 (SPEC-R2-WORKSHOP-CAPTURE §7.4): source from the
    // controller-local store rather than dialling every connected
    // peer's data_tcp. Works even when sensors are offline + naturally
    // includes the event-mark sidecars (`<stem>.marks.csv`) which the
    // per-sensor live-fetch path couldn't see.
    let stem = name.strip_suffix(".csv").unwrap_or(&name);
    let sessions = state.captures.list_sessions().await;
    let Some(session) = sessions.into_iter().find(|s| s.session_stem == stem) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("session-stem {:?} not in controller-local store", stem)})),
        ).into_response();
    };

    // Per-device data files for this session, in sorted-alias order
    // (stable header). Sidecars are pulled out separately so the
    // mark column is filled in.
    let mut data_entries: Vec<captures::CaptureEntry> = session.files.iter()
        .filter(|e| e.kind == captures::CaptureKind::Data)
        .cloned()
        .collect();
    data_entries.sort_by(|a, b| a.device_safe.cmp(&b.device_safe));
    let marks_entries: Vec<&captures::CaptureEntry> = session.files.iter()
        .filter(|e| e.kind == captures::CaptureKind::Marks)
        .collect();

    if data_entries.is_empty() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no data files synced for this session yet"})),
        ).into_response();
    }

    // Load each per-device file off disk. For the merged path the
    // CSV header (line 1 from the controller-local store) is skipped:
    // we re-emit our own header below. Map (device_safe → raw bytes
    // post-header), preserving sorted order for column stability.
    let mut fetched: Vec<(String, Vec<u8>)> = Vec::with_capacity(data_entries.len());
    for e in &data_entries {
        match std::fs::read(&e.controller_path) {
            Ok(bytes) => {
                // Strip the first line (the spliced CSV header — see
                // CapturesStore::write_data — `seq,ts_ms,<dev>_x,…\n`).
                // The merged path doesn't want the per-file header
                // in-band; it'd corrupt the row-parser below.
                let body = match bytes.iter().position(|&b| b == b'\n') {
                    Some(nl) => bytes[nl + 1..].to_vec(),
                    None     => bytes,
                };
                fetched.push((e.device_safe.clone(), body));
            }
            Err(err) => {
                eprintln!("[merge] read {:?}: {}", e.controller_path, err);
            }
        }
    }
    if fetched.is_empty() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "captures dir read failed for every device"})),
        ).into_response();
    }

    // Parse marks sidecars (SPEC-R2-WORKSHOP-CAPTURE §4.1):
    //   # r2-workshop event marks v1
    //   ts_ms,mark_id,label
    //   <rows>
    // De-dup across devices on (ts_ms, mark_id) — same event lands
    // in every sensor's sidecar so they should agree.
    let mut marks: std::collections::BTreeMap<i64, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut seen_mark_ids: std::collections::HashSet<(i64, u32)> =
        std::collections::HashSet::new();
    for e in marks_entries {
        let Ok(text) = std::fs::read_to_string(&e.controller_path) else { continue; };
        for (i, line) in text.lines().enumerate() {
            // Skip the "# r2-workshop event marks v1" header + the
            // CSV column line. Either-or — operator could rebuild the
            // file by hand; just be permissive.
            if i < 2 && (line.starts_with('#') || line.starts_with("ts_ms")) { continue; }
            if line.is_empty() { continue; }
            // Minimal RFC-4180 split: label may be quoted with
            // doubled-quote escapes.
            let mut parts = line.splitn(3, ',');
            let Some(ts_s) = parts.next() else { continue; };
            let Some(mid_s) = parts.next() else { continue; };
            let label_raw = parts.next().unwrap_or("");
            let Ok(ts) = ts_s.trim().parse::<i64>() else { continue; };
            let Ok(mid) = mid_s.trim().parse::<u32>() else { continue; };
            if !seen_mark_ids.insert((ts, mid)) { continue; }
            let label = unquote_csv(label_raw);
            marks.entry(ts).or_default().push(label);
        }
    }

    // Fixed-width capture row (SPEC-R2-WORKSHOP-CAPTURE §4 +
    // SPEC-R2-WORKSHOP-SENSOR §6.2):
    //   bytes  0..10  : seq (right-aligned)
    //   bytes 11..25  : ts_ms (right-aligned)
    //   bytes 26..37  : x   (right-aligned)
    //   bytes 38..49  : y
    //   bytes 50..61  : z
    //   byte  61      : '\n' (counted into ROW_BYTES below — last byte)
    const ROW_BYTES: usize = 62;

    // Build a sorted ts_ms → [per-sensor (x,y,z) Option] map. BTreeMap
    // gives us ascending iteration for free. Each sensor contributes
    // its samples; if two sensors share a ts_ms, both fill the same
    // row; if their timestamps diverge by even 1 ms, separate rows.
    //
    // The expected scale here is small: 10 Hz × minutes × ~2 sensors
    // = a few thousand rows. BTreeMap is fine.
    type Triplet = (String, String, String);
    let n_peers = fetched.len();
    let mut by_ts: std::collections::BTreeMap<i64, Vec<Option<Triplet>>> =
        std::collections::BTreeMap::new();

    // Accumulators for the bin_ms aggregation path. One per (peer, bucket):
    //   (sum_x, sum_y, sum_z, count)
    // ; we render mean at emit time.
    let mut buckets: std::collections::BTreeMap<i64, Vec<Option<(f64, f64, f64, u32)>>> =
        std::collections::BTreeMap::new();

    for (peer_idx, (_, bytes)) in fetched.iter().enumerate() {
        for row in bytes.chunks_exact(ROW_BYTES) {
            let Some(ts) = parse_row_ts_ms(row) else { continue; };
            // Column layout offsets — adjacent fields are separated by
            // single commas, captured by the +1 shifts below.
            let x_str = trim_str(&row[26..37]);
            let y_str = trim_str(&row[38..49]);
            let z_str = trim_str(&row[50..61]);

            if let Some(bin) = bin_ms {
                // Bucket start = floor(ts / bin) * bin. Per-sensor running
                // sum + count so we emit mean(x/y/z) per bucket.
                let bucket = (ts / bin) * bin;
                let slot = buckets.entry(bucket)
                    .or_insert_with(|| vec![None; n_peers]);
                let x = x_str.parse::<f64>().unwrap_or(f64::NAN);
                let y = y_str.parse::<f64>().unwrap_or(f64::NAN);
                let z = z_str.parse::<f64>().unwrap_or(f64::NAN);
                if x.is_nan() || y.is_nan() || z.is_nan() { continue; }
                let entry = slot[peer_idx].get_or_insert((0.0, 0.0, 0.0, 0));
                entry.0 += x; entry.1 += y; entry.2 += z; entry.3 += 1;
            } else {
                by_ts.entry(ts)
                    .or_insert_with(|| vec![None; n_peers])
                    [peer_idx] = Some((x_str, y_str, z_str));
            }
        }
    }

    // Attach each mark to the row with the nearest ts_ms. No
    // synthetic rows: a mark whose nearest sample is many ms away
    // still rides on that row (analyst sees the marker; the mark's
    // own ts_ms is preserved in the sidecar file if they need the
    // exact stamp). Same rule for raw and binned modes — the row
    // timestamps differ (sample ts vs bucket ts) but the
    // closest-neighbour lookup is identical.
    //
    // Implementation: build the sorted row-key set (sample ts in
    // raw mode, bucket ts in bin mode), then for each mark use
    // BTreeMap::range to find the immediate neighbours and pick
    // the closer one.
    let row_keys: std::collections::BTreeSet<i64> = if bin_ms.is_some() {
        buckets.keys().copied().collect()
    } else {
        by_ts.keys().copied().collect()
    };
    let mut marks_by_key: std::collections::BTreeMap<i64, Vec<String>> =
        std::collections::BTreeMap::new();
    for (&mark_ts, labels) in &marks {
        // Find the row ts closest to this mark.
        let mut below = row_keys.range(..=mark_ts).next_back().copied();
        let mut above = row_keys.range(mark_ts..).next().copied();
        // Edge case: mark_ts itself might be a row ts; the .. ranges
        // above already handle that (below = mark_ts, above = mark_ts).
        let closest = match (below.take(), above.take()) {
            (Some(b), Some(a)) => {
                if (mark_ts - b).abs() <= (a - mark_ts).abs() { Some(b) } else { Some(a) }
            }
            (Some(b), None) => Some(b),
            (None, Some(a)) => Some(a),
            (None, None)    => None, // no rows at all — mark is dropped
        };
        if let Some(key) = closest {
            marks_by_key.entry(key).or_default().extend(labels.iter().cloned());
        }
    }
    let mark_for_key = |ts: i64| -> Option<String> {
        marks_by_key.get(&ts).map(|labels| {
            let joined = labels.join("; ");
            if joined.contains(',') || joined.contains('"') || joined.contains('\n') {
                format!("\"{}\"", joined.replace('"', "\"\""))
            } else {
                joined
            }
        })
    };
    // No more mark-injection: every emitted row is a real sample
    // row, mark column simply blank when no mark snapped to it.
    let mark_keys: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();

    let mut output = String::with_capacity(64 * 1024);
    // Header: ts_ms then three columns per sensor in sorted-alias
    // order, then a single "mark" column at the end. Mark is one
    // text column so analysts can grep/filter without flattening
    // multi-mark rows.
    output.push_str("ts_ms");
    for (dev_safe, _) in &fetched {
        output.push(','); output.push_str(dev_safe); output.push_str("_x");
        output.push(','); output.push_str(dev_safe); output.push_str("_y");
        output.push(','); output.push_str(dev_safe); output.push_str("_z");
    }
    output.push_str(",mark\n");

    if bin_ms.is_some() {
        // Union of buckets that have samples AND buckets that hold a mark.
        let mut all_keys: std::collections::BTreeSet<i64> = buckets.keys().copied().collect();
        all_keys.extend(mark_keys.iter().copied());
        for ts in &all_keys {
            output.push_str(&ts.to_string());
            let empty_slots: Vec<Option<(f64, f64, f64, u32)>> = vec![None; fetched.len()];
            let slots = buckets.get(ts).unwrap_or(&empty_slots);
            for slot in slots {
                output.push(',');
                if let Some((sx, sy, sz, n)) = slot {
                    let nf = *n as f64;
                    let _ = std::fmt::Write::write_fmt(&mut output,
                        format_args!("{:.6}", sx / nf));
                    output.push(',');
                    let _ = std::fmt::Write::write_fmt(&mut output,
                        format_args!("{:.6}", sy / nf));
                    output.push(',');
                    let _ = std::fmt::Write::write_fmt(&mut output,
                        format_args!("{:.6}", sz / nf));
                } else {
                    output.push(','); output.push(',');
                }
            }
            output.push(',');
            if let Some(m) = mark_for_key(*ts) { output.push_str(&m); }
            output.push('\n');
        }
    } else {
        // Union of sample-rows and mark-rows.
        let mut all_keys: std::collections::BTreeSet<i64> = by_ts.keys().copied().collect();
        all_keys.extend(mark_keys.iter().copied());
        for ts in &all_keys {
            output.push_str(&ts.to_string());
            let empty_slots: Vec<Option<Triplet>> = vec![None; fetched.len()];
            let slots = by_ts.get(ts).unwrap_or(&empty_slots);
            for slot in slots {
                output.push(',');
                if let Some((x, y, z)) = slot {
                    output.push_str(x); output.push(',');
                    output.push_str(y); output.push(',');
                    output.push_str(z);
                } else {
                    output.push(','); output.push(',');
                }
            }
            output.push(',');
            if let Some(m) = mark_for_key(*ts) { output.push_str(&m); }
            output.push('\n');
        }
    }

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"merged-{}\"", name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, output).into_response()
}

/// `GET /api/data/zip` — end-of-day bundle: every file on every
/// connected sensor's SD ring, flat in a zip with the same
/// `<stem>__<dev>.csv` filenames + device-stamped CSV headers the
/// single-file path produces. Use-case: at the end of an experiment
/// run, the operator grabs one zip, copies it to a USB drive, and
/// walks away. Each file inside still self-identifies (filename +
/// CSV column titles) so pandas / Excel work later without a
/// `_README` lookup.
///
/// Implementation: dial data_tcp once per sensor for LIST, then once
/// per (sensor, file) for GET. Files concatenated into an in-memory
/// zip via the `zip` crate's stored-or-deflated entries. CSVs
/// compress well, so the zip is typically 1/4-1/3 the raw size.
async fn data_zip_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    use zip::write::{SimpleFileOptions, ZipWriter};
    use zip::CompressionMethod;
    use std::io::{Cursor, Write};

    // v0.2 (SPEC-R2-WORKSHOP-DASHBOARD §5.1): bundle from the
    // controller-local captures dir instead of round-tripping every
    // connected sensor. Much faster, works while sensors are offline,
    // and matches the operator's "data is already on the laptop" mental
    // model after auto-sync (SPEC-R2-WORKSHOP-CAPTURE §7.4).
    //
    // Files in the captures dir are pre-spliced (main captures have
    // the CSV header inline; sidecars are byte-for-byte). Both go
    // into the zip as-is.
    let sessions = state.captures.list_sessions().await;
    if sessions.is_empty() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "No synced captures on the controller yet — connect sensors and run an experiment first.".to_string(),
        ).into_response();
    }

    let mut buf: Vec<u8> = Vec::new();
    let zip_cursor = Cursor::new(&mut buf);
    let mut zw = ZipWriter::new(zip_cursor);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut files_added: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    for session in &sessions {
        for entry in &session.files {
            let body = match std::fs::read(&entry.controller_path) {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("read {}: {}", entry.controller_path.display(), e));
                    continue;
                }
            };
            let out_name = entry.controller_path.file_name()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| format!("{}__{}.csv", entry.session_stem, entry.device_safe));

            if let Err(e) = zw.start_file(&out_name, opts) {
                errors.push(format!("zip start_file {}: {}", out_name, e));
                continue;
            }
            if let Err(e) = zw.write_all(&body) {
                errors.push(format!("zip body {}: {}", out_name, e));
                continue;
            }
            files_added += 1;
        }
    }

    // Optional manifest with anything that went wrong. Keeps the zip
    // self-describing.
    if !errors.is_empty() {
        let _ = zw.start_file("_errors.txt", opts);
        let _ = zw.write_all(errors.join("\n").as_bytes());
    }

    if let Err(e) = zw.finish() {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("zip finish: {e}"),
        ).into_response();
    }

    if files_added == 0 {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "Controller-local store is empty.".to_string(),
        ).into_response();
    }

    // Date-stamped filename so multiple end-of-day grabs sit side by
    // side on the operator's USB drive without overwriting. UTC keeps
    // the date stable across timezone boundaries (operator might be
    // syncing the drive from a different machine later).
    let today = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "captures".to_string());
    let download_name = format!("r2-workshop-captures-{}.zip", today);

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "application/zip".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", download_name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, buf).into_response()
}

/// Small data_tcp LIST helper for `data_zip_handler`. Returns the file
/// names only — we don't need sizes/mtimes for the zip, just the
/// names to GET. Mirrors the parsing in `data_list_handler` minus the
/// JSON wrapping.
/// `GET /api/data/local/list` — controller-local capture index per
/// SPEC-R2-WORKSHOP-DASHBOARD §5.1 and SPEC-R2-WORKSHOP-CAPTURE §7.4.
/// Returns the CapturesStore sessions view as JSON: one row per
/// session-stem, with per-device files inside. Reflects what has
/// actually synced to the laptop — works even when no sensor is
/// currently connected.
async fn data_local_list_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let sessions = state.captures.list_sessions().await;
    (axum::http::StatusCode::OK, Json(serde_json::json!({"sessions": sessions}))).into_response()
}

/// `DELETE /api/data/local/all` — wipe every synced file from the
/// controller-local store. Pairs with the operator's "Delete all
/// data" action; per-sensor DELETE_ALL remains the way to clear the
/// sensor's SD ring.
async fn data_local_delete_all_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match state.captures.clear_all().await {
        Ok(n) => (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "removed": n}))).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

/// `DELETE /api/data/session/{stem}` — delete one session from the logical
/// store. The store is the abstraction: a session's data lives both on the
/// controller (auto-synced) and on each sensor's SD (the original, left after
/// sync), so "delete" removes both. Local copies go now; the SD copy is wiped
/// on any currently-connected sensor immediately and on offline sensors when
/// they reconnect (via the persistent tombstone), so a deleted session never
/// re-syncs back.
async fn data_delete_session_handler(
    State(state): State<Arc<AppState>>,
    Path(stem): Path<String>,
) -> impl IntoResponse {
    let (removed_local, targets) = match state.captures.delete_session(&stem).await {
        Ok(v) => v,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
    };
    let mut sd_deleted = 0usize;
    let mut sd_pending = 0usize;
    for (device_pk, name) in &targets {
        let ip = {
            let peers = state.peers.read().await;
            peers.iter()
                .find(|(_, p)| p.device_pk.as_deref() == Some(device_pk.as_str()))
                .map(|(sa, _)| sa.ip().to_string())
        };
        match ip {
            Some(ip) if delete_file_on_sensor(&ip, name).await => {
                state.captures.clear_tombstone(device_pk, name).await;
                sd_deleted += 1;
            }
            // Offline (no live peer) or the SD delete failed / file absent —
            // the tombstone stays and the sync engine reconciles on reconnect
            // (and self-prunes if the file's already gone).
            _ => sd_pending += 1,
        }
    }
    eprintln!("[captures] delete session '{stem}': {removed_local} local, {sd_deleted} SD now, {sd_pending} tombstoned");
    (axum::http::StatusCode::OK, Json(serde_json::json!({
        "ok": true,
        "removed_local": removed_local,
        "sd_deleted": sd_deleted,
        "sd_pending": sd_pending,
    }))).into_response()
}

/// `GET /api/data/local/file/{name}` — stream a single file from the
/// controller-local captures dir per SPEC-R2-WORKSHOP-DASHBOARD §5.1.
/// `{name}` is the `<stem>__<dev>.csv` (or `.marks.csv`) filename as
/// it appears in the local/list response.
async fn data_local_file_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Path-traversal guard: only accept names that match the
    // on-disk shape the captures store wrote. The store's lookup
    // helper already filters by file_name() match against the index,
    // so a bogus `../../etc/passwd` won't return Some(entry).
    let Some(entry) = state.captures.lookup_on_disk_name(&name).await else {
        return (axum::http::StatusCode::NOT_FOUND, "not in captures store").into_response();
    };
    let body = match tokio::fs::read(&entry.controller_path).await {
        Ok(b) => b,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {}: {}", entry.controller_path.display(), e),
            ).into_response();
        }
    };
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, body).into_response()
}

async fn list_files_on_sensor(addr: &str) -> std::io::Result<Vec<String>> {
    let mut s = dial_data_tcp(addr).await?;
    s.write_all(&[0x01u8]).await?;
    let mut status = [0u8; 1];
    s.read_exact(&mut status).await?;
    if status[0] != ST_OK {
        let msg = read_err_msg(&mut s).await.unwrap_or_default();
        return Err(std::io::Error::new(std::io::ErrorKind::Other,
            format!("LIST status_byte={}: {}", status[0], msg)));
    }
    let mut count_buf = [0u8; 4];
    s.read_exact(&mut count_buf).await?;
    let count = u32::from_be_bytes(count_buf) as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut nl = [0u8; 2];
        s.read_exact(&mut nl).await?;
        let nlen = u16::from_be_bytes(nl) as usize;
        let mut name_buf = vec![0u8; nlen];
        s.read_exact(&mut name_buf).await?;
        let mut size_buf = [0u8; 8];
        s.read_exact(&mut size_buf).await?;
        let mut mtime_buf = [0u8; 8];
        s.read_exact(&mut mtime_buf).await?;
        out.push(String::from_utf8_lossy(&name_buf).into_owned());
    }
    Ok(out)
}

/// Decode a `r2.sensor.capture.state` CBOR payload (WIRE row 20) into
/// `(state, filename?)`. Returns `None` if the payload doesn't look
/// like the expected map. Used by the auto-sync transition watcher
/// (SPEC-R2-WORKSHOP-CAPTURE §7.4).
fn decode_capture_state(payload: &[u8]) -> Option<CaptureStateSnapshot> {
    let json = decode_cbor_payload(payload)?;
    let obj = json.as_object()?;
    // Payload keys are int-keyed CBOR; decode_cbor_payload stringifies
    // them, so "0" is the state byte and "1" is the optional filename.
    let state = obj.get("0").and_then(|v| v.as_u64())? as u8;
    let filename = obj.get("1")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(CaptureStateSnapshot { state, filename })
}

/// SPEC-R2-WORKSHOP-CAPTURE §7.4 — fetch a just-finalised capture
/// file (and its event-mark sidecar if present) from the named sensor
/// over `data_tcp` (port 21047), write under the controller-local
/// captures dir with the device-stamped filename + CSV header, then
/// emit `r2.dash.capture.synced` (WIRE row 44) for every viewer.
///
/// Detached: spawned from the per-peer dispatch loop on a
/// `Recording → Idle` transition. Errors are logged but not
/// propagated — the next reconciliation pass (§7.4) will retry.
async fn sync_capture_from_sensor(
    state: Arc<AppState>,
    addr: String,
    sensor_filename: String,
) {
    // device_pk + device_safe — same resolution path as
    // data_get_handler / data_zip_handler. Snapshot under the locks,
    // then release before hitting the network.
    let ip_only = addr.split(':').next().unwrap_or(&addr).to_string();
    let device_pk = {
        let peers = state.peers.read().await;
        peers.iter()
            .find(|(sa, _)| sa.ip().to_string() == ip_only)
            .and_then(|(_, p)| p.device_pk.clone())
    };
    let Some(device_pk) = device_pk else {
        eprintln!("[sync] {addr}: no device_pk yet for {sensor_filename} — will retry next reconciliation pass");
        return;
    };
    let alias = {
        let g = state.device_aliases.lock().await;
        g.get(&device_pk).cloned()
    };
    let raw_name = alias.unwrap_or_else(|| ip_only.replace('.', "_"));
    let device_safe: String = raw_name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();

    // Idempotency: if we've already synced this (device_pk, filename),
    // skip. Reconciliation pass can race with the transition watcher.
    if state.captures.has(&device_pk, &sensor_filename).await {
        return;
    }

    let addr_listen = format!("{ip_only}:{DATA_PORT}");

    // Main capture file first. Wrap a short delay around the very
    // first fetch attempt — the firmware emits `state=Idle` at the
    // tail end of `stop()` so the fsync has landed; this gives the
    // FAT cache a moment to settle on slow SD cards.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    // Tell viewers a sync is starting — they'll show a "Syncing…"
    // pill on the matching session row until the synced event lands.
    broadcast_sync_started(&state, &device_pk, &sensor_filename, "data").await;

    match fetch_file_on_sensor(&addr_listen, &sensor_filename).await {
        Ok(body) => {
            match state.captures.write_data(&device_pk, &device_safe, &sensor_filename, &body, 0).await {
                Ok(entry) => {
                    eprintln!("[sync] {ip_only} → {} ({} bytes)",
                        entry.controller_path.display(), entry.size);
                    broadcast_synced(&state, &entry).await;
                }
                Err(e) => eprintln!("[sync] write {sensor_filename}: {e}"),
            }
        }
        Err(e) => {
            eprintln!("[sync] fetch {addr_listen} {sensor_filename}: {e}");
            // No retry here — the 60 s reconciliation pass will pick
            // it up. Return so we don't also try the sidecar fetch
            // (which is doomed if the sensor TCP is unavailable).
            return;
        }
    }

    // Event-mark sidecar — same stem with `.marks.csv` suffix per
    // SPEC-R2-WORKSHOP-CAPTURE §4.1. Optional: only exists when the
    // operator hit Mark at least once during the recording. Fetch
    // attempt is best-effort; ENOENT-style errors are silent.
    let stem = sensor_filename.strip_suffix(".csv").unwrap_or(&sensor_filename);
    let marks_name = format!("{stem}.marks.csv");
    if state.captures.has(&device_pk, &marks_name).await {
        return;
    }
    broadcast_sync_started(&state, &device_pk, &marks_name, "marks").await;
    match fetch_file_on_sensor(&addr_listen, &marks_name).await {
        Ok(body) => {
            match state.captures.write_marks(&device_pk, &device_safe, &marks_name, &body, 0).await {
                Ok(entry) => {
                    eprintln!("[sync] {ip_only} → {} (sidecar, {} bytes)",
                        entry.controller_path.display(), entry.size);
                    broadcast_synced(&state, &entry).await;
                }
                Err(e) => eprintln!("[sync] write marks {marks_name}: {e}"),
            }
        }
        Err(_) => {
            // No sidecar — operator didn't Mark during this run.
            // Silent. Reconciliation poll won't re-try because the
            // sensor's LIST won't return a file that doesn't exist.
        }
    }
}

/// SPEC-R2-WORKSHOP-CAPTURE §7.4 reconciliation pass. One snapshot
/// over the currently-connected peers; per peer, LIST via data_tcp,
/// fetch any file the captures store doesn't already have. `ST_BUSY`
/// (the live recording) is skipped here — the transition watcher
/// catches it on Stop. Errors are logged and the pass continues.
async fn reconcile_captures_pass(state: &Arc<AppState>) {
    // Snapshot (ip, device_pk, alias) per peer up front; release the
    // locks before hitting the network so the dispatch loop isn't
    // blocked while we wait on sensor TCP.
    let targets: Vec<(String, String, String)> = {
        let peers = state.peers.read().await;
        let aliases = state.device_aliases.lock().await;
        peers.iter()
            .filter_map(|(sa, p)| {
                let device_pk = p.device_pk.clone()?;
                let ip_only = sa.ip().to_string();
                let raw_name = aliases.get(&device_pk)
                    .cloned()
                    .unwrap_or_else(|| ip_only.replace('.', "_"));
                let device_safe: String = raw_name.chars()
                    .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                    .collect();
                Some((ip_only, device_pk, device_safe))
            })
            .collect()
    };

    if targets.is_empty() {
        return;
    }

    for (ip, device_pk, device_safe) in targets {
        reconcile_single_peer(state, &ip, &device_pk, &device_safe).await;
    }
}

/// SPEC-R2-WORKSHOP-CAPTURE §7.4: one-peer variant of the
/// reconciliation pass. Used by the fleet-wide 60 s loop AND by the
/// immediate-on-reconnect path in handle_sensor_connection's announce
/// handler — when a sensor's TCP comes back up, we don't wait up to
/// 60 s for the next fleet poll, we kick a one-shot pass for that
/// peer right then. Closes the operator-visible blind window after
/// a mid-experiment sensor reset.
async fn reconcile_single_peer(
    state: &Arc<AppState>,
    ip: &str,
    device_pk: &str,
    device_safe: &str,
) {
    let addr_listen = format!("{ip}:{DATA_PORT}");
    let listing = match list_files_on_sensor(&addr_listen).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[sync recon] {ip}: list failed: {e}");
            return;
        }
    };
    // Self-clean: drop any tombstone for this device whose file is no longer
    // on the SD (already gone / rotated out) so the set can't grow unbounded.
    state.captures.prune_tombstones(device_pk, &listing).await;
    for fname in listing {
        // The operator deleted this session while the sensor was offline — wipe
        // the SD copy now instead of re-syncing it, then drop the tombstone, so
        // a deleted session never reappears on reconnect.
        if state.captures.is_tombstoned(device_pk, &fname).await {
            if delete_file_on_sensor(&addr_listen, &fname).await {
                state.captures.clear_tombstone(device_pk, &fname).await;
                eprintln!("[sync recon] {ip}: wiped tombstoned {fname} off SD (deleted session)");
            }
            continue;
        }
        if state.captures.has(device_pk, &fname).await { continue; }
        let is_marks = fname.ends_with(".marks.csv");
        broadcast_sync_started(state, device_pk, &fname,
            if is_marks { "marks" } else { "data" }).await;
        match fetch_file_on_sensor(&addr_listen, &fname).await {
            Ok(body) => {
                let write_result = if is_marks {
                    state.captures.write_marks(device_pk, device_safe, &fname, &body, 0).await
                } else {
                    state.captures.write_data(device_pk, device_safe, &fname, &body, 0).await
                };
                match write_result {
                    Ok(entry) => {
                        eprintln!("[sync recon] {ip} → {} ({} bytes{})",
                            entry.controller_path.display(),
                            entry.size,
                            if is_marks { ", sidecar" } else { "" });
                        broadcast_synced(state, &entry).await;
                    }
                    Err(e) => eprintln!("[sync recon] write {fname}: {e}"),
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("status_byte=2") {
                    eprintln!("[sync recon] {ip}/{fname}: {msg}");
                }
            }
        }
    }
}

/// Emit `r2.dash.capture.sync_started` so viewers can show a
/// "Syncing…" pill on the session row while the fetch is in flight.
/// Paired with `r2.dash.capture.synced` (success) — viewers clear
/// the pill when the matching synced event arrives.
async fn broadcast_sync_started(
    state: &Arc<AppState>,
    device_pk: &str,
    sensor_filename: &str,
    kind: &str,
) {
    let mut buf = vec![0u8; 32 + device_pk.len() + sensor_filename.len() + kind.len()];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let _ = enc.map(3);
    let _ = enc.kv(1, &r2_cbor::Value::Text(device_pk));
    let _ = enc.kv(2, &r2_cbor::Value::Text(sensor_filename));
    let _ = enc.kv(5, &r2_cbor::Value::Text(kind));
    let used = enc.len();
    buf.truncate(used);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CAPTURE_SYNC_STARTED, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Emit `r2.dash.capture.synced` (WIRE row 44) on `/r2` so every
/// connected viewer can update the session-row sync badge in real
/// time. Best-effort — if no viewers are listening the broadcast just
/// drops on the floor.
async fn broadcast_synced(state: &Arc<AppState>, entry: &captures::CaptureEntry) {
    let kind_str = match entry.kind {
        captures::CaptureKind::Data => "data",
        captures::CaptureKind::Marks => "marks",
    };
    // CBOR map per SPEC-R2-WORKSHOP-WIRE row 44.
    let mut buf = vec![0u8; 64 + entry.device_pk.len() + entry.sensor_filename.len() + kind_str.len()];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let _ = enc.map(5);
    let _ = enc.kv(1, &r2_cbor::Value::Text(&entry.device_pk));
    let _ = enc.kv(2, &r2_cbor::Value::Text(&entry.sensor_filename));
    let _ = enc.kv(3, &r2_cbor::Value::UInt(entry.size));
    let _ = enc.kv(4, &r2_cbor::Value::UInt(entry.fetched_at_ms));
    let _ = enc.kv(5, &r2_cbor::Value::Text(kind_str));
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CAPTURE_SYNCED, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Small data_tcp GET helper for `data_zip_handler`. Mirrors
/// `data_get_handler` minus the HTTP framing and the CSV-header
/// splicing (the zip handler stamps that header itself so it can use
/// the same device-safe name as the filename).
async fn fetch_file_on_sensor(addr: &str, name: &str) -> std::io::Result<Vec<u8>> {
    let mut s = dial_data_tcp(addr).await?;
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x02);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    s.write_all(&req).await?;
    let mut status = [0u8; 1];
    s.read_exact(&mut status).await?;
    if status[0] != ST_OK {
        let msg = read_err_msg(&mut s).await.unwrap_or_default();
        return Err(std::io::Error::new(std::io::ErrorKind::Other,
            format!("GET status_byte={}: {}", status[0], msg)));
    }
    let mut size_buf = [0u8; 8];
    s.read_exact(&mut size_buf).await?;
    let size = u64::from_be_bytes(size_buf) as usize;
    let mut body = vec![0u8; size];
    s.read_exact(&mut body).await?;
    Ok(body)
}

/// Delete one capture file off a sensor's SD via `data_tcp` OP_DEL
/// (`[0x03][u16 BE len][name]` → status byte). Returns true on `ST_OK`.
/// Best-effort: any transport error returns false. Used by the per-session
/// delete handler and the sync engine's tombstone reconciliation.
async fn delete_file_on_sensor(addr: &str, name: &str) -> bool {
    let mut s = match dial_data_tcp(addr).await { Ok(s) => s, Err(_) => return false };
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x03);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    if s.write_all(&req).await.is_err() { return false; }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() { return false; }
    status[0] == ST_OK
}

/// Path used by `load_device_aliases` / `save_device_aliases`. We
/// store under `$XDG_CONFIG_HOME` (falling back to `~/.config`) so
/// renames travel with the controller account; a fresh dashboard
/// install on the same machine picks them back up.
fn device_aliases_path() -> std::path::PathBuf {
    let cfg = std::env::var("XDG_CONFIG_HOME").ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.config")
        });
    std::path::PathBuf::from(cfg).join("r2-workshop").join("device_aliases.json")
}

fn load_device_aliases() -> HashMap<String, String> {
    let path = device_aliases_path();
    let Ok(bytes) = std::fs::read(&path) else { return HashMap::new(); };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        eprintln!("[aliases] {:?} not valid JSON — starting empty", path);
        return HashMap::new();
    };
    let Some(obj) = value.as_object() else { return HashMap::new(); };
    let mut out = HashMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    if !out.is_empty() {
        eprintln!("[aliases] loaded {} aliases from {:?}", out.len(), path);
    }
    out
}

fn save_device_aliases(map: &HashMap<String, String>) {
    let path = device_aliases_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(map).unwrap_or_else(|_| "{}".to_string());
    if let Err(e) = std::fs::write(&path, json) {
        eprintln!("[aliases] write {:?}: {e}", path);
    }
}

/// `GET /api/devices/aliases` — return the current device_pk → name map.
async fn device_aliases_get_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let g = state.device_aliases.lock().await;
    let map: serde_json::Map<String, serde_json::Value> = g.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    (axum::http::StatusCode::OK, Json(serde_json::Value::Object(map)))
}

/// `POST /api/devices/alias` `{device_pk, name}` — set / clear an
/// alias. Empty / null name clears. Broadcasts on /ws/status so
/// every connected dashboard browser picks up the change.
/// Shared device-alias set/clear core. Returns `Ok(final_name)` —
/// empty string means the alias was cleared. `Err(msg)` on validation
/// failure. Persists to disk and emits `r2.dash.device.alias.changed`
/// on success.
async fn do_device_alias_set(
    state: &Arc<AppState>,
    device_pk: &str,
    name: &str,
) -> Result<String, String> {
    if device_pk.is_empty() {
        return Err("device_pk required".to_string());
    }
    if device_pk.len() != 64 || !device_pk.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("device_pk must be 64 hex chars".to_string());
    }
    let trimmed = name.trim().to_string();
    let map_snapshot;
    {
        let mut g = state.device_aliases.lock().await;
        if trimmed.is_empty() {
            g.remove(device_pk);
        } else {
            // Cap + sanitise — surfaces in CSV filenames so no
            // path-busting characters.
            let clean: String = trimmed.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
                .take(64).collect();
            g.insert(device_pk.to_string(), clean);
        }
        map_snapshot = g.clone();
    }
    save_device_aliases(&map_snapshot);
    let final_name = map_snapshot.get(device_pk).cloned().unwrap_or_default();
    emit_device_alias_changed(state, device_pk, &final_name);
    Ok(final_name)
}

fn parse_row_ts_ms(row: &[u8]) -> Option<i64> {
    if row.len() < 26 { return None; }
    // bytes 11..25 carry ts_ms (right-aligned). Trim ASCII spaces, parse.
    let ts_field = &row[11..25];
    let s = std::str::from_utf8(ts_field).ok()?;
    s.trim().parse::<i64>().ok()
}

fn trim_str(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Fetch one capture file from `<addr>:21047` over data_tcp GET.
async fn fetch_capture_bytes(addr: &str, name: &str) -> std::io::Result<Vec<u8>> {
    let mut s = dial_data_tcp(addr).await?;
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x02);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    s.write_all(&req).await?;
    let mut status = [0u8; 1];
    s.read_exact(&mut status).await?;
    if status[0] != ST_OK {
        let _ = read_err_msg(&mut s).await;
        return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("status {}", status[0])));
    }
    let mut size_buf = [0u8; 8];
    s.read_exact(&mut size_buf).await?;
    let size = u64::from_be_bytes(size_buf) as usize;
    let mut body = vec![0u8; size];
    s.read_exact(&mut body).await?;
    Ok(body)
}

async fn read_err_msg(s: &mut TcpStream) -> Option<String> {
    let mut ml = [0u8; 2];
    s.read_exact(&mut ml).await.ok()?;
    let len = u16::from_be_bytes(ml) as usize;
    let mut msg = vec![0u8; len];
    s.read_exact(&mut msg).await.ok()?;
    Some(String::from_utf8_lossy(&msg).into_owned())
}

pub(crate) fn encode_raw_frame_envelope(rf: &RawFrame) -> Vec<u8> {
    let src = rf.src.as_bytes();
    let mut out = Vec::with_capacity(2 + src.len() + 4 + 2 + rf.frame.len());
    out.extend_from_slice(&(src.len() as u16).to_be_bytes());
    out.extend_from_slice(src);
    out.extend_from_slice(&(rf.ts_ms as u32).to_be_bytes());
    out.extend_from_slice(&(rf.frame.len() as u16).to_be_bytes());
    out.extend_from_slice(&rf.frame);
    out
}

/// `/api/keyholder/tg-pub` — return the trust-group public key (hex).
///
/// Used by browsers during enrolment to confirm they're talking to the
/// expected TG (cross-check against the QR-code-encoded TG fingerprint).
async fn tg_pub_handler() -> impl IntoResponse {
    // trust_keys/tg_pub.bin sits at the repo root, two levels up from
    // dashboard/src/main.rs.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("trust_keys/tg_pub.bin"));
    let bytes = match path.and_then(|p| std::fs::read(p).ok()) {
        Some(b) if b.len() == 32 => b,
        _ => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "tg_pub.bin not found or wrong length",
                    "hint": "run tools/r2-workshop-tg keygen and copy tg_pub.bin to trust_keys/",
                })),
            )
                .into_response();
        }
    };
    let hex_str: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    Json(serde_json::json!({
        "tg_public_key_hex": hex_str,
        "tg_public_key_len": 32,
    }))
    .into_response()
}

// ── SPEC-R2-WORKSHOP-ACCESS handlers ────────────────────────────────────
//
// All four routes share one helper that fetches the AccessHandle from
// state. The handlers themselves are small — the heavy lifting lives in
// `access.rs` (TrustGroup wrangling, token table, QR rendering).

/// Returns the `AccessHandle` or a 503 response describing why Access
/// is offline.
async fn require_access(state: &Arc<AppState>) -> std::result::Result<
    access::AccessHandle,
    axum::response::Response,
> {
    match state.access.as_ref() {
        Some(h) => Ok(h.clone()),
        None => Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "Access is not configured on this dashboard.",
                "hint": "Run tools/r2-workshop-tg keygen to generate tg_priv.bin under ~/.config/r2-workshop/tg_signer/, then restart.",
            })),
        ).into_response()),
    }
}

/// KeyHolder gate for v0.1 per SPEC-R2-WORKSHOP-ACCESS §11.1 (2): only
/// the controller's own browser may invite, list, or revoke. The check
/// is "the request came in over a loopback address." A cert-handshake
/// gate replaces this in v1.
fn is_keyholder(connect: SocketAddr) -> bool {
    connect.ip().is_loopback()
}

async fn access_whoami_handler(
    State(state): State<Arc<AppState>>,
    Path(device_pk): Path<String>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    let access = handle.lock().await;
    match access.lookup_member(&device_pk) {
        Some(row) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "enrolled": !row.revoked,
                "revoked":  row.revoked,
                "name":     row.name,
                "role":     row.role,
                "paired_at_ms": row.paired_at_ms,
            })),
        ).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "enrolled": false,
                "error":    "no such member",
            })),
        ).into_response(),
    }
}

async fn access_onboard_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "only the KeyHolder (localhost) may fetch onboarding QRs",
            })),
        ).into_response();
    }
    let host_override = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let access = handle.lock().await;
    match access.onboard_info(host_override.as_deref()) {
        Ok(info) => (axum::http::StatusCode::OK, Json(serde_json::to_value(&info).unwrap_or_default())).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ).into_response(),
    }
}

/// `/api/enrol-init` — KeyHolder generates a one-time join token.
/// **Stub** until Phase 5d-enrol; returns 501 NotImplemented for now.
/// When implemented: returns `{ token, qr_payload, expires_at }`.
async fn enrol_init_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "enrolment not yet implemented",
            "phase": "5d-enrol",
        })),
    )
}

/// `/api/enrol-complete` — browser submits its public key + token; KeyHolder
/// verifies, issues a TG-signed device cert. **Stub** until Phase 5d-enrol.
async fn enrol_complete_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "enrolment not yet implemented",
            "phase": "5d-enrol",
        })),
    )
}
