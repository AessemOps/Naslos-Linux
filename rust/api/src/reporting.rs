//! Bounded historical reporting store (FR-RPT, `docs/reporting.md`).
//!
//! A small append-only file of fixed-width, checksummed rollup records keeps
//! at most three resolutions (1 min / 5 min / 1 h) on the reporting volume.
//! There is deliberately no database dependency: the whole store is one file
//! plus a tiny series registry, and every query scans at most the retained
//! window. Recording runs in its own task and is best-effort: a broken store
//! degrades to "no history" and never touches live metrics.

use crate::agent::DiskHealthReport;
use crate::metrics::{NetworkInterface, SystemMetrics};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// File magic. The trailing newline makes a truncated header obvious.
const MAGIC: &[u8] = b"NASLOSRPT1\n";
const HEADER_LEN: u64 = 11;
/// Fixed-width on-disk record size.
pub const RECORD_SIZE: usize = 56;

pub const RES_RAW: u32 = 60;
pub const RES_5M: u32 = 300;
pub const RES_1H: u32 = 3600;

const RAW_RETENTION: i64 = 86_400;
const FIVE_MIN_RETENTION: i64 = 7 * 86_400;
const HOUR_RETENTION: i64 = 30 * 86_400;

/// Compile-time cardinality cap (`docs/reporting.md` §3).
pub const MAX_SERIES: usize = 256;
/// Maximum series a single query may ask for.
pub const MAX_QUERY_SERIES: usize = 8;
/// Default hard cap for the history file.
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_PATH: &str = "/var/lib/naslos/reporting/history.bin";

/// In-memory window kept to derive closed rollups without re-reading the file.
const RAW_MEMORY: i64 = 20 * 60;
const FIVE_MIN_MEMORY: i64 = 2 * 3600;
const HOUR_MEMORY: i64 = 3 * 3600;

/// The three supported query ranges. The resolution is implied, never chosen
/// by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportRange {
    Hours24,
    Days7,
    Days30,
}

impl ReportRange {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "24h" => Some(Self::Hours24),
            "7d" => Some(Self::Days7),
            "30d" => Some(Self::Days30),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hours24 => "24h",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
        }
    }

    pub fn resolution(self) -> u32 {
        match self {
            Self::Hours24 => RES_RAW,
            Self::Days7 => RES_5M,
            Self::Days30 => RES_1H,
        }
    }

    fn window_seconds(self) -> i64 {
        match self {
            Self::Hours24 => RAW_RETENTION,
            Self::Days7 => FIVE_MIN_RETENTION,
            Self::Days30 => HOUR_RETENTION,
        }
    }

    fn max_points(self) -> usize {
        match self {
            Self::Hours24 => 1_440,
            Self::Days7 => 2_016,
            Self::Days30 => 720,
        }
    }
}

/// One series definition from the allowlist registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SeriesMeta {
    pub id: u32,
    pub key: String,
    pub kind: String,
    pub unit: String,
}

/// A single stored bucket (`min`, `max`, weighted `avg`, newest `last`, count).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    min: f64,
    max: f64,
    avg: f64,
    last: f64,
    count: u64,
}

impl Sample {
    fn single(value: f64) -> Self {
        Self {
            min: value,
            max: value,
            avg: value,
            last: value,
            count: 1,
        }
    }

    fn empty() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            avg: 0.0,
            last: 0.0,
            count: 0,
        }
    }

    /// Merge a newer sample into this accumulator. `last` follows append order.
    fn merge_after(&mut self, other: Sample) {
        if other.count == 0 {
            return;
        }
        if self.count == 0 {
            *self = other;
            return;
        }
        let total = self.count.saturating_add(other.count);
        self.avg = (self.avg * self.count as f64 + other.avg * other.count as f64) / total as f64;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.last = other.last;
        self.count = total;
    }
}

#[derive(Clone, Copy, Debug)]
struct Record {
    id: u32,
    resolution: u32,
    bucket: i64,
    sample: Sample,
}

#[derive(Debug)]
struct Scan {
    records: Vec<Record>,
    /// Bytes consumed by valid records (header excluded).
    valid_bytes: u64,
    header_ok: bool,
    truncated: bool,
}

