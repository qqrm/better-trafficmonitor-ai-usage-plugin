use chrono::DateTime;
use leveldb_core::{Record, parse_log_bytes, parse_table_bytes};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(windows)]
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::sync::mpsc;
#[cfg(windows)]
use std::thread;

pub const MAX_HISTORY_POINTS: usize = 128;
// Claude Desktop normally appends usage samples every 15 minutes, but may skip
// individual polls. Do not hide both Claude windows after one missed update.
const CLAUDE_HISTORY_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const CLAUDE_RATE_LIMIT_MAX_AGE: Duration = Duration::from_secs(20 * 60);
const CODEX_RATE_LIMIT_MAX_AGE: Duration = Duration::from_secs(20 * 60);
const CODEX_HISTORY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_PERSISTED_CODEX_HISTORY_POINTS: usize = 1024;
const MAX_CODEX_FILES: usize = 24;
const MAX_CODEX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_CODEX_FILE_TAIL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CLAUDE_LOCAL_STORAGE_FILES: usize = 16;
const MAX_CLAUDE_LOCAL_STORAGE_FILE_BYTES: u64 = 4 * 1024 * 1024;
// The ZCode (z.ai coding plan) taskbar display always reflects the latest
// server response from the quota endpoint. The local sample store keeps the
// burn-down graphs filled and preserves the last known values while a request
// fails; locally computed percentages are never substituted.
const ZCODE_FIVE_HOUR_WINDOW: Duration = Duration::from_secs(5 * 60 * 60);
const ZCODE_WEEKLY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const ZCODE_FRESH_MAX_AGE: Duration = Duration::from_secs(30 * 60);
const ZCODE_WEEKLY_FRESH_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);
const ZCODE_MAX_PERSISTED_HISTORY_POINTS: usize = 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsageCoreHistoryPoint {
    pub timestamp_unix_seconds: i64,
    pub used_percentage: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsageCoreMetric {
    pub available: u8,
    pub _padding: [u8; 7],
    pub used_percentage: f64,
    pub reset_at_unix_seconds: i64,
    pub history_len: u32,
    pub _reserved: u32,
    pub history: [UsageCoreHistoryPoint; MAX_HISTORY_POINTS],
}

impl Default for UsageCoreMetric {
    fn default() -> Self {
        Self {
            available: 0,
            _padding: [0; 7],
            used_percentage: 0.0,
            reset_at_unix_seconds: 0,
            history_len: 0,
            _reserved: 0,
            history: [UsageCoreHistoryPoint::default(); MAX_HISTORY_POINTS],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsageCoreSnapshot {
    pub claude_5h: UsageCoreMetric,
    pub claude_7d: UsageCoreMetric,
    pub codex_7d: UsageCoreMetric,
    pub claude_next_reset_at_unix_seconds: i64,
    pub zcode_5h: UsageCoreMetric,
    pub zcode_7d: UsageCoreMetric,
    pub zcode_next_reset_at_unix_seconds: i64,
    pub zcode_5h_used_units: i64,
    pub zcode_5h_limit_units: i64,
    pub zcode_7d_used_units: i64,
    pub zcode_7d_limit_units: i64,
}

#[derive(Clone, Copy)]
struct Sample {
    timestamp: i64,
    used: f64,
    reset_at: i64,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn normalized_samples(
    mut samples: Vec<Sample>,
    window: Duration,
    max_points: usize,
) -> Vec<Sample> {
    samples.retain(|sample| sample.used.is_finite() && (0.0..=100.0).contains(&sample.used));
    samples.sort_by_key(|sample| sample.timestamp);
    samples.dedup_by_key(|sample| sample.timestamp);
    let Some(latest) = samples.last() else {
        return Vec::new();
    };
    let oldest = latest.timestamp.saturating_sub(window.as_secs() as i64);
    samples.retain(|sample| sample.timestamp >= oldest);
    if samples.len() <= max_points || max_points <= 1 {
        return samples;
    }

    // Keep the full time window represented. Dropping the oldest points makes
    // a continuously refreshed graph silently shrink from seven days to a few
    // recent hours.
    let last_index = samples.len() - 1;
    (0..max_points)
        .map(|index| samples[index * last_index / (max_points - 1)])
        .collect()
}

fn normalized_metric(samples: Vec<Sample>, window: Duration) -> UsageCoreMetric {
    let samples = normalized_samples(samples, window, MAX_HISTORY_POINTS);
    let Some(latest) = samples.last().copied() else {
        return UsageCoreMetric::default();
    };

    let mut metric = UsageCoreMetric {
        available: 1,
        used_percentage: latest.used,
        reset_at_unix_seconds: latest.reset_at,
        history_len: samples.len() as u32,
        ..UsageCoreMetric::default()
    };
    for (slot, sample) in metric.history.iter_mut().zip(samples) {
        *slot = UsageCoreHistoryPoint {
            timestamp_unix_seconds: sample.timestamp,
            used_percentage: sample.used,
        };
    }
    metric
}

fn metric_if_recent(metric: UsageCoreMetric, now: i64, max_age: Duration) -> UsageCoreMetric {
    let history_len = usize::min(metric.history_len as usize, MAX_HISTORY_POINTS);
    let Some(latest) = metric.history[..history_len].last() else {
        return UsageCoreMetric::default();
    };
    if latest.timestamp_unix_seconds <= 0
        || latest.timestamp_unix_seconds > now.saturating_add(max_age.as_secs() as i64)
        || now.saturating_sub(latest.timestamp_unix_seconds) > max_age.as_secs() as i64
    {
        return UsageCoreMetric::default();
    }
    metric
}

fn newest_claude_cache() -> Option<PathBuf> {
    let packages = PathBuf::from(env::var_os("LOCALAPPDATA")?).join("Packages");
    fs::read_dir(packages)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("Claude_"))
        .map(|entry| {
            entry
                .path()
                .join("LocalCache/Roaming/Claude/plan-usage-history.json")
        })
        .filter_map(|path| fs::metadata(&path).ok().map(|metadata| (path, metadata)))
        .max_by_key(|(_, metadata)| metadata.modified().ok())
        .map(|(path, _)| path)
}

fn load_claude() -> (UsageCoreMetric, UsageCoreMetric) {
    let Some(path) = newest_claude_cache() else {
        return (UsageCoreMetric::default(), UsageCoreMetric::default());
    };
    let Ok(text) = fs::read_to_string(path) else {
        return (UsageCoreMetric::default(), UsageCoreMetric::default());
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return (UsageCoreMetric::default(), UsageCoreMetric::default());
    };
    let mut five_hour = Vec::new();
    let mut seven_day = Vec::new();
    for sample in root
        .get("samples")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let timestamp = sample.get("t").and_then(Value::as_i64).unwrap_or_default() / 1000;
        let usage = sample.get("u").and_then(Value::as_object);
        if timestamp <= 0 {
            continue;
        }
        if let Some(used) = usage
            .and_then(|usage| usage.get("fh"))
            .and_then(Value::as_f64)
        {
            five_hour.push(Sample {
                timestamp,
                used,
                reset_at: 0,
            });
        }
        if let Some(used) = usage
            .and_then(|usage| usage.get("sd"))
            .and_then(Value::as_f64)
        {
            seven_day.push(Sample {
                timestamp,
                used,
                reset_at: 0,
            });
        }
    }
    let now = unix_now();
    (
        metric_if_recent(
            normalized_metric(five_hour, Duration::from_secs(5 * 60 * 60)),
            now,
            CLAUDE_HISTORY_MAX_AGE,
        ),
        metric_if_recent(
            normalized_metric(seven_day, CODEX_HISTORY_WINDOW),
            now,
            CLAUDE_HISTORY_MAX_AGE,
        ),
    )
}

fn claude_reset_from_record(record: &Record, now: i64) -> Option<(u64, i64)> {
    if record.deleted {
        return None;
    }
    let json_start = record.value.iter().position(|byte| *byte == b'{')?;
    let value = serde_json::from_slice::<Value>(&record.value[json_start..]).ok()?;
    let reset_at = value.get("resetsAt")?.as_i64()?;
    let observed_at = value.get("observedAt")?.as_f64()? as i64;
    value.get("utilization")?.as_f64()?;
    if reset_at <= now
        || observed_at <= 0
        || now.saturating_sub(observed_at) > CLAUDE_RATE_LIMIT_MAX_AGE.as_secs() as i64
    {
        return None;
    }
    Some((record.seq, reset_at))
}

fn load_claude_next_reset(claude_data_dir: &Path) -> i64 {
    let local_storage = claude_data_dir.join("Local Storage/leveldb");
    let Ok(entries) = fs::read_dir(local_storage) else {
        return 0;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension()?.to_str()?.to_ascii_lowercase();
            if extension != "ldb" && extension != "sst" && extension != "log" {
                return None;
            }
            let metadata = fs::metadata(&path).ok()?;
            (metadata.len() <= MAX_CLAUDE_LOCAL_STORAGE_FILE_BYTES).then_some(path)
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    files.reverse();
    files.truncate(MAX_CLAUDE_LOCAL_STORAGE_FILES);

    let now = unix_now();
    let mut newest = None;
    for path in files {
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        let records = match path.extension().and_then(|extension| extension.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("log") => {
                parse_log_bytes(&data, &path).ok()
            }
            _ => parse_table_bytes(&data, &path).ok(),
        };
        for record in records.into_iter().flatten() {
            if let Some(candidate) = claude_reset_from_record(&record, now) {
                if newest.is_none_or(|current: (u64, i64)| candidate.0 > current.0) {
                    newest = Some(candidate);
                }
            }
        }
    }
    newest.map(|(_, reset_at)| reset_at).unwrap_or_default()
}

fn codex_home() -> Option<PathBuf> {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".codex")))
}

fn collect_jsonl(root: &Path, files: &mut Vec<PathBuf>, entries_seen: &mut usize) {
    if *entries_seen >= MAX_CODEX_DIRECTORY_ENTRIES {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        *entries_seen += 1;
        if *entries_seen >= MAX_CODEX_DIRECTORY_ENTRIES {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, files, entries_seen);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            files.push(path);
        }
    }
}

