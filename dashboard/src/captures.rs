//! Controller-local capture store + auto-sync engine bookkeeping per
//! SPEC-R2-WORKSHOP-CAPTURE §7.4 (auto-sync to controller) and
//! §4.1 / §7.5 (event-mark sidecars).
//!
//! Files land at `$XDG_DATA_HOME/r2-workshop/captures/` (fallback
//! `~/.local/share/r2-workshop/captures/`). Filename convention is
//! `<sensor-stem>__<dev>.csv` for main captures and
//! `<sensor-stem>__<dev>.marks.csv` for sidecars — same convention
//! the per-sensor download + zip endpoints have been using since
//! v0.1, so the on-disk layout is self-describing without an index.
//! The in-memory index is a cache to avoid re-fetching files we
//! already have; if it gets out of sync with disk a restart fixes
//! it by re-scanning.
//!
//! No `sled` / SQLite / anything heavyweight — the index is rebuilt
//! on every boot from a directory scan. Operators who want to wipe
//! the laptop's captures can `rm -rf` the directory and restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::Mutex;

/// One per-file index entry. Keyed in the in-memory map by
/// `(device_pk, sensor_filename)` because the same `<stem>.csv` can
/// legitimately appear on multiple sensors at the same time.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureEntry {
    /// 64-hex Ed25519 public key from the sensor's announce.
    pub device_pk: String,
    /// Operator-resolved device-safe name at the time of fetch
    /// (alias if set, IP-with-underscores fallback, sanitised). The
    /// `<dev>` portion of the on-disk filename.
    pub device_safe: String,
    /// Sensor-side filename (what `data_tcp` LIST returned), e.g.
    /// `2026-05-26_14-22-01-stress-test.csv`. Does not include the
    /// `__<dev>` suffix or the `.marks.csv` variant — that suffix
    /// is in `kind`.
    pub sensor_filename: String,
    /// The session-stem (filename without `.csv` extension). The
    /// sessions-first Data tab groups rows by this. Sidecar entries
    /// carry the SAME `session_stem` as their data sibling so the
    /// 🎬 N badge can be rendered in one pass over the index.
    pub session_stem: String,
    /// On-disk path under the captures dir.
    pub controller_path: PathBuf,
    /// Bytes (excluding the spliced CSV header for `kind = Data`).
    pub size: u64,
    /// Local-write epoch ms.
    pub fetched_at_ms: u64,
    /// Sensor-side mtime if known (from LIST); 0 if we didn't have it.
    pub mtime_ms: i64,
    pub kind: CaptureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureKind {
    /// Main capture (`<stem>__<dev>.csv`). Has a spliced CSV header.
    Data,
    /// Event-mark sidecar (`<stem>__<dev>.marks.csv`). Plain UTF-8,
    /// no header splicing — sensor writes the `# r2-workshop event
    /// marks v1\nts_ms,mark_id,label\n` preamble itself per §4.1.
    Marks,
}

impl CaptureKind {
    fn suffix(self) -> &'static str {
        match self {
            CaptureKind::Data => ".csv",
            CaptureKind::Marks => ".marks.csv",
        }
    }
}

/// Cache key — (device_pk, sensor_filename) is unique per file on the
/// fleet. Two sensors can have the same sensor_filename (same session)
/// without colliding.
type IndexKey = (String, String);

/// Singleton-per-dashboard. `Arc<Mutex<_>>`-shared because the sync
/// engine task, the reconciliation poll, and the HTTP handlers all
/// touch it.
pub struct CapturesStore {
    /// Absolute path to the captures directory. Created at `new()` if
    /// missing.
    dir: PathBuf,
    /// In-memory index. Built from a disk scan in `load()`.
    index: Mutex<HashMap<IndexKey, CaptureEntry>>,
    /// Monotonic counter for `r2.dash.capture.event_mark.mark_id`.
    /// Resets on dashboard restart — collisions across restarts are
    /// fine, the `(ts_ms, label)` pair disambiguates downstream
    /// (CAPTURE §7.5).
    next_mark_id: AtomicU32,
}

