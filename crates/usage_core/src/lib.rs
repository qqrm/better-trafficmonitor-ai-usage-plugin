use chrono::DateTime;
use leveldb_core::{Record, parse_log_bytes, parse_table_bytes};
use serde_json::Value;
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
const MAX_CODEX_FILES: usize = 24;
const MAX_CODEX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_CODEX_FILE_TAIL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CLAUDE_LOCAL_STORAGE_FILES: usize = 16;
const MAX_CLAUDE_LOCAL_STORAGE_FILE_BYTES: u64 = 4 * 1024 * 1024;

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

fn normalized_metric(mut samples: Vec<Sample>, window: Duration) -> UsageCoreMetric {
    samples.retain(|sample| sample.used.is_finite() && (0.0..=100.0).contains(&sample.used));
    samples.sort_by_key(|sample| sample.timestamp);
    samples.dedup_by_key(|sample| sample.timestamp);
    let Some(latest) = samples.last().copied() else {
        return UsageCoreMetric::default();
    };
    let oldest = latest.timestamp.saturating_sub(window.as_secs() as i64);
    samples.retain(|sample| sample.timestamp >= oldest);
    if samples.len() > MAX_HISTORY_POINTS {
        samples.drain(0..samples.len() - MAX_HISTORY_POINTS);
    }

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

fn load_codex_history() -> UsageCoreMetric {
    let Some(sessions) = codex_home().map(|home| home.join("sessions")) else {
        return UsageCoreMetric::default();
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
    codex_metric_from_samples(samples, unix_now())
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
    normalized_metric(vec![sample], CODEX_HISTORY_WINDOW)
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

/// Writes a fixed-size snapshot into caller-owned memory. Claude is read from
/// local application data. On Windows, Codex first asks the installed Codex
/// app-server for an authenticated live snapshot; its fresh local JSONL data is
/// used only if that request is unavailable.
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
    // SAFETY: null was checked above; the ABI requires a writable snapshot.
    unsafe {
        *out = UsageCoreSnapshot {
            claude_5h,
            claude_7d,
            codex_7d,
            claude_next_reset_at_unix_seconds,
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
        assert_eq!(metric.history[0].timestamp_unix_seconds, 1_012);
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
}