fn parse_rfc3339_seconds(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_str)
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp())
        .unwrap_or_default()
}

fn find_rate_limits(value: &Value) -> Option<&Value> {
    if let Some(found) = value.get("rate_limits") {
        return Some(found);
    }
    match value {
        Value::Object(object) => object.values().find_map(find_rate_limits),
        Value::Array(values) => values.iter().find_map(find_rate_limits),
        _ => None,
    }
}

fn extract_weekly(rate_limits: &Value, timestamp: i64) -> Option<Sample> {
    for key in ["primary", "secondary"] {
        let Some(metric) = rate_limits.get(key) else {
            continue;
        };
        let minutes = metric
            .get("window_minutes")
            .or_else(|| metric.get("windowDurationMins"))
            .and_then(Value::as_i64)?;
        if !(6 * 24 * 60..=8 * 24 * 60).contains(&minutes) {
            continue;
        }
        let used = metric
            .get("used_percent")
            .or_else(|| metric.get("usedPercent"))
            .and_then(Value::as_f64)
            .or_else(|| {
                metric
                    .get("remaining_percent")
                    .and_then(Value::as_f64)
                    .map(|remaining| 100.0 - remaining)
            })?;
        return Some(Sample {
            timestamp,
            used,
            // Codex persists this as a Unix timestamp named `resets_at`.
            // Retain `reset_at` as a compatibility fallback for older records.
            reset_at: metric
                .get("resets_at")
                .or_else(|| metric.get("resetsAt"))
                .and_then(Value::as_i64)
                .unwrap_or_else(|| parse_rfc3339_seconds(metric.get("reset_at"))),
        });
    }
    None
}