impl CapturesStore {
    /// Build the store, ensure the directory exists, scan it, populate
    /// the index. Idempotent — calling on a startup where some files
    /// already exist on disk picks them up without re-fetching.
    pub async fn load() -> std::io::Result<Arc<Self>> {
        let dir = captures_dir();
        std::fs::create_dir_all(&dir)?;

        let mut index: HashMap<IndexKey, CaptureEntry> = HashMap::new();

        // Disk scan — anything matching `<stem>__<dev>.csv` or
        // `<stem>__<dev>.marks.csv` is indexable. Files that don't
        // match the pattern are left alone (operator-dropped files,
        // README, anything else).
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                let Some(fname) = path.file_name().and_then(|s| s.to_str()) else { continue; };
                let Some((sensor_stem, device_safe, kind)) = parse_capture_filename(fname) else { continue; };
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let size = meta.len();
                let mtime_ms = meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);

                // We can't recover device_pk from the filename alone
                // (alias name is operator-chosen, not the pk). Use a
                // synthetic key derived from device_safe — once a
                // matching sensor reconnects and the alias is known
                // again, re-indexing on the next sync will overwrite
                // the synthetic entry with the real device_pk.
                let synthetic_pk = format!("alias:{}", device_safe);
                let session_stem = sensor_stem.strip_suffix(".csv")
                    .unwrap_or(&sensor_stem)
                    .to_string();
                let sensor_filename = format!("{}.csv", session_stem);
                let key = (synthetic_pk.clone(), sensor_filename.clone());
                index.insert(key, CaptureEntry {
                    device_pk: synthetic_pk,
                    device_safe: device_safe.clone(),
                    sensor_filename,
                    session_stem,
                    controller_path: path.clone(),
                    size,
                    fetched_at_ms: mtime_ms.max(0) as u64,
                    mtime_ms,
                    kind,
                });
            }
        }

        eprintln!("[captures] {} entries indexed under {:?}", index.len(), dir);

        Ok(Arc::new(Self {
            dir,
            index: Mutex::new(index),
            next_mark_id: AtomicU32::new(1),
        }))
    }

    pub fn dir(&self) -> &Path { &self.dir }

    /// Issue the next `mark_id`. Monotonic for the controller process
    /// lifetime per SPEC-R2-WORKSHOP-CAPTURE §7.5.
    pub fn next_mark_id(&self) -> u32 {
        self.next_mark_id.fetch_add(1, Ordering::Relaxed)
    }

    /// `true` if we've already fetched this `(device_pk, sensor_filename)`
    /// pair — guards the sync engine against duplicate work when a
    /// reconciliation pass races with the transition watcher, or when
    /// a sensor re-LISTs the same file across reconciliation cycles.
    pub async fn has(&self, device_pk: &str, sensor_filename: &str) -> bool {
        let g = self.index.lock().await;
        g.contains_key(&(device_pk.to_string(), sensor_filename.to_string()))
    }

    /// Atomic: write the file to disk + record the index entry. Returns
    /// the controller path for the caller to use in the
    /// `r2.dash.capture.synced` event payload.
    pub async fn write_data(
        &self,
        device_pk: &str,
        device_safe: &str,
        sensor_filename: &str,
        body: &[u8],
        mtime_ms: i64,
    ) -> std::io::Result<CaptureEntry> {
        let stem = sensor_filename.strip_suffix(".csv").unwrap_or(sensor_filename);
        let on_disk_name = format!("{}__{}.csv", stem, device_safe);
        let path = self.dir.join(&on_disk_name);

        // CSV header splice — same convention as data_get_handler in
        // main.rs. Pre-spliced on disk so the local file is
        // self-describing in pandas without re-running the dashboard.
        let header = format!("seq,ts_ms,{0}_x,{0}_y,{0}_z\n", device_safe);
        let mut out: Vec<u8> = Vec::with_capacity(header.len() + body.len());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(body);
        atomic_write(&path, &out)?;

        let entry = CaptureEntry {
            device_pk: device_pk.to_string(),
            device_safe: device_safe.to_string(),
            sensor_filename: sensor_filename.to_string(),
            session_stem: stem.to_string(),
            controller_path: path,
            size: body.len() as u64,
            fetched_at_ms: now_ms(),
            mtime_ms,
            kind: CaptureKind::Data,
        };
        let mut g = self.index.lock().await;
        // Real pk now known — drop any synthetic placeholder from the
        // disk-scan that referred to the same on-disk file. Otherwise
        // the index would carry both and replay both to viewers.
        let synth_pk = format!("alias:{}", device_safe);
        g.remove(&(synth_pk, sensor_filename.to_string()));
        g.insert((device_pk.to_string(), sensor_filename.to_string()), entry.clone());
        Ok(entry)
    }

    /// Sidecar variant — sensor writes the v1 header itself per §4.1,
    /// so we copy byte-for-byte without splicing anything.
    pub async fn write_marks(
        &self,
        device_pk: &str,
        device_safe: &str,
        sensor_filename: &str,
        body: &[u8],
        mtime_ms: i64,
    ) -> std::io::Result<CaptureEntry> {
        let stem = sensor_filename.strip_suffix(".marks.csv")
            .unwrap_or_else(|| sensor_filename.strip_suffix(".csv").unwrap_or(sensor_filename));
        let on_disk_name = format!("{}__{}.marks.csv", stem, device_safe);
        let path = self.dir.join(&on_disk_name);
        atomic_write(&path, body)?;

        let entry = CaptureEntry {
            device_pk: device_pk.to_string(),
            device_safe: device_safe.to_string(),
            sensor_filename: sensor_filename.to_string(),
            session_stem: stem.to_string(),
            controller_path: path,
            size: body.len() as u64,
            fetched_at_ms: now_ms(),
            mtime_ms,
            kind: CaptureKind::Marks,
        };
        let mut g = self.index.lock().await;
        let synth_pk = format!("alias:{}", device_safe);
        g.remove(&(synth_pk, sensor_filename.to_string()));
        g.insert((device_pk.to_string(), sensor_filename.to_string()), entry.clone());
        Ok(entry)
    }

    /// Sessions-first index view for the Data tab: each row is one
    /// session-stem, files grouped by device.
    pub async fn list_sessions(&self) -> Vec<SessionRow> {
        let g = self.index.lock().await;
        let mut by_stem: HashMap<String, SessionRow> = HashMap::new();
        for entry in g.values() {
            let row = by_stem.entry(entry.session_stem.clone())
                .or_insert_with(|| SessionRow {
                    session_stem: entry.session_stem.clone(),
                    earliest_mtime_ms: entry.mtime_ms,
                    latest_mtime_ms: entry.mtime_ms,
                    total_size: 0,
                    files: Vec::new(),
                });
            if entry.mtime_ms != 0 {
                if row.earliest_mtime_ms == 0 || entry.mtime_ms < row.earliest_mtime_ms {
                    row.earliest_mtime_ms = entry.mtime_ms;
                }
                if entry.mtime_ms > row.latest_mtime_ms {
                    row.latest_mtime_ms = entry.mtime_ms;
                }
            }
            row.total_size += entry.size;
            row.files.push(entry.clone());
        }

        let mut rows: Vec<SessionRow> = by_stem.into_values().collect();
        // Latest-first so the run the operator just finished is at the top.
        rows.sort_by(|a, b| b.latest_mtime_ms.cmp(&a.latest_mtime_ms));
        for row in &mut rows {
            // Within a session, list main files first (by device), then sidecars.
            row.files.sort_by(|a, b| {
                let kind_order = |k: CaptureKind| match k { CaptureKind::Data => 0, CaptureKind::Marks => 1 };
                kind_order(a.kind).cmp(&kind_order(b.kind))
                    .then_with(|| a.device_safe.cmp(&b.device_safe))
            });
        }
        rows
    }

    /// Wipe the in-memory index AND every file under the captures
    /// directory. Used by `DELETE /api/data/local/all` per the
    /// operator-visible "Delete all data" action. Returns the count
    /// of files removed (best effort — IO errors are logged, not
    /// raised, so a single permission glitch on one file doesn't
    /// strand the rest).
    pub async fn clear_all(&self) -> std::io::Result<usize> {
        let mut g = self.index.lock().await;
        g.clear();
        drop(g);
        let mut removed = 0usize;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                // Only touch files we created — preserves any
                // operator-dropped README or similar.
                let Some(fname) = path.file_name().and_then(|s| s.to_str()) else { continue; };
                if parse_capture_filename(fname).is_none() { continue; }
                match std::fs::remove_file(&path) {
                    Ok(_) => removed += 1,
                    Err(e) => eprintln!("[captures] remove {:?}: {e}", path),
                }
            }
        }
        Ok(removed)
    }

    /// Look up an entry by the on-disk filename (used by
    /// `/api/data/local/file/{name}` to validate the request).
    pub async fn lookup_on_disk_name(&self, on_disk_name: &str) -> Option<CaptureEntry> {
        let g = self.index.lock().await;
        g.values()
            .find(|e| {
                e.controller_path.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s == on_disk_name)
                    .unwrap_or(false)
            })
            .cloned()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionRow {
    pub session_stem: String,
    pub earliest_mtime_ms: i64,
    pub latest_mtime_ms: i64,
    pub total_size: u64,
    pub files: Vec<CaptureEntry>,
}