impl Scan {
    fn empty(header_ok: bool) -> Self {
        Self {
            records: Vec::new(),
            valid_bytes: 0,
            header_ok,
            truncated: false,
        }
    }
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut crc = i as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
            *slot = crc;
        }
        table
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let table = crc_table();
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(buf: &mut [u8], offset: usize, value: i64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_f64(buf: &mut [u8], offset: usize, value: f64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
}

fn get_i64(buf: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

fn get_f64(buf: &[u8], offset: usize) -> f64 {
    f64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

/// Encode a record. Layout: CRC32 (4) | id (4) | resolution (4) | count (4) |
/// bucket (8) | min/max/avg/last (32).
fn encode_record(record: &Record, buf: &mut [u8; RECORD_SIZE]) {
    buf.fill(0);
    put_u32(buf, 4, record.id);
    put_u32(buf, 8, record.resolution);
    let count = record.sample.count.min(u64::from(u32::MAX)) as u32;
    put_u32(buf, 12, count);
    put_i64(buf, 16, record.bucket);
    put_f64(buf, 24, record.sample.min);
    put_f64(buf, 32, record.sample.max);
    put_f64(buf, 40, record.sample.avg);
    put_f64(buf, 48, record.sample.last);
    let crc = crc32(&buf[4..]);
    put_u32(buf, 0, crc);
}

fn decode_record(buf: &[u8]) -> Option<Record> {
    if buf.len() != RECORD_SIZE {
        return None;
    }
    let expected = get_u32(buf, 0);
    if crc32(&buf[4..]) != expected {
        return None;
    }
    let id = get_u32(buf, 4);
    if id == 0 {
        return None;
    }
    let resolution = get_u32(buf, 8);
    if !matches!(resolution, RES_RAW | RES_5M | RES_1H) {
        return None;
    }
    let count = u64::from(get_u32(buf, 12));
    if count == 0 {
        return None;
    }
    let bucket = get_i64(buf, 16);
    if bucket.rem_euclid(i64::from(resolution)) != 0 {
        return None;
    }
    let sample = Sample {
        min: get_f64(buf, 24),
        max: get_f64(buf, 32),
        avg: get_f64(buf, 40),
        last: get_f64(buf, 48),
        count,
    };
    if !sample.min.is_finite()
        || !sample.max.is_finite()
        || !sample.avg.is_finite()
        || !sample.last.is_finite()
        || sample.min > sample.max
    {
        return None;
    }
    Some(Record {
        id,
        resolution,
        bucket,
        sample,
    })
}

/// Floor a Unix timestamp to its bucket start.
pub fn floor_bucket(timestamp: i64, resolution: u32) -> i64 {
    let resolution = i64::from(resolution);
    timestamp - timestamp.rem_euclid(resolution)
}

fn scan_file(path: &Path) -> std::io::Result<Scan> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Scan::empty(false)),
        Err(e) => return Err(e),
    };
    let mut magic = [0u8; HEADER_LEN as usize];
    if file.read_exact(&mut magic).is_err() || magic != MAGIC {
        return Ok(Scan::empty(false));
    }
    let mut scan = Scan::empty(true);
    let mut buf = [0u8; RECORD_SIZE];
    loop {
        match file.read_exact(&mut buf) {
            Ok(()) => match decode_record(&buf) {
                Some(record) => {
                    scan.records.push(record);
                    scan.valid_bytes += RECORD_SIZE as u64;
                }
                None => {
                    scan.truncated = true;
                    break;
                }
            },
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    let len = file.metadata()?.len();
    if len > HEADER_LEN + scan.valid_bytes {
        scan.truncated = true;
    }
    Ok(scan)
}

/// Visit every whole record in the store without materializing a `Vec`.
/// Queries and retention passes only need one record at a time, which keeps a
/// full history scan from doubling peak memory during rollups.
fn for_each_record(path: &Path, mut visit: impl FnMut(Record)) -> std::io::Result<()> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut magic = [0u8; HEADER_LEN as usize];
    if file.read_exact(&mut magic).is_err() || magic != MAGIC {
        return Ok(());
    }
    let mut buf = [0u8; RECORD_SIZE];
    loop {
        match file.read_exact(&mut buf) {
            Ok(()) => match decode_record(&buf) {
                Some(record) => visit(record),
                None => break,
            },
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
fn read_records(path: &Path) -> std::io::Result<Vec<Record>> {
    let mut records = Vec::new();
    for_each_record(path, |record| records.push(record))?;
    Ok(records)
}

fn now_string(timestamp: i64) -> String {
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// Best-effort directory fsync so a rename survives a crash.
fn sync_directory(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
}

/// Series registry: key -> stable numeric id. Written atomically and kept
/// small (the allowlist caps it at 256 entries).
struct Registry {
    path: PathBuf,
    next_id: u32,
    by_key: BTreeMap<String, SeriesMeta>,
    by_id: HashMap<u32, SeriesMeta>,
    dirty: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RegistryFile {
    version: u32,
    next_id: u32,
    series: Vec<SeriesMeta>,
}

impl Registry {
    fn load(path: PathBuf) -> Self {
        let mut registry = Self {
            path,
            next_id: 1,
            by_key: BTreeMap::new(),
            by_id: HashMap::new(),
            dirty: false,
        };
        let raw = match std::fs::read(&registry.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == ErrorKind::NotFound => return registry,
            Err(e) => {
                tracing::warn!("reporting series registry unreadable ({e}); starting empty");
                return registry;
            }
        };
        let parsed: RegistryFile = match serde_json::from_slice(&raw) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::warn!("reporting series registry is corrupt ({e}); starting empty");
                let backup = registry
                    .path
                    .with_extension(format!("corrupt-{}", Utc::now().timestamp()));
                let _ = std::fs::rename(&registry.path, backup);
                return registry;
            }
        };
        let mut max_id = 0;
        for meta in parsed.series {
            if meta.id == 0
                || registry.by_id.contains_key(&meta.id)
                || registry.by_key.contains_key(&meta.key)
            {
                continue;
            }
            max_id = max_id.max(meta.id);
            registry.by_id.insert(meta.id, meta.clone());
            registry.by_key.insert(meta.key.clone(), meta);
        }
        registry.next_id = parsed.next_id.max(max_id.saturating_add(1)).max(1);
        registry
    }

    fn save(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let file = RegistryFile {
            version: 1,
            next_id: self.next_id,
            series: self.by_key.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec(&file).unwrap_or_default();
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut out = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            out.write_all(&bytes)?;
            out.sync_all()?;
        }
        set_mode(&tmp, 0o600)?;
        std::fs::rename(&tmp, &self.path)?;
        sync_directory(&self.path);
        self.dirty = false;
        Ok(())
    }

    fn id_for(&mut self, key: &str, kind: &str, unit: &str) -> Option<u32> {
        if let Some(meta) = self.by_key.get(key) {
            return Some(meta.id);
        }
        if self.by_key.len() >= MAX_SERIES {
            return None;
        }
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        let meta = SeriesMeta {
            id,
            key: key.to_string(),
            kind: kind.to_string(),
            unit: unit.to_string(),
        };
        self.by_id.insert(id, meta.clone());
        self.by_key.insert(key.to_string(), meta);
        self.dirty = true;
        Some(id)
    }

    fn list(&self) -> Vec<SeriesMeta> {
        self.by_key.values().cloned().collect()
    }
}

struct Store {
    path: PathBuf,
    file: File,
    registry: Registry,
    max_bytes: u64,
    net_counters: HashMap<String, (u64, u64, i64)>,
    recent_raw: VecDeque<Record>,
    recent_five: VecDeque<Record>,
    recent_hour: VecDeque<Record>,
    last_prune: i64,
    warned_series: bool,
    warned_size: bool,
}

impl Store {
    fn open(path: &Path, max_bytes: u64) -> Result<Self, String> {
        let max_bytes = max_bytes.max((RECORD_SIZE * 16) as u64);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating reporting directory: {e}"))?;
            // A mounted PVC root may be owned by root even though the API can
            // write through group/other bits; tightening it is best-effort.
            // The store file itself is always chmodded below.
            if let Err(e) = set_mode(parent, 0o750) {
                tracing::debug!("could not set reporting directory mode: {e}");
            }
        }

        let mut scan = scan_file(path).map_err(|e| format!("reading reporting store: {e}"))?;
        if !scan.header_ok {
            if path.exists()
                && std::fs::metadata(path)
                    .map(|meta| meta.is_file() && meta.len() > 0)
                    .unwrap_or(false)
            {
                let backup = path.with_extension(format!("corrupt-{}", Utc::now().timestamp()));
                std::fs::rename(path, &backup)
                    .map_err(|e| format!("moving corrupt reporting store aside: {e}"))?;
                tracing::warn!(
                    "reporting store had an invalid header; moved aside to {}",
                    backup.display()
                );
            }
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .map_err(|e| format!("creating reporting store: {e}"))?;
            file.write_all(MAGIC)
                .map_err(|e| format!("writing reporting header: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("syncing reporting header: {e}"))?;
            scan = Scan::empty(true);
        }
        if scan.truncated {
            let valid = HEADER_LEN + scan.valid_bytes;
            let file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|e| format!("opening reporting store for repair: {e}"))?;
            file.set_len(valid)
                .map_err(|e| format!("repairing reporting store: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("syncing repaired reporting store: {e}"))?;
            tracing::warn!(
                "reporting store had a corrupt tail; truncated to {} valid bytes",
                scan.valid_bytes
            );
        }
        set_mode(path, 0o600).map_err(|e| format!("setting reporting file mode: {e}"))?;

        let max_record_id = scan
            .records
            .iter()
            .map(|record| record.id)
            .max()
            .unwrap_or(0);
        let mut registry = Registry::load(path.with_file_name("series.json"));
        // A lost/corrupt registry must not hand out ids that already label
        // records in history.bin: old samples would silently appear under a
        // new key. Start above every id present in the file instead.
        if registry.next_id <= max_record_id {
            registry.next_id = max_record_id.saturating_add(1);
            registry.dirty = true;
        }
        if registry.dirty {
            if let Err(e) = registry.save() {
                tracing::warn!("could not persist reconciled reporting series ids: {e}");
            }
        }
        let file = open_append(path).map_err(|e| format!("opening reporting store: {e}"))?;
        let mut store = Self {
            path: path.to_path_buf(),
            file,
            registry,
            max_bytes,
            net_counters: HashMap::new(),
            recent_raw: VecDeque::new(),
            recent_five: VecDeque::new(),
            recent_hour: VecDeque::new(),
            last_prune: 0,
            warned_series: false,
            warned_size: false,
        };
        let now = Utc::now().timestamp();
        // Release the startup scan before the rollup rebuild reads the same
        // file again; retaining both copies doubles peak memory on a large
        // history.
        drop(scan);
        if let Err(e) = store.rebuild_rollups(now) {
            tracing::warn!("reporting rollup rebuild: {e}");
        }
        Ok(store)
    }

    fn file_len(&self) -> std::io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    /// Truncate a partial record left by an interrupted append and return the
    /// last whole-record offset. Without this, every later append would sit
    /// behind an undecodable tail and disappear on the next scan.
    fn align_tail(&mut self) -> Result<u64, String> {
        let len = self.file_len().map_err(|e| e.to_string())?;
        let records = len.saturating_sub(HEADER_LEN) / RECORD_SIZE as u64;
        let aligned = HEADER_LEN + records * RECORD_SIZE as u64;
        if len != aligned {
            self.file
                .set_len(aligned)
                .map_err(|e| format!("repairing a partial reporting append: {e}"))?;
            tracing::warn!("reporting store truncated a partial record before append");
        }
        Ok(aligned)
    }

    fn append(&mut self, record: Record, now: i64) -> Result<(), String> {
        let start = self.align_tail()?;
        if start + RECORD_SIZE as u64 > self.max_bytes {
            self.prune(now, true)?;
        }
        let start = self.align_tail()?;
        if start + RECORD_SIZE as u64 > self.max_bytes {
            return Err("reporting store is at its size cap".to_string());
        }
        let mut buf = [0u8; RECORD_SIZE];
        encode_record(&record, &mut buf);
        if let Err(e) = self.file.write_all(&buf) {
            // A short write must not poison every later record.
            let _ = self.file.set_len(start);
            return Err(format!("writing reporting sample: {e}"));
        }
        Ok(())
    }

    fn push_recent(&mut self, record: Record, now: i64) {
        match record.resolution {
            RES_RAW => {
                self.recent_raw.push_back(record);
                while self
                    .recent_raw
                    .front()
                    .map(|r| r.bucket < now - RAW_MEMORY)
                    .unwrap_or(false)
                {
                    self.recent_raw.pop_front();
                }
            }
            RES_5M => {
                self.recent_five.push_back(record);
                while self
                    .recent_five
                    .front()
                    .map(|r| r.bucket < now - FIVE_MIN_MEMORY)
                    .unwrap_or(false)
                {
                    self.recent_five.pop_front();
                }
            }
            RES_1H => {
                self.recent_hour.push_back(record);
                while self
                    .recent_hour
                    .front()
                    .map(|r| r.bucket < now - HOUR_MEMORY)
                    .unwrap_or(false)
                {
                    self.recent_hour.pop_front();
                }
            }
            _ => {}
        }
    }

    /// Derive every closed rollup bucket we know about. Rollups are always
    /// recomputed from their source rows (raw -> 5 min, 5 min -> 1 h), never
    /// accumulated blindly, so a missed tick leaves a gap instead of corrupting
    /// the next level.
    fn derive_rollups(&mut self, now: i64) -> Result<(), String> {
        let mut five: BTreeMap<(u32, i64), Sample> = BTreeMap::new();
        for record in &self.recent_raw {
            let bucket = floor_bucket(record.bucket, RES_5M);
            if bucket + i64::from(RES_5M) <= now {
                five.entry((record.id, bucket))
                    .or_insert_with(Sample::empty)
                    .merge_after(record.sample);
            }
        }
        let mut existing: HashSet<(u32, i64)> = self
            .recent_five
            .iter()
            .map(|record| (record.id, record.bucket))
            .collect();
        for ((id, bucket), sample) in five {
            if !existing.insert((id, bucket)) {
                continue;
            }
            let record = Record {
                id,
                resolution: RES_5M,
                bucket,
                sample,
            };
            self.append(record, now)?;
            self.push_recent(record, now);
        }

        let mut hours: BTreeMap<(u32, i64), Sample> = BTreeMap::new();
        for record in &self.recent_five {
            let bucket = floor_bucket(record.bucket, RES_1H);
            if bucket + i64::from(RES_1H) <= now {
                hours
                    .entry((record.id, bucket))
                    .or_insert_with(Sample::empty)
                    .merge_after(record.sample);
            }
        }
        let mut existing: HashSet<(u32, i64)> = self
            .recent_hour
            .iter()
            .map(|record| (record.id, record.bucket))
            .collect();
        for ((id, bucket), sample) in hours {
            if !existing.insert((id, bucket)) {
                continue;
            }
            let record = Record {
                id,
                resolution: RES_1H,
                bucket,
                sample,
            };
            self.append(record, now)?;
            self.push_recent(record, now);
        }
        Ok(())
    }

    /// Fill in rollups that were not derived before a restart, then load the
    /// in-memory windows. Runs once at open.
    fn rebuild_rollups(&mut self, now: i64) -> Result<(), String> {
        let mut five: BTreeMap<(u32, i64), Sample> = BTreeMap::new();
        let mut existing_five: HashSet<(u32, i64)> = HashSet::new();
        let mut existing_hour: HashSet<(u32, i64)> = HashSet::new();
        for_each_record(&self.path, |record| match record.resolution {
            RES_RAW if record.bucket >= now - RAW_RETENTION => {
                let bucket = floor_bucket(record.bucket, RES_5M);
                five.entry((record.id, bucket))
                    .or_insert_with(Sample::empty)
                    .merge_after(record.sample);
            }
            RES_5M => {
                existing_five.insert((record.id, record.bucket));
            }
            RES_1H => {
                existing_hour.insert((record.id, record.bucket));
            }
            _ => {}
        })
        .map_err(|e| format!("reading reporting store: {e}"))?;
        for ((id, bucket), sample) in five {
            if bucket + i64::from(RES_5M) > now || !existing_five.insert((id, bucket)) {
                continue;
            }
            let record = Record {
                id,
                resolution: RES_5M,
                bucket,
                sample,
            };
            self.append(record, now)?;
        }

        let mut hours: BTreeMap<(u32, i64), Sample> = BTreeMap::new();
        for_each_record(&self.path, |record| {
            if record.resolution == RES_5M {
                let bucket = floor_bucket(record.bucket, RES_1H);
                hours
                    .entry((record.id, bucket))
                    .or_insert_with(Sample::empty)
                    .merge_after(record.sample);
            }
        })
        .map_err(|e| format!("reading reporting store: {e}"))?;
        for ((id, bucket), sample) in hours {
            if bucket + i64::from(RES_1H) > now || !existing_hour.insert((id, bucket)) {
                continue;
            }
            let record = Record {
                id,
                resolution: RES_1H,
                bucket,
                sample,
            };
            self.append(record, now)?;
        }

        self.load_recent(now)
            .map_err(|e| format!("reading reporting store: {e}"))?;
        self.prune(now, false)?;
        Ok(())
    }

    fn load_recent(&mut self, now: i64) -> std::io::Result<()> {
        self.recent_raw.clear();
        self.recent_five.clear();
        self.recent_hour.clear();
        let path = self.path.clone();
        for_each_record(&path, |record| match record.resolution {
            RES_RAW if record.bucket >= now - RAW_MEMORY => self.recent_raw.push_back(record),
            RES_5M if record.bucket >= now - FIVE_MIN_MEMORY => self.recent_five.push_back(record),
            RES_1H if record.bucket >= now - HOUR_MEMORY => self.recent_hour.push_back(record),
            _ => {}
        })
    }

    /// Enforce the age retention and the hard size cap by rewriting the file
    /// atomically. Runs hourly (and once at startup).
    fn prune(&mut self, now: i64, aggressive: bool) -> Result<(), String> {
        let mut before = 0usize;
        let mut kept: Vec<Record> = Vec::new();
        for_each_record(&self.path, |record| {
            before += 1;
            let age = now.saturating_sub(record.bucket);
            if record.bucket > now + 3600 {
                return;
            }
            let retain = match record.resolution {
                RES_RAW => age <= RAW_RETENTION,
                RES_5M => age <= FIVE_MIN_RETENTION,
                RES_1H => age <= HOUR_RETENTION,
                _ => false,
            };
            if retain {
                kept.push(record);
            }
        })
        .map_err(|e| e.to_string())?;
        let mut changed = kept.len() != before;

        let over_cap = self.file_len().map_err(|e| e.to_string())? > self.max_bytes;
        if aggressive || over_cap {
            let target = self.max_bytes.saturating_mul(3) / 4;
            let max_records = (target / RECORD_SIZE as u64) as usize;
            if kept.len() > max_records {
                kept.sort_by_key(|record| (record.bucket, record.resolution, record.id));
                let keep = max_records.max(1);
                let drop = kept.len().saturating_sub(keep);
                if drop > 0 {
                    kept.drain(0..drop);
                    changed = true;
                    if !self.warned_size {
                        tracing::warn!(
                            "reporting store reached its size cap; dropped {drop} oldest buckets"
                        );
                        self.warned_size = true;
                    }
                }
            }
        }
        if changed {
            self.rewrite(&kept)?;
        }
        self.last_prune = now;
        Ok(())
    }

    fn rewrite(&mut self, records: &[Record]) -> Result<(), String> {
        let tmp = self.path.with_extension("bin.tmp");
        {
            let mut out = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)
                .map_err(|e| format!("creating reporting rewrite: {e}"))?;
            out.write_all(MAGIC)
                .map_err(|e| format!("writing reporting rewrite header: {e}"))?;
            let mut buf = [0u8; RECORD_SIZE];
            for record in records {
                encode_record(record, &mut buf);
                out.write_all(&buf)
                    .map_err(|e| format!("writing reporting rewrite: {e}"))?;
            }
            out.sync_all()
                .map_err(|e| format!("syncing reporting rewrite: {e}"))?;
        }
        set_mode(&tmp, 0o600).map_err(|e| format!("setting reporting rewrite mode: {e}"))?;
        // Open the replacement handle before the rename: after a rename the
        // path names the new inode, so reopening afterwards can fail and leave
        // `self.file` pointing at the unlinked old inode (appends would then be
        // lost). With the handle already open the rename is the commit point.
        let replacement =
            open_append(&tmp).map_err(|e| format!("reopening reporting store: {e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("replacing reporting store: {e}"))?;
        sync_directory(&self.path);
        self.file = replacement;
        Ok(())
    }

    fn network_rate(&mut self, interface: &NetworkInterface, now: i64) -> Option<(f64, f64)> {
        let previous = self.net_counters.insert(
            interface.name.clone(),
            (interface.bytes_recv, interface.bytes_sent, now),
        );
        let (prev_rx, prev_tx, prev_at) = previous?;
        let elapsed = now - prev_at;
        if !(1..=600).contains(&elapsed) {
            return None;
        }
        if interface.bytes_recv < prev_rx || interface.bytes_sent < prev_tx {
            // Counter reset (link flap or reboot): a gap, never a spike.
            return None;
        }
        Some((
            (interface.bytes_recv - prev_rx) as f64 / elapsed as f64,
            (interface.bytes_sent - prev_tx) as f64 / elapsed as f64,
        ))
    }

    fn ingest(
        &mut self,
        at: i64,
        metrics: &SystemMetrics,
        disk: Option<&DiskHealthReport>,
    ) -> Result<(), String> {
        let mut values: Vec<(String, &'static str, &'static str, f64)> = vec![
            (
                "cpu.usage_percent".to_string(),
                "cpu",
                "percent",
                metrics.cpu.usage_percent,
            ),
            (
                "cpu.load1".to_string(),
                "cpu",
                "load",
                metrics.cpu.load_avg1,
            ),
            (
                "cpu.load5".to_string(),
                "cpu",
                "load",
                metrics.cpu.load_avg5,
            ),
            (
                "cpu.load15".to_string(),
                "cpu",
                "load",
                metrics.cpu.load_avg15,
            ),
        ];
        if let Some(temperature) = metrics.cpu.temperature_c {
            values.push((
                "cpu.temperature_c".to_string(),
                "cpu",
                "celsius",
                temperature,
            ));
        }

        values.push((
            "memory.total_bytes".to_string(),
            "memory",
            "bytes",
            metrics.memory.total as f64,
        ));
        values.push((
            "memory.used_bytes".to_string(),
            "memory",
            "bytes",
            metrics.memory.used as f64,
        ));
        values.push((
            "memory.available_bytes".to_string(),
            "memory",
            "bytes",
            metrics.memory.available as f64,
        ));
        values.push((
            "memory.usage_percent".to_string(),
            "memory",
            "percent",
            metrics.memory.usage_percent,
        ));
        values.push((
            "memory.swap_used_bytes".to_string(),
            "memory",
            "bytes",
            metrics.memory.swap_used as f64,
        ));

        let mut interfaces: Vec<&NetworkInterface> = metrics
            .network
            .interfaces
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|interface| is_physical_interface(&interface.name))
            .collect();
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        let mut seen = HashSet::new();
        for interface in interfaces.into_iter().take(16) {
            if !seen.insert(interface.name.clone()) {
                continue;
            }
            let Some((rx, tx)) = self.network_rate(interface, at) else {
                continue;
            };
            let name = sanitize_component(&interface.name);
            values.push((
                format!("network.{name}.rx_bytes_per_s"),
                "network",
                "bytes_per_s",
                rx,
            ));
            values.push((
                format!("network.{name}.tx_bytes_per_s"),
                "network",
                "bytes_per_s",
                tx,
            ));
        }
        // Bound the counter cache to the interfaces seen this tick; a renamed
        // NIC must not leave an entry behind forever.
        self.net_counters.retain(|name, _| seen.contains(name));

        let mut pools: Vec<&crate::metrics::PoolMetrics> = metrics
            .zfs
            .pools
            .as_deref()
            .unwrap_or_default()
            .iter()
            .collect();
        pools.sort_by(|a, b| a.name.cmp(&b.name));
        for pool in pools.into_iter().take(8) {
            let name = sanitize_component(&pool.name);
            values.push((
                format!("zfs.{name}.used_bytes"),
                "zfs",
                "bytes",
                pool.size.saturating_sub(pool.free) as f64,
            ));
            values.push((
                format!("zfs.{name}.alloc_bytes"),
                "zfs",
                "bytes",
                pool.alloc as f64,
            ));
            values.push((
                format!("zfs.{name}.free_bytes"),
                "zfs",
                "bytes",
                pool.free as f64,
            ));
        }

        if let Some(report) = disk {
            let mut disks: Vec<&crate::agent::DiskHealth> = report.disks.iter().collect();
            disks.sort_by(|a, b| a.device.cmp(&b.device));
            for disk in disks.into_iter().take(64) {
                let name = sanitize_component(&disk.device);
                if let Some(temperature) = disk.smart.temperature_c.filter(|value| *value > 0) {
                    values.push((
                        format!("disk.{name}.temperature_c"),
                        "disk",
                        "celsius",
                        temperature as f64,
                    ));
                }
                if let Some(wear) = disk.smart.wear_percent {
                    values.push((
                        format!("disk.{name}.wear_percent"),
                        "disk",
                        "percent",
                        wear as f64,
                    ));
                }
            }
        }

        let bucket = floor_bucket(at, RES_RAW);
        for (key, kind, unit, value) in values {
            if !value.is_finite() || value < 0.0 {
                continue;
            }
            let Some(id) = self.registry.id_for(&key, kind, unit) else {
                if !self.warned_series {
                    tracing::warn!(
                        "reporting series cap reached ({MAX_SERIES}); skipping new series {key}"
                    );
                    self.warned_series = true;
                }
                continue;
            };
            let record = Record {
                id,
                resolution: RES_RAW,
                bucket,
                sample: Sample::single(value),
            };
            self.append(record, at)?;
            self.push_recent(record, at);
        }

        self.derive_rollups(at)?;
        if at.saturating_sub(self.last_prune) >= 3600 {
            self.prune(at, false)?;
        } else if self.file_len().map_err(|e| e.to_string())? > self.max_bytes {
            self.prune(at, true)?;
        }
        self.registry
            .save()
            .map_err(|e| format!("saving reporting series registry: {e}"))
    }

    fn query(
        &self,
        range: ReportRange,
        requested: &[String],
        now: i64,
    ) -> Result<ReportSeriesResponse, String> {
        let from = now - range.window_seconds();
        let resolution = range.resolution();
        let mut ignored = Vec::new();
        let mut selected: Vec<SeriesMeta> = Vec::new();
        let mut ids: HashMap<u32, usize> = HashMap::new();

        let keys = if requested.is_empty() {
            default_keys(&self.registry.list())
        } else {
            requested.to_vec()
        };
        for key in keys {
            if selected.len() >= MAX_QUERY_SERIES {
                ignored.push(key);
                continue;
            }
            let Some(meta) = self.registry.by_key.get(&key) else {
                ignored.push(key);
                continue;
            };
            if ids.contains_key(&meta.id) {
                ignored.push(key);
                continue;
            }
            ids.insert(meta.id, selected.len());
            selected.push(meta.clone());
        }

        let mut groups: HashMap<(u32, i64), Sample> = HashMap::new();
        for_each_record(&self.path, |record| {
            // Half-open lower bound: `from` is the start of the requested
            // window, so a full window is exactly max_points buckets rather
            // than max_points + 1 and never needs a forced 2x downsample.
            if record.resolution != resolution || record.bucket <= from || record.bucket > now {
                return;
            }
            if !ids.contains_key(&record.id) {
                return;
            }
            groups
                .entry((record.id, record.bucket))
                .or_insert_with(Sample::empty)
                .merge_after(record.sample);
        })
        .map_err(|e| format!("reading history: {e}"))?;

        let mut collected: Vec<(SeriesMeta, Vec<(i64, Sample)>)> =
            Vec::with_capacity(selected.len());
        for meta in selected {
            let mut points: Vec<(i64, Sample)> = groups
                .iter()
                .filter(|((id, _), _)| *id == meta.id)
                .map(|((_, bucket), sample)| (*bucket, *sample))
                .collect();
            points.sort_by_key(|(bucket, _)| *bucket);
            collected.push((meta, points));
        }

        // A single response-wide factor keeps one `sample_interval_seconds`
        // accurate for every series, including series that start later or have
        // fewer points than the longest one.
        let largest = collected
            .iter()
            .map(|(_, points)| points.len())
            .max()
            .unwrap_or(0);
        let factor = if largest > range.max_points() {
            largest.div_ceil(range.max_points())
        } else {
            1
        };
        let interval = i64::from(resolution) * factor as i64;

        let mut series = Vec::with_capacity(collected.len());
        for (meta, points) in collected {
            let points = if factor > 1 {
                downsample(points, factor)
            } else {
                points
            };
            series.push(ReportSeries {
                key: meta.key,
                kind: meta.kind,
                unit: meta.unit,
                points: points
                    .into_iter()
                    .map(|(bucket, sample)| {
                        (
                            bucket,
                            sample.min,
                            sample.max,
                            sample.avg,
                            sample.last,
                            sample.count,
                        )
                    })
                    .collect(),
            });
        }

        Ok(ReportSeriesResponse {
            range: range.as_str().to_string(),
            resolution,
            sample_interval_seconds: interval,
            from: now_string(from),
            to: now_string(now),
            available: true,
            error: None,
            series,
            ignored,
        })
    }

    fn metadata(&self) -> ReportMetadata {
        ReportMetadata {
            enabled: true,
            available: true,
            error: None,
            max_bytes: self.max_bytes,
            max_series: MAX_SERIES,
            max_series_per_query: MAX_QUERY_SERIES,
            ranges: range_metadata(),
            series: self.registry.list(),
        }
    }
}

fn downsample(points: Vec<(i64, Sample)>, factor: usize) -> Vec<(i64, Sample)> {
    let factor = factor.max(1);
    let mut out = Vec::with_capacity(points.len().div_ceil(factor));
    for chunk in points.chunks(factor) {
        let Some((bucket, _)) = chunk.first() else {
            continue;
        };
        let mut merged = Sample::empty();
        for (_, sample) in chunk {
            merged.merge_after(*sample);
        }
        out.push((*bucket, merged));
    }
    out
}

/// Default dashboard set: CPU, memory, then the first available families.
fn default_keys(series: &[SeriesMeta]) -> Vec<String> {
    let mut out = Vec::new();
    for key in [
        "cpu.usage_percent",
        "memory.usage_percent",
        "memory.used_bytes",
    ] {
        if series.iter().any(|meta| meta.key == key) {
            out.push(key.to_string());
        }
    }
    for meta in series {
        if out.len() >= MAX_QUERY_SERIES {
            break;
        }
        if !out.contains(&meta.key) {
            out.push(meta.key.clone());
        }
    }
    out
}

fn range_metadata() -> Vec<ReportRangeMeta> {
    [
        ReportRange::Hours24,
        ReportRange::Days7,
        ReportRange::Days30,
    ]
    .into_iter()
    .map(|range| ReportRangeMeta {
        range: range.as_str().to_string(),
        resolution: range.resolution(),
        window_seconds: range.window_seconds(),
        max_points: range.max_points(),
    })
    .collect()
}

/// Sanitize a pool/device/interface name into a bounded series component.
/// Characters outside `[A-Za-z0-9._-]` collapse to `_`; names are not merged
/// case-insensitively.
pub fn sanitize_component(value: &str) -> String {
    let value = value.trim().trim_start_matches("/dev/");
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out.to_string()
    }
}

/// Heuristic physical-interface filter: the live collector can report pod and
/// overlay links, which are not useful in a node history and would eat the
/// allowlist.
fn is_physical_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name.is_empty() || name == "lo" {
        return false;
    }
    const VIRTUAL_PREFIXES: &[&str] = &[
        "veth", "docker", "cni", "flannel", "cali", "kube", "tunl", "vxlan", "br-", "virbr", "tap",
        "wg",
    ];
    !VIRTUAL_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

#[derive(Debug, Serialize)]
pub struct ReportRangeMeta {
    pub range: String,
    pub resolution: u32,
    pub window_seconds: i64,
    pub max_points: usize,
}

#[derive(Debug, Serialize)]
pub struct ReportMetadata {
    pub enabled: bool,
    pub available: bool,
    pub error: Option<String>,
    pub max_bytes: u64,
    pub max_series: usize,
    pub max_series_per_query: usize,
    pub ranges: Vec<ReportRangeMeta>,
    pub series: Vec<SeriesMeta>,
}

#[derive(Debug, Serialize)]
pub struct ReportSeries {
    pub key: String,
    pub kind: String,
    pub unit: String,
    /// `[bucket, min, max, avg, last, count]`
    pub points: Vec<(i64, f64, f64, f64, f64, u64)>,
}

#[derive(Debug, Serialize)]
pub struct ReportSeriesResponse {
    pub range: String,
    pub resolution: u32,
    pub sample_interval_seconds: i64,
    pub from: String,
    pub to: String,
    pub available: bool,
    pub error: Option<String>,
    pub series: Vec<ReportSeries>,
    pub ignored: Vec<String>,
}

impl ReportSeriesResponse {
    fn unavailable(range: ReportRange, now: i64, error: Option<String>) -> Self {
        Self {
            range: range.as_str().to_string(),
            resolution: range.resolution(),
            sample_interval_seconds: i64::from(range.resolution()),
            from: now_string(now - range.window_seconds()),
            to: now_string(now),
            available: false,
            error,
            series: Vec::new(),
            ignored: Vec::new(),
        }
    }
}

struct ReporterInner {
    store: Option<Store>,
    error: Option<String>,
}

/// The shared reporting handle in `AppState`. Recording is best-effort: this
/// never blocks the live metrics collector and a failed store leaves every
/// other API surface untouched.
pub struct Reporter {
    enabled: bool,
    path: PathBuf,
    max_bytes: u64,
    inner: Mutex<ReporterInner>,
    write_warned: AtomicBool,
}

impl Reporter {
    pub fn from_env() -> Self {
        let enabled = std::env::var("REPORTING_ENABLED")
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(true);
        let path = std::env::var("REPORTING_FILE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PATH.to_string());
        let max_bytes = std::env::var("REPORTING_MAX_BYTES")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_BYTES)
            .max((RECORD_SIZE * 16) as u64);
        Self::new(enabled, PathBuf::from(path), max_bytes)
    }

    pub fn new(enabled: bool, path: PathBuf, max_bytes: u64) -> Self {
        if !enabled {
            return Self::disabled();
        }
        match Store::open(&path, max_bytes) {
            Ok(store) => Self {
                enabled: true,
                path,
                max_bytes,
                inner: Mutex::new(ReporterInner {
                    store: Some(store),
                    error: None,
                }),
                write_warned: AtomicBool::new(false),
            },
            Err(error) => {
                tracing::warn!("reporting history unavailable: {error}");
                Self {
                    enabled: true,
                    path,
                    max_bytes,
                    inner: Mutex::new(ReporterInner {
                        store: None,
                        error: Some(error),
                    }),
                    write_warned: AtomicBool::new(false),
                }
            }
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            path: PathBuf::from(DEFAULT_PATH),
            max_bytes: DEFAULT_MAX_BYTES,
            inner: Mutex::new(ReporterInner {
                store: None,
                error: Some("historical reporting is disabled".to_string()),
            }),
            write_warned: AtomicBool::new(false),
        }
    }

    /// Whether the recorder is configured *and* has a usable store. A store
    /// that failed to open at startup leaves the recorder task off entirely.
    pub fn is_enabled(&self) -> bool {
        self.enabled && self.inner.lock().unwrap().store.is_some()
    }

    pub fn record(&self, at: i64, metrics: &SystemMetrics, disk: Option<&DiskHealthReport>) {
        let mut inner = self.inner.lock().unwrap();
        let Some(store) = inner.store.as_mut() else {
            return;
        };
        match store.ingest(at, metrics, disk) {
            Ok(()) => inner.error = None,
            Err(error) => {
                if !self.write_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!("reporting history write failed: {error}");
                }
                inner.error = Some(error);
            }
        }
    }

    pub fn query(
        &self,
        range: ReportRange,
        requested: &[String],
        now: i64,
    ) -> ReportSeriesResponse {
        let inner = self.inner.lock().unwrap();
        match inner.store.as_ref() {
            Some(store) => match store.query(range, requested, now) {
                Ok(mut response) => {
                    response.error = inner.error.clone();
                    response
                }
                Err(error) => ReportSeriesResponse::unavailable(range, now, Some(error)),
            },
            None => ReportSeriesResponse::unavailable(range, now, inner.error.clone()),
        }
    }

    pub fn metadata(&self) -> ReportMetadata {
        let inner = self.inner.lock().unwrap();
        match inner.store.as_ref() {
            Some(store) => {
                let mut meta = store.metadata();
                meta.error = inner.error.clone();
                meta
            }
            None => ReportMetadata {
                enabled: self.enabled,
                available: false,
                error: inner.error.clone(),
                max_bytes: self.max_bytes,
                max_series: MAX_SERIES,
                max_series_per_query: MAX_QUERY_SERIES,
                ranges: range_metadata(),
                series: Vec::new(),
            },
        }
    }

    /// Path of the backing file (diagnostics only; no secrets).
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn record_sample(&self, at: i64, key: &str, kind: &str, unit: &str, value: f64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(store) = inner.store.as_mut() {
            if let Some(id) = store.registry.id_for(key, kind, unit) {
                let record = Record {
                    id,
                    resolution: RES_RAW,
                    bucket: floor_bucket(at, RES_RAW),
                    sample: Sample::single(value),
                };
                let _ = store.append(record, at);
                store.push_recent(record, at);
                let _ = store.derive_rollups(at);
                let _ = store.registry.save();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{DiskHealth, DiskHealthReport, SmartInfo};
    use crate::metrics::{
        CpuMetrics, MemoryMetrics, NetworkInterface, NetworkMetrics, PoolMetrics,
    };

    fn sample() -> SystemMetrics {
        SystemMetrics {
            cpu: CpuMetrics {
                usage_percent: 12.5,
                load_avg1: 0.5,
                load_avg5: 0.4,
                load_avg15: 0.3,
                temperature_c: Some(45.0),
                ..Default::default()
            },
            memory: MemoryMetrics {
                total: 16_000,
                used: 4_000,
                available: 12_000,
                usage_percent: 25.0,
                swap_used: 0,
                ..Default::default()
            },
            network: NetworkMetrics {
                interfaces: Some(vec![
                    NetworkInterface {
                        name: "eth0".into(),
                        bytes_recv: 1_000,
                        bytes_sent: 2_000,
                        ..Default::default()
                    },
                    NetworkInterface {
                        name: "veth1234".into(),
                        bytes_recv: 9_000,
                        bytes_sent: 9_000,
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            zfs: crate::metrics::ZfsMetrics {
                pools: Some(vec![PoolMetrics {
                    name: "tank".into(),
                    size: 1_000,
                    alloc: 400,
                    free: 600,
                    usage_percent: 40.0,
                    health: "ONLINE".into(),
                }]),
            },
            ..Default::default()
        }
    }

    fn open_store(dir: &tempfile::TempDir) -> Store {
        Store::open(&dir.path().join("history.bin"), DEFAULT_MAX_BYTES).unwrap()
    }

    #[test]
    fn buckets_floor_at_boundaries() {
        assert_eq!(floor_bucket(59, RES_RAW), 0);
        assert_eq!(floor_bucket(60, RES_RAW), 60);
        assert_eq!(floor_bucket(61, RES_RAW), 60);
        assert_eq!(floor_bucket(299, RES_5M), 0);
        assert_eq!(floor_bucket(300, RES_5M), 300);
        assert_eq!(floor_bucket(301, RES_5M), 300);
        assert_eq!(floor_bucket(3599, RES_1H), 0);
        assert_eq!(floor_bucket(3600, RES_1H), 3600);
        assert_eq!(floor_bucket(3601, RES_1H), 3600);
    }

    #[test]
    fn merge_math_is_weighted_and_keeps_the_newest_last() {
        let mut merged = Sample::single(10.0);
        merged.merge_after(Sample::single(20.0));
        assert_eq!(merged.min, 10.0);
        assert_eq!(merged.max, 20.0);
        assert_eq!(merged.avg, 15.0);
        assert_eq!(merged.last, 20.0);
        assert_eq!(merged.count, 2);

        let mut weighted = Sample {
            min: 10.0,
            max: 10.0,
            avg: 10.0,
            last: 10.0,
            count: 3,
        };
        weighted.merge_after(Sample {
            min: 30.0,
            max: 30.0,
            avg: 30.0,
            last: 30.0,
            count: 1,
        });
        assert_eq!(weighted.avg, 15.0);
        assert_eq!(weighted.count, 4);
        assert_eq!(weighted.last, 30.0);
    }

    #[test]
    fn ingest_records_series_and_derives_closed_rollups() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let metrics = sample();
        for offset in (0..=300).step_by(60) {
            store.ingest(offset, &metrics, None).unwrap();
        }
        let raw = store.query(ReportRange::Hours24, &[], 300).unwrap();
        let cpu = raw
            .series
            .iter()
            .find(|series| series.key == "cpu.usage_percent")
            .unwrap();
        assert_eq!(cpu.points.len(), 6);
        assert_eq!(cpu.points[0].1, 12.5);

        let week = store.query(ReportRange::Days7, &[], 300).unwrap();
        let cpu = week
            .series
            .iter()
            .find(|series| series.key == "cpu.usage_percent")
            .unwrap();
        assert_eq!(cpu.points.len(), 1);
        assert_eq!(cpu.points[0].0, 0);
        assert_eq!(cpu.points[0].5, 5);
        assert_eq!(cpu.points[0].3, 12.5);
    }

    #[test]
    fn hour_rollup_is_derived_from_five_minute_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let metrics = sample();
        for offset in (0..=3600).step_by(60) {
            store.ingest(offset, &metrics, None).unwrap();
        }
        let month = store.query(ReportRange::Days30, &[], 3600).unwrap();
        let cpu = month
            .series
            .iter()
            .find(|series| series.key == "cpu.usage_percent")
            .unwrap();
        assert_eq!(cpu.points.len(), 1);
        assert_eq!(cpu.points[0].0, 0);
        assert_eq!(cpu.points[0].5, 60);
        assert_eq!(cpu.points[0].3, 12.5);
    }

    #[test]
    fn retention_prunes_each_resolution_and_keeps_the_30_day_floor() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let now = 40 * 86_400;
        let sample = Sample::single(1.0);
        for (resolution, bucket) in [
            (RES_RAW, floor_bucket(now - RAW_RETENTION - 60, RES_RAW)),
            (RES_RAW, floor_bucket(now - 60, RES_RAW)),
            (RES_5M, floor_bucket(now - FIVE_MIN_RETENTION - 300, RES_5M)),
            (RES_5M, floor_bucket(now - 300, RES_5M)),
            (RES_1H, floor_bucket(now - HOUR_RETENTION - 3600, RES_1H)),
            (RES_1H, floor_bucket(now - 3600, RES_1H)),
        ] {
            store
                .append(
                    Record {
                        id: 1,
                        resolution,
                        bucket,
                        sample,
                    },
                    now,
                )
                .unwrap();
        }
        store.prune(now, false).unwrap();
        let records = read_records(&store.path).unwrap();
        assert_eq!(records.len(), 3);
        assert!(records
            .iter()
            .all(|record| record.bucket >= now - HOUR_RETENTION));
    }

    #[test]
    fn a_full_window_returns_max_points_at_the_stored_interval() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let now = 1_800_000_000i64;
        let id = store
            .registry
            .id_for("cpu.usage_percent", "cpu", "percent")
            .unwrap();
        let start = now - RAW_RETENTION;
        for bucket in (start..=now).step_by(60) {
            store
                .append(
                    Record {
                        id,
                        resolution: RES_RAW,
                        bucket,
                        sample: Sample::single(1.0),
                    },
                    now,
                )
                .unwrap();
        }
        let response = store
            .query(
                ReportRange::Hours24,
                &["cpu.usage_percent".to_string()],
                now,
            )
            .unwrap();
        assert_eq!(response.series[0].points.len(), 1_440);
        assert_eq!(response.sample_interval_seconds, 60);
        assert_eq!(response.series[0].points.first().unwrap().0, start + 60);
        assert_eq!(response.series[0].points.last().unwrap().0, now);
    }

    #[test]
    fn a_lost_registry_starts_above_ids_already_in_history() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now().timestamp();
        let bucket = floor_bucket(now, RES_RAW);
        let old_id = {
            let mut store = open_store(&dir);
            let id = store
                .registry
                .id_for("old.series", "cpu", "percent")
                .unwrap();
            store
                .append(
                    Record {
                        id,
                        resolution: RES_RAW,
                        bucket,
                        sample: Sample::single(1.0),
                    },
                    now,
                )
                .unwrap();
            store.registry.save().unwrap();
            id
        };
        std::fs::remove_file(dir.path().join("series.json")).unwrap();

        let mut store = Store::open(&dir.path().join("history.bin"), DEFAULT_MAX_BYTES).unwrap();
        assert!(store.registry.next_id > old_id);
        let new_id = store
            .registry
            .id_for("new.series", "cpu", "percent")
            .unwrap();
        assert_ne!(new_id, old_id);
        let response = store
            .query(ReportRange::Hours24, &["new.series".to_string()], now)
            .unwrap();
        assert_eq!(response.series.len(), 1);
        assert!(
            response.series[0].points.is_empty(),
            "a new id must not surface the orphaned old samples"
        );
    }

    #[test]
    fn a_partial_append_is_truncated_before_the_next_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let now = Utc::now().timestamp();
        store.ingest(now, &sample(), None).unwrap();
        let path = dir.path().join("history.bin");
        {
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&[0xAB; RECORD_SIZE / 2]).unwrap();
        }
        let before = read_records(&path).unwrap().len();

        store.ingest(now + 60, &sample(), None).unwrap();
        let after = read_records(&path).unwrap().len();
        assert!(after > before);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!((len - HEADER_LEN) % RECORD_SIZE as u64, 0);
    }

    #[test]
    fn a_failed_rewrite_keeps_the_store_appendable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.bin");
        let mut store = Store::open(&path, DEFAULT_MAX_BYTES).unwrap();
        let now = Utc::now().timestamp();
        let record = Record {
            id: 1,
            resolution: RES_RAW,
            bucket: floor_bucket(now, RES_RAW),
            sample: Sample::single(1.0),
        };
        store.append(record, now).unwrap();

        // Occupy the rewrite temp path with a directory so the replacement
        // handle cannot be opened. The live store handle must stay usable.
        let tmp = path.with_extension("bin.tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let error = store.rewrite(&[record]).unwrap_err();
        assert!(
            error.contains("reporting rewrite") || error.contains("reopening"),
            "{error}"
        );
        std::fs::remove_dir_all(&tmp).unwrap();

        store.append(record, now).unwrap();
        assert_eq!(read_records(&path).unwrap().len(), 2);
    }

    #[test]
    fn corrupt_tail_is_truncated_and_the_store_keeps_writing() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now().timestamp();
        {
            let mut store = open_store(&dir);
            store.registry.id_for("cpu.usage_percent", "cpu", "percent");
            store
                .append(
                    Record {
                        id: 1,
                        resolution: RES_RAW,
                        bucket: floor_bucket(now, RES_RAW),
                        sample: Sample::single(1.0),
                    },
                    now,
                )
                .unwrap();
        }
        let path = dir.path().join("history.bin");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[0xAB; RECORD_SIZE / 2]).unwrap();
        drop(file);

        let mut store = Store::open(&path, DEFAULT_MAX_BYTES).unwrap();
        let records = read_records(&path).unwrap();
        assert_eq!(records.len(), 1);
        store
            .ingest(now + 60, &sample(), None)
            .expect("store still accepts writes after repair");
        assert!(read_records(&path).unwrap().len() > 1);
    }

    #[test]
    fn corrupt_header_is_moved_aside_and_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.bin");
        std::fs::write(&path, b"definitely not a history file").unwrap();
        let store = Store::open(&path, DEFAULT_MAX_BYTES).unwrap();
        assert_eq!(store.file_len().unwrap(), HEADER_LEN);
        assert!(std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.path().to_string_lossy().contains(".corrupt-")));
    }

    #[test]
    fn series_registry_holds_stable_ids_and_caps_cardinality() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let first = store
            .registry
            .id_for("cpu.usage_percent", "cpu", "percent")
            .unwrap();
        let again = store
            .registry
            .id_for("cpu.usage_percent", "cpu", "percent")
            .unwrap();
        assert_eq!(first, again);

        for index in 0..(MAX_SERIES + 20) {
            store
                .registry
                .id_for(&format!("disk.d{index}.temperature_c"), "disk", "celsius");
        }
        assert_eq!(store.registry.by_key.len(), MAX_SERIES);
        store.registry.save().unwrap();
        let reloaded = Registry::load(store.registry.path.clone());
        assert_eq!(reloaded.by_key.len(), MAX_SERIES);
        assert_eq!(
            reloaded.by_key.get("cpu.usage_percent").map(|meta| meta.id),
            Some(first)
        );
    }

    #[test]
    fn network_rates_skip_counter_resets_and_keep_bytes_per_second() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let mut metrics = sample();
        metrics.network.interfaces = Some(vec![NetworkInterface {
            name: "eth0".into(),
            bytes_recv: 1_000,
            bytes_sent: 2_000,
            ..Default::default()
        }]);
        store.ingest(0, &metrics, None).unwrap();
        metrics.network.interfaces = Some(vec![NetworkInterface {
            name: "eth0".into(),
            bytes_recv: 1_100,
            bytes_sent: 2_200,
            ..Default::default()
        }]);
        store.ingest(60, &metrics, None).unwrap();
        let response = store.query(
            ReportRange::Hours24,
            &[
                "network.eth0.rx_bytes_per_s".to_string(),
                "network.eth0.tx_bytes_per_s".to_string(),
            ],
            60,
        );
        let response = response.unwrap();
        let rx = response
            .series
            .iter()
            .find(|series| series.key == "network.eth0.rx_bytes_per_s")
            .unwrap();
        assert_eq!(rx.points.len(), 1);
        assert!((rx.points[0].3 - 1.666).abs() < 0.01);

        // A counter reset is a gap, not a spike.
        metrics.network.interfaces = Some(vec![NetworkInterface {
            name: "eth0".into(),
            bytes_recv: 10,
            bytes_sent: 20,
            ..Default::default()
        }]);
        store.ingest(120, &metrics, None).unwrap();
        let response = store
            .query(
                ReportRange::Hours24,
                &["network.eth0.rx_bytes_per_s".to_string()],
                120,
            )
            .unwrap();
        let rx = &response.series[0];
        assert_eq!(rx.points.len(), 1);
    }

    #[test]
    fn disk_smart_values_are_recorded_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        let report = DiskHealthReport {
            smartctl_available: true,
            disks: vec![DiskHealth {
                device: "/dev/sda".into(),
                smart: SmartInfo {
                    available: true,
                    temperature_c: Some(41),
                    wear_percent: Some(7),
                    ..Default::default()
                },
                zfs: None,
            }],
            ..Default::default()
        };
        store.ingest(0, &sample(), Some(&report)).unwrap();
        let response = store
            .query(
                ReportRange::Hours24,
                &[
                    "disk.sda.temperature_c".to_string(),
                    "disk.sda.wear_percent".to_string(),
                ],
                0,
            )
            .unwrap();
        assert_eq!(response.series.len(), 2);
        assert_eq!(response.series[0].points[0].3, 41.0);
    }

    #[test]
    fn unsupported_series_are_ignored_and_requests_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        store.ingest(0, &sample(), None).unwrap();
        let requested: Vec<String> = (0..MAX_QUERY_SERIES + 3)
            .map(|index| format!("nope.{index}"))
            .collect();
        let response = store.query(ReportRange::Hours24, &requested, 60).unwrap();
        assert!(response.series.is_empty());
        assert_eq!(response.ignored.len(), requested.len());

        let mut keys = vec!["cpu.usage_percent".to_string(); MAX_QUERY_SERIES + 2];
        keys.push("does.not.exist".to_string());
        let response = store.query(ReportRange::Hours24, &keys, 60).unwrap();
        assert_eq!(response.series.len(), 1);
        assert!(response.ignored.len() >= 2);
    }

    #[test]
    fn query_downsampling_caps_points() {
        let points: Vec<(i64, Sample)> = (0..3_000)
            .map(|index| (index as i64 * 60, Sample::single(index as f64)))
            .collect();
        let down = downsample(points, 3);
        assert_eq!(down.len(), 1_000);
        assert_eq!(down[0].0, 0);
        assert_eq!(down[0].1.count, 3);
        assert_eq!(down[0].1.last, 2.0);
    }

    #[test]
    fn sanitize_component_keeps_allowed_characters_and_bounds_length() {
        assert_eq!(sanitize_component("tank"), "tank");
        assert_eq!(sanitize_component("/dev/sda"), "sda");
        assert_eq!(sanitize_component("my pool/../x"), "my_pool_.._x");
        assert_eq!(sanitize_component("!!!"), "unnamed");
        assert_eq!(sanitize_component(&"a".repeat(100)).len(), 40);
    }

    #[test]
    fn virtual_interfaces_are_filtered() {
        assert!(is_physical_interface("eth0"));
        assert!(is_physical_interface("enp3s0"));
        assert!(is_physical_interface("bond0"));
        assert!(!is_physical_interface("lo"));
        assert!(!is_physical_interface("veth1234"));
        assert!(!is_physical_interface("cni0"));
        assert!(!is_physical_interface("flannel.1"));
    }

    #[test]
    fn ranges_are_an_enum_not_a_duration() {
        assert_eq!(ReportRange::parse("24h"), Some(ReportRange::Hours24));
        assert_eq!(ReportRange::parse("7d"), Some(ReportRange::Days7));
        assert_eq!(ReportRange::parse("30d"), Some(ReportRange::Days30));
        assert_eq!(ReportRange::parse("1h"), None);
        assert_eq!(ReportRange::parse("86400"), None);
    }

    #[test]
    fn a_directory_instead_of_a_file_degrades_to_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter::new(true, dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        let meta = reporter.metadata();
        assert!(meta.enabled);
        assert!(!meta.available);
        assert!(meta.error.is_some());
    }

    #[test]
    fn rewrite_keeps_the_file_readable() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_store(&dir);
        store.ingest(0, &sample(), None).unwrap();
        store.prune(60, false).unwrap();
        assert!(!read_records(&store.path).unwrap().is_empty());
        store.ingest(60, &sample(), None).unwrap();
    }
}