fn read_file_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    if size > MAX_CODEX_FILE_TAIL_BYTES {
        file.seek(SeekFrom::Start(size - MAX_CODEX_FILE_TAIL_BYTES))
            .ok()?;
    }
    let mut data = String::new();
    file.read_to_string(&mut data).ok()?;
    Some(data)
}

fn codex_metric_from_samples(samples: Vec<Sample>, now: i64) -> UsageCoreMetric {
    metric_if_recent(
        normalized_metric(samples, CODEX_HISTORY_WINDOW),
        now,
        CODEX_RATE_LIMIT_MAX_AGE,
    )
}

fn parse_persisted_codex_history(data: &str) -> Vec<Sample> {
    let Ok(root) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    root.get("samples")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|sample| {
            Some(Sample {
                timestamp: sample.get("timestamp")?.as_i64()?,
                used: sample.get("used")?.as_f64()?,
                reset_at: sample.get("reset_at")?.as_i64()?,
            })
        })
        .collect()
}

fn persisted_codex_history_json(samples: &[Sample]) -> String {
    let samples: Vec<Value> = samples
        .iter()
        .map(|sample| {
            serde_json::json!({
                "timestamp": sample.timestamp,
                "used": sample.used,
                "reset_at": sample.reset_at,
            })
        })
        .collect();
    serde_json::json!({ "version": 1, "samples": samples }).to_string()
}

#[cfg(windows)]
fn persisted_codex_history_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(env::var_os("LOCALAPPDATA")?)
            .join("BetterTrafficMonitorAiUsage")
            .join("codex-history.json"),
    )
}

#[cfg(windows)]
fn load_persisted_codex_history() -> Vec<Sample> {
    let Some(path) = persisted_codex_history_path() else {
        return Vec::new();
    };
    fs::read_to_string(path)
        .ok()
        .map(|data| parse_persisted_codex_history(&data))
        .unwrap_or_default()
}

#[cfg(windows)]
fn save_persisted_codex_history(samples: &[Sample]) {
    let Some(path) = persisted_codex_history_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension("json.tmp");
    if fs::write(&temporary, persisted_codex_history_json(samples)).is_err() {
        return;
    }
    if fs::rename(&temporary, &path).is_err() {
        let _ = fs::remove_file(&path);
        let _ = fs::rename(temporary, path);
    }
}

fn load_codex_history_samples() -> Vec<Sample> {
    let Some(sessions) = codex_home().map(|home| home.join("sessions")) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    collect_jsonl(&sessions, &mut files, &mut 0);
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    files.reverse();
    files.truncate(MAX_CODEX_FILES);
    let mut samples = Vec::new();
    for path in files {
        let Some(data) = read_file_tail(&path) else {
            continue;
        };
        for line in data.lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let timestamp = parse_rfc3339_seconds(event.get("timestamp"));
            if timestamp <= 0 {
                continue;
            }
            if let Some(rate_limits) = find_rate_limits(&event) {
                if let Some(sample) = extract_weekly(rate_limits, timestamp) {
                    samples.push(sample);
                }
            }
        }
    }
    samples
}

fn load_codex_history() -> UsageCoreMetric {
    #[cfg(windows)]
    {
        let mut samples = load_codex_history_samples();
        samples.extend(load_persisted_codex_history());
        return codex_metric_from_samples(samples, unix_now());
    }
    #[cfg(not(windows))]
    codex_metric_from_samples(load_codex_history_samples(), unix_now())
}

fn codex_history_with_live_sample(mut history: Vec<Sample>, live: Sample) -> Vec<Sample> {
    // Live account data is authoritative for the displayed percentage and
    // reset. Keep only older session samples so it is necessarily the final
    // point, including when a session event shares its one-second timestamp.
    history.retain(|sample| sample.timestamp < live.timestamp);
    history.push(live);
    normalized_samples(
        history,
        CODEX_HISTORY_WINDOW,
        MAX_PERSISTED_CODEX_HISTORY_POINTS,
    )
}