/// `$XDG_DATA_HOME/r2-workshop/captures/`, falling back to
/// `~/.local/share/r2-workshop/captures/`.
pub fn captures_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME").ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.local/share")
        });
    PathBuf::from(base).join("r2-workshop").join("captures")
}

/// Parse `<sensor-stem>__<dev>.csv` or `<sensor-stem>__<dev>.marks.csv`
/// into `(sensor_stem_with_dot_csv, device_safe, kind)`.
/// Returns None if the filename doesn't look like a capture.
///
/// We split on the LAST `__` so a session-stem that itself contains `_`
/// (very common: hyphen-and-underscore allowed in operator-supplied
/// names) doesn't split in the wrong place.
fn parse_capture_filename(fname: &str) -> Option<(String, String, CaptureKind)> {
    let (kind, base) = if let Some(stem) = fname.strip_suffix(".marks.csv") {
        (CaptureKind::Marks, stem)
    } else if let Some(stem) = fname.strip_suffix(".csv") {
        (CaptureKind::Data, stem)
    } else {
        return None;
    };
    let (stem, dev) = base.rsplit_once("__")?;
    if stem.is_empty() || dev.is_empty() { return None; }
    Some((format!("{}.csv", stem), dev.to_string(), kind))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp.{}",
        path.extension().and_then(|s| s.to_str()).unwrap_or(""),
        std::process::id(),
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_capture_filename_data() {
        let (stem, dev, kind) = parse_capture_filename(
            "2026-05-26_14-22-01-stress-test__left.csv",
        ).unwrap();
        assert_eq!(stem, "2026-05-26_14-22-01-stress-test.csv");
        assert_eq!(dev, "left");
        assert_eq!(kind, CaptureKind::Data);
    }

    #[test]
    fn parse_capture_filename_marks() {
        let (stem, dev, kind) = parse_capture_filename(
            "2026-05-26_14-22-01-stress-test__right.marks.csv",
        ).unwrap();
        assert_eq!(stem, "2026-05-26_14-22-01-stress-test.csv");
        assert_eq!(dev, "right");
        assert_eq!(kind, CaptureKind::Marks);
    }

    #[test]
    fn parse_capture_filename_rejects_non_capture() {
        assert!(parse_capture_filename("README.md").is_none());
        assert!(parse_capture_filename("not-a-capture.csv").is_none());
        assert!(parse_capture_filename("__leading-empty.csv").is_none());
        assert!(parse_capture_filename("trailing-empty__.csv").is_none());
    }

    #[test]
    fn parse_capture_filename_handles_underscores_in_stem() {
        let (stem, dev, _kind) = parse_capture_filename(
            "2026_05_26_some_under_scored_stem__alias.csv",
        ).unwrap();
        assert_eq!(stem, "2026_05_26_some_under_scored_stem.csv");
        assert_eq!(dev, "alias");
    }
}