#[cfg(windows)]
fn newest_codex_executable() -> Option<PathBuf> {
    let root = PathBuf::from(env::var_os("LOCALAPPDATA")?).join("OpenAI/Codex/bin");
    fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("codex.exe"))
        .filter(|path| path.is_file())
        .max_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        })
}

#[cfg(windows)]
fn request_codex_rate_limits(executable: PathBuf) -> Option<Value> {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new(executable);
    command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Prevent a console window when TrafficMonitor refreshes the plug-in.
        .creation_flags(0x0800_0000);
    let mut child = command.spawn().ok()?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let mut write_succeeded = true;
    for request in [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "better-trafficmonitor-ai-usage-plugin", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": {}
            }
        }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": null }),
    ] {
        if writeln!(stdin, "{request}").is_err() {
            write_succeeded = false;
            break;
        }
    }
    if write_succeeded {
        write_succeeded = stdin.flush().is_ok();
    }
    if !write_succeeded {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut response = None;
        for line in BufReader::new(stdout).lines().take(64) {
            let Ok(line) = line else { break };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id").and_then(Value::as_i64) == Some(2) {
                response = message.get("result").cloned();
                break;
            }
        }
        let _ = sender.send(response);
    });
    let response = receiver.recv_timeout(Duration::from_secs(3)).ok().flatten();
    let _ = child.kill();
    let _ = child.wait();
    response
}

#[cfg(windows)]
fn load_codex_live() -> UsageCoreMetric {
    let Some(executable) = newest_codex_executable() else {
        return UsageCoreMetric::default();
    };
    let Some(response) = request_codex_rate_limits(executable) else {
        return UsageCoreMetric::default();
    };
    let rate_limits = response
        .get("rateLimitsByLimitId")
        .and_then(|limits| limits.get("codex"))
        .or_else(|| response.get("rateLimits"));
    let Some(sample) = rate_limits.and_then(|limits| extract_weekly(limits, unix_now())) else {
        return UsageCoreMetric::default();
    };
    let mut history = load_codex_history_samples();
    history.extend(load_persisted_codex_history());
    let history = codex_history_with_live_sample(history, sample);
    save_persisted_codex_history(&history);
    normalized_metric(history, CODEX_HISTORY_WINDOW)
}

fn load_codex() -> UsageCoreMetric {
    #[cfg(windows)]
    {
        let live = load_codex_live();
        if live.available != 0 {
            return live;
        }
    }
    load_codex_history()
}

fn zcode_home() -> Option<PathBuf> {
    env::var_os("ZCODE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".zcode")))
}


struct ZCodeUsage {
    five_hour: UsageCoreMetric,
    seven_day: UsageCoreMetric,
    next_reset_at: i64,
    five_hour_used_units: i64,
    five_hour_limit_units: i64,
    seven_day_used_units: i64,
    seven_day_limit_units: i64,
}

impl Default for ZCodeUsage {
    fn default() -> Self {
        Self {
            five_hour: UsageCoreMetric::default(),
            seven_day: UsageCoreMetric::default(),
            next_reset_at: 0,
            five_hour_used_units: 0,
            five_hour_limit_units: 0,
            seven_day_used_units: 0,
            seven_day_limit_units: 0,
        }
    }
}

// The z.ai coding plan reports server-side credit pools per rolling window.
#[derive(Clone, Copy)]
struct ZCodeLiveLimit {
    percentage: f64,
    used_units: i64,
    limit_units: i64,
    reset_at_unix_seconds: i64,
}

struct ZCodeLiveQuota {
    five_hour: ZCodeLiveLimit,
    week: ZCodeLiveLimit,
}

fn read_live_limit(value: &Value) -> Option<ZCodeLiveLimit> {
    Some(ZCodeLiveLimit {
        percentage: value.get("percentage")?.as_f64()?,
        used_units: value.get("currentValue")?.as_i64()?,
        limit_units: value.get("usage")?.as_i64()?,
        reset_at_unix_seconds: value.get("nextResetTime")?.as_i64()? / 1000,
    })
}

fn parse_zcode_quota(data: &str) -> Option<ZCodeLiveQuota> {
    let root = serde_json::from_str::<Value>(data).ok()?;
    let limits = root.get("data")?.get("limits")?.as_array()?;
    // unit 3 counts hours and number 5 selects the five-hour pool; unit 6
    // spans one week. Fall back to the two smallest/largest pools when the
    // server changes its units.
    let mut parsed: Vec<(Value, ZCodeLiveLimit)> = limits
        .iter()
        .filter_map(|value| read_live_limit(value).map(|limit| (value.clone(), limit)))
        .collect();
    if parsed.is_empty() {
        return None;
    }
    parsed.sort_by_key(|(_, limit)| limit.limit_units);
    let five_hour = parsed
        .iter()
        .find(|(value, _)| value.get("number").and_then(Value::as_i64) == Some(5))
        .map(|(_, limit)| *limit)
        .unwrap_or(parsed[0].1);
    let week = parsed
        .iter()
        .rev()
        .find(|(value, _)| value.get("number").and_then(Value::as_i64) != Some(5))
        .map(|(_, limit)| *limit)?;
    Some(ZCodeLiveQuota { five_hour, week })
}

fn zcode_api_key() -> Option<String> {
    let config = zcode_home()?.join("v2").join("config.json");
    let text = fs::read_to_string(config).ok()?;
    let root = serde_json::from_str::<Value>(&text).ok()?;
    for provider in ["builtin:zai-coding-plan", "builtin:zai-start-plan"] {
        let key = root
            .get("provider")?
            .get(provider)?
            .get("options")?
            .get("apiKey")?
            .as_str()?
            .trim();
        if !key.is_empty() {
            return Some(key.to_owned());
        }
    }
    None
}

const ZCODE_QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";

#[cfg(windows)]
fn fetch_zcode_live_quota() -> Option<ZCodeLiveQuota> {
    let key = zcode_api_key()?;
    let response = ureq::get(ZCODE_QUOTA_URL)
        .timeout(Duration::from_secs(4))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .call()
        .ok()?;
    let text = response.into_string().ok()?;
    parse_zcode_quota(&text)
}

fn parse_persisted_zcode_samples(value: Option<&Value>) -> Vec<Sample> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|sample| {
            Some(Sample {
                timestamp: sample.get("timestamp")?.as_i64()?,
                used: sample.get("used")?.as_f64()?,
                reset_at: sample.get("reset_at").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .collect()
}

// Units of the most recent server response, stored beside the samples so a
// missed request does not blank the tooltip.
#[derive(Clone, Copy, Default)]
struct ZCodeUnits {
    five_hour_used: i64,
    five_hour_limit: i64,
    week_used: i64,
    week_limit: i64,
}

fn parse_persisted_zcode_history(data: &str) -> (Vec<Sample>, Vec<Sample>, ZCodeUnits) {
    let Ok(root) = serde_json::from_str::<Value>(data) else {
        return (Vec::new(), Vec::new(), ZCodeUnits::default());
    };
    let units = root.get("latest").unwrap_or(&Value::Null);
    let read = |key: &str| units.get(key).and_then(Value::as_i64).unwrap_or(0);
    (
        parse_persisted_zcode_samples(root.get("five_hour")),
        parse_persisted_zcode_samples(root.get("seven_day")),
        ZCodeUnits {
            five_hour_used: read("five_hour_used"),
            five_hour_limit: read("five_hour_limit"),
            week_used: read("week_used"),
            week_limit: read("week_limit"),
        },
    )
}

fn persisted_zcode_history_json(
    five_hour: &[Sample],
    seven_day: &[Sample],
    units: ZCodeUnits,
) -> String {
    serde_json::json!({
        "version": 2,
        "five_hour": persisted_zcode_samples_json(five_hour),
        "seven_day": persisted_zcode_samples_json(seven_day),
        "latest": {
            "five_hour_used": units.five_hour_used,
            "five_hour_limit": units.five_hour_limit,
            "week_used": units.week_used,
            "week_limit": units.week_limit,
        },
    })
    .to_string()
}

fn persisted_zcode_samples_json(samples: &[Sample]) -> Value {
    Value::Array(
        samples
            .iter()
            .map(|sample| {
                serde_json::json!({
                    "timestamp": sample.timestamp,
                    "used": sample.used,
                    "reset_at": sample.reset_at,
                })
            })
            .collect(),
    )
}

// Freshly computed samples are authoritative; retained persisted samples only
// fill the gaps ZCode may have pruned from its own records.
fn merge_zcode_samples(
    persisted: Vec<Sample>,
    computed: Vec<Sample>,
    window: Duration,
    max_points: usize,
) -> Vec<Sample> {
    let Some(newest) = computed.last().map(|sample| sample.timestamp) else {
        return Vec::new();
    };
    let oldest = newest.saturating_sub(window.as_secs() as i64);
    let mut merged: BTreeMap<i64, Sample> = persisted
        .into_iter()
        .filter(|sample| sample.timestamp > oldest && sample.timestamp <= newest)
        .map(|sample| (sample.timestamp, sample))
        .collect();
    for sample in computed {
        merged.insert(sample.timestamp, sample);
    }
    let mut samples: Vec<Sample> = merged.into_values().collect();
    if samples.len() > max_points {
        let last_index = samples.len() - 1;
        samples = (0..max_points)
            .map(|index| samples[index * last_index / (max_points - 1)])
            .collect();
    }
    samples
}

#[cfg(windows)]
fn persisted_zcode_history_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(env::var_os("LOCALAPPDATA")?)
            .join("BetterTrafficMonitorAiUsage")
            .join("zcode-history.json"),
    )
}

#[cfg(windows)]
fn load_persisted_zcode_history() -> (Vec<Sample>, Vec<Sample>, ZCodeUnits) {
    persisted_zcode_history_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|data| parse_persisted_zcode_history(&data))
        .unwrap_or_default()
}

#[cfg(windows)]
fn save_persisted_zcode_history(five_hour: &[Sample], seven_day: &[Sample], units: ZCodeUnits) {
    let Some(path) = persisted_zcode_history_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension("json.tmp");
    if fs::write(&temporary, persisted_zcode_history_json(five_hour, seven_day, units)).is_err() {
        return;
    }
    if fs::rename(&temporary, &path).is_err() {
        let _ = fs::remove_file(&path);
        let _ = fs::rename(temporary, path);
    }
}

fn load_zcode() -> ZCodeUsage {
    let now = unix_now();

    // The latest z.ai response is the only display source. When a request
    // fails, the last stored response stays on display until it goes stale.
    #[cfg(windows)]
    if let Some(quota) = fetch_zcode_live_quota() {
        let five_hour_sample = Sample {
            timestamp: now,
            used: quota.five_hour.percentage,
            reset_at: quota.five_hour.reset_at_unix_seconds,
        };
        let seven_day_sample = Sample {
            timestamp: now,
            used: quota.week.percentage,
            reset_at: quota.week.reset_at_unix_seconds,
        };
        let (persisted_five_hour, persisted_seven_day, _) = load_persisted_zcode_history();
        let five_hour_samples = merge_zcode_samples(
            persisted_five_hour,
            vec![five_hour_sample],
            ZCODE_FIVE_HOUR_WINDOW,
            ZCODE_MAX_PERSISTED_HISTORY_POINTS,
        );
        let seven_day_samples = merge_zcode_samples(
            persisted_seven_day,
            vec![seven_day_sample],
            ZCODE_WEEKLY_WINDOW,
            ZCODE_MAX_PERSISTED_HISTORY_POINTS,
        );
        let units = ZCodeUnits {
            five_hour_used: quota.five_hour.used_units,
            five_hour_limit: quota.five_hour.limit_units,
            week_used: quota.week.used_units,
            week_limit: quota.week.limit_units,
        };
        save_persisted_zcode_history(&five_hour_samples, &seven_day_samples, units);
        return ZCodeUsage {
            five_hour: metric_if_recent(
                normalized_metric(five_hour_samples, ZCODE_FIVE_HOUR_WINDOW),
                now,
                ZCODE_FRESH_MAX_AGE,
            ),
            seven_day: metric_if_recent(
                normalized_metric(seven_day_samples, ZCODE_WEEKLY_WINDOW),
                now,
                ZCODE_WEEKLY_FRESH_MAX_AGE,
            ),
            next_reset_at: quota.five_hour.reset_at_unix_seconds,
            five_hour_used_units: units.five_hour_used,
            five_hour_limit_units: units.five_hour_limit,
            seven_day_used_units: units.week_used,
            seven_day_limit_units: units.week_limit,
        };
    }

    // No fresh response this tick: keep showing the latest one we have.
    #[cfg(windows)]
    {
        let (five_hour_samples, seven_day_samples, units) = load_persisted_zcode_history();
        let next_reset_at = five_hour_samples
            .last()
            .map(|sample| sample.reset_at)
            .unwrap_or_default();
        return ZCodeUsage {
            five_hour: metric_if_recent(
                normalized_metric(five_hour_samples, ZCODE_FIVE_HOUR_WINDOW),
                now,
                ZCODE_FRESH_MAX_AGE,
            ),
            seven_day: metric_if_recent(
                normalized_metric(seven_day_samples, ZCODE_WEEKLY_WINDOW),
                now,
                ZCODE_WEEKLY_FRESH_MAX_AGE,
            ),
            next_reset_at,
            five_hour_used_units: units.five_hour_used,
            five_hour_limit_units: units.five_hour_limit,
            seven_day_used_units: units.week_used,
            seven_day_limit_units: units.week_limit,
        };
    }
    #[cfg(not(windows))]
    ZCodeUsage::default()
}

/// Writes a fixed-size snapshot into caller-owned memory. Claude is read from
/// local application data. On Windows, Codex first asks the installed Codex
/// app-server for an authenticated live snapshot; its fresh local JSONL data is
/// used only if that request is unavailable. ZCode usage comes from the
/// latest z.ai quota response, kept locally between requests.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usage_core_refresh(out: *mut UsageCoreSnapshot) -> i32 {
    if out.is_null() {
        return 0;
    }
    let (claude_5h, claude_7d) = load_claude();
    let claude_next_reset_at_unix_seconds = newest_claude_cache()
        .and_then(|path| path.parent().map(load_claude_next_reset))
        .unwrap_or_default();
    let codex_7d = load_codex();
    let zcode = load_zcode();
    // SAFETY: null was checked above; the ABI requires a writable snapshot.
    unsafe {
        *out = UsageCoreSnapshot {
            claude_5h,
            claude_7d,
            codex_7d,
            claude_next_reset_at_unix_seconds,
            zcode_5h: zcode.five_hour,
            zcode_7d: zcode.seven_day,
            zcode_next_reset_at_unix_seconds: zcode.next_reset_at,
            zcode_5h_used_units: zcode.five_hour_used_units,
            zcode_5h_limit_units: zcode.five_hour_limit_units,
            zcode_7d_used_units: zcode.seven_day_used_units,
            zcode_7d_limit_units: zcode.seven_day_limit_units,
        };
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalized_metric_keeps_the_latest_window_and_caps_history() {
        let samples = (0..140)
            .map(|index| Sample {
                timestamp: 1_000 + index,
                used: index as f64 / 2.0,
                reset_at: 9_999,
            })
            .collect();

        let metric = normalized_metric(samples, Duration::from_secs(10_000));

        assert_eq!(metric.available, 1);
        assert_eq!(metric.history_len as usize, MAX_HISTORY_POINTS);
        assert_eq!(metric.used_percentage, 69.5);
        assert_eq!(metric.history[0].timestamp_unix_seconds, 1_000);
        assert_eq!(
            metric.history[MAX_HISTORY_POINTS - 1].timestamp_unix_seconds,
            1_139
        );
    }

    #[test]
    fn claude_history_tolerates_missed_poll_but_not_stale_data() {
        let metric = normalized_metric(
            vec![Sample {
                timestamp: 1_000,
                used: 42.0,
                reset_at: 0,
            }],
            Duration::from_secs(5 * 60 * 60),
        );

        assert_eq!(
            metric_if_recent(metric, 1_000 + 45 * 60, CLAUDE_HISTORY_MAX_AGE).available,
            1
        );
        assert_eq!(
            metric_if_recent(metric, 1_000 + 60 * 60 + 1, CLAUDE_HISTORY_MAX_AGE).available,
            0
        );
    }

    #[test]
    fn codex_uses_only_the_latest_fresh_rate_limit_update() {
        let now = 2_000_000;
        let metric = codex_metric_from_samples(
            vec![
                Sample {
                    timestamp: now - 25 * 60,
                    used: 96.0,
                    reset_at: now + 74 * 24 * 60 * 60,
                },
                Sample {
                    timestamp: now - 5 * 60,
                    used: 18.0,
                    reset_at: now + 6 * 24 * 60 * 60,
                },
            ],
            now,
        );

        assert_eq!(metric.available, 1);
        assert_eq!(metric.used_percentage, 18.0);
        assert_eq!(metric.reset_at_unix_seconds, now + 6 * 24 * 60 * 60);
    }

    #[test]
    fn live_codex_sample_keeps_older_session_history_for_the_graph() {
        let live_timestamp = 2_000_000;
        let metric = normalized_metric(
            codex_history_with_live_sample(
                vec![
                    Sample {
                        timestamp: live_timestamp - 2 * 60 * 60,
                        used: 9.0,
                        reset_at: live_timestamp + 6 * 24 * 60 * 60,
                    },
                    Sample {
                        timestamp: live_timestamp,
                        used: 99.0,
                        reset_at: live_timestamp + 6 * 24 * 60 * 60,
                    },
                ],
                Sample {
                    timestamp: live_timestamp,
                    used: 18.0,
                    reset_at: live_timestamp + 5 * 24 * 60 * 60,
                },
            ),
            CODEX_HISTORY_WINDOW,
        );

        assert_eq!(metric.available, 1);
        assert_eq!(metric.history_len, 2);
        assert_eq!(metric.history[0].used_percentage, 9.0);
        assert_eq!(metric.history[1].used_percentage, 18.0);
        assert_eq!(metric.used_percentage, 18.0);
        assert_eq!(
            metric.reset_at_unix_seconds,
            live_timestamp + 5 * 24 * 60 * 60
        );
    }

    #[test]
    fn persisted_codex_history_keeps_a_seven_day_span_when_refreshed_often() {
        let now = 2_000_000;
        let samples = (0..20_000)
            .map(|index| Sample {
                timestamp: now - CODEX_HISTORY_WINDOW.as_secs() as i64 + index * 30,
                used: (index % 100) as f64,
                reset_at: now + 6 * 24 * 60 * 60,
            })
            .collect();

        let history = codex_history_with_live_sample(
            samples,
            Sample {
                timestamp: now,
                used: 18.0,
                reset_at: now + 5 * 24 * 60 * 60,
            },
        );

        assert_eq!(history.len(), MAX_PERSISTED_CODEX_HISTORY_POINTS);
        assert_eq!(
            history.first().expect("oldest sample").timestamp,
            now - CODEX_HISTORY_WINDOW.as_secs() as i64
        );
        assert_eq!(history.last().expect("live sample").used, 18.0);
    }

    #[test]
    fn persisted_codex_history_round_trips() {
        let samples = vec![Sample {
            timestamp: 123,
            used: 18.0,
            reset_at: 456,
        }];

        let parsed = parse_persisted_codex_history(&persisted_codex_history_json(&samples));

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].timestamp, 123);
        assert_eq!(parsed[0].used, 18.0);
        assert_eq!(parsed[0].reset_at, 456);
    }

    #[test]
    fn codex_hides_rate_limit_after_no_recent_update() {
        let now = 2_000_000;
        let metric = codex_metric_from_samples(
            vec![Sample {
                timestamp: now - CODEX_RATE_LIMIT_MAX_AGE.as_secs() as i64 - 1,
                used: 96.0,
                reset_at: now + 74 * 24 * 60 * 60,
            }],
            now,
        );

        assert_eq!(metric.available, 0);
        assert_eq!(metric.history_len, 0);
        assert_eq!(metric.reset_at_unix_seconds, 0);
    }

    #[test]
    fn weekly_rate_limit_can_come_from_secondary_only() {
        let limits = json!({
            "secondary": {
                "window_minutes": 10_080,
                "remaining_percent": 37.0,
                "reset_at": "2026-08-30T00:00:00Z"
            }
        });

        let sample = extract_weekly(&limits, 123).expect("weekly metric");

        assert_eq!(sample.timestamp, 123);
        assert_eq!(sample.used, 63.0);
        assert!(sample.reset_at > 0);
    }

    #[test]
    fn weekly_rate_limit_reads_codex_unix_reset_timestamp() {
        let limits = json!({
            "primary": {
                "window_minutes": 10_080,
                "used_percent": 15.0,
                "resets_at": 1_788_272_240_i64
            }
        });

        let sample = extract_weekly(&limits, 123).expect("weekly metric");

        assert_eq!(sample.reset_at, 1_788_272_240);
    }

    #[test]
    fn weekly_rate_limit_reads_live_codex_app_server_format() {
        let limits = json!({
            "primary": {
                "windowDurationMins": 10_080,
                "usedPercent": 18,
                "resetsAt": 1_788_272_240_i64
            }
        });

        let sample = extract_weekly(&limits, 123).expect("weekly metric");

        assert_eq!(sample.used, 18.0);
        assert_eq!(sample.reset_at, 1_788_272_240);
    }

    #[test]
    fn non_weekly_rate_limit_is_ignored() {
        let limits = json!({
            "primary": {
                "window_minutes": 300,
                "used_percent": 12.0
            }
        });

        assert!(extract_weekly(&limits, 123).is_none());
    }

    #[test]
    fn claude_reset_requires_fresh_usage_state() {
        let record = Record {
            key: Vec::new(),
            value: b"\x01{\"resetsAt\":2000,\"utilization\":0.23,\"observedAt\":1500}".to_vec(),
            seq: 42,
            deleted: false,
            origin_file: PathBuf::new(),
        };

        assert_eq!(claude_reset_from_record(&record, 1_600), Some((42, 2_000)));
        assert_eq!(claude_reset_from_record(&record, 3_000), None);
    }


    #[test]
    fn zcode_persisted_history_round_trips_with_units() {
        let json = persisted_zcode_history_json(
            &[Sample { timestamp: 123, used: 18.0, reset_at: 456 }],
            &[Sample { timestamp: 789, used: 24.0, reset_at: 0 }],
            ZCodeUnits {
                five_hour_used: 285,
                five_hour_limit: 12000,
                week_used: 16311,
                week_limit: 60000,
            },
        );
        let (five_hour, seven_day, units) = parse_persisted_zcode_history(&json);

        assert_eq!(five_hour.len(), 1);
        assert_eq!(five_hour[0].timestamp, 123);
        assert_eq!(five_hour[0].reset_at, 456);
        assert_eq!(seven_day.len(), 1);
        assert_eq!(seven_day[0].used, 24.0);
        assert_eq!(units.five_hour_used, 285);
        assert_eq!(units.five_hour_limit, 12000);
        assert_eq!(units.week_used, 16311);
        assert_eq!(units.week_limit, 60000);
    }

    #[test]
    fn zcode_quota_response_maps_windows_and_credits() {
        let data = r#"{"code":200,"msg":"Operation successful","data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":12000,"currentValue":1645,"remaining":10354,"percentage":13,"nextResetTime":1789540647386},
            {"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":60000,"currentValue":13667,"remaining":46332,"percentage":22,"nextResetTime":1790096970983}
        ],"level":"pro"},"success":true}"#;

        let quota = parse_zcode_quota(data).expect("quota");

        assert_eq!(quota.five_hour.used_units, 1645);
        assert_eq!(quota.five_hour.limit_units, 12000);
        assert_eq!(quota.five_hour.percentage, 13.0);
        assert_eq!(quota.five_hour.reset_at_unix_seconds, 1_789_540_647);
        assert_eq!(quota.week.used_units, 13667);
        assert_eq!(quota.week.limit_units, 60000);
        assert_eq!(quota.week.percentage, 22.0);
        assert_eq!(quota.week.reset_at_unix_seconds, 1_790_096_970);
    }

    #[test]
    fn zcode_quota_response_with_unknown_units_falls_back_to_pool_size() {
        let data = r#"{"code":200,"data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":9,"number":4,"usage":60000,"currentValue":1,"remaining":59999,"percentage":1,"nextResetTime":2000000},
            {"type":"CREDIT_LIMIT","unit":9,"number":2,"usage":12000,"currentValue":2,"remaining":11998,"percentage":2,"nextResetTime":3000000}
        ]}}"#;

        let quota = parse_zcode_quota(data).expect("quota");

        // The smaller pool is treated as the five-hour window.
        assert_eq!(quota.five_hour.used_units, 2);
        assert_eq!(quota.week.used_units, 1);
    }

    #[test]
    fn zcode_quota_garbage_is_rejected() {
        assert!(parse_zcode_quota("not json").is_none());
        assert!(parse_zcode_quota(r#"{"code":200,"data":{"limits":[]}}"#).is_none());
    }

    #[test]
    fn zcode_merge_prefers_computed_samples_and_drops_expired_ones() {
        let now = 10_000;
        let merged = merge_zcode_samples(
            vec![
                Sample { timestamp: now - 600, used: 10.0, reset_at: 0 },
                Sample { timestamp: now - 200, used: 20.0, reset_at: 0 },
            ],
            vec![Sample { timestamp: now - 200, used: 40.0, reset_at: 0 }],
            Duration::from_secs(300),
            1024,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].timestamp, now - 200);
        assert_eq!(merged[0].used, 40.0);
    }
}
