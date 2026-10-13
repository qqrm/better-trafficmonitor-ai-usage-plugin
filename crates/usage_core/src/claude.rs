//! Claude local-cache adapter. Parsing accepts explicit text and time.
use crate::UsageCoreMetric;
use crate::history::{Sample, metric_if_recent, normalized_metric, unix_now};
use leveldb_core::{Record, parse_log_bytes, parse_table_bytes};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

const CLAUDE_HISTORY_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const CLAUDE_RATE_LIMIT_MAX_AGE: Duration = Duration::from_secs(20 * 60);
const MAX_CLAUDE_LOCAL_STORAGE_FILES: usize = 16;
const MAX_CLAUDE_LOCAL_STORAGE_FILE_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn newest_claude_cache() -> Option<PathBuf> {
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

pub(crate) fn load_claude() -> (UsageCoreMetric, UsageCoreMetric) {
    let Some(path) = newest_claude_cache() else {
        return (UsageCoreMetric::default(), UsageCoreMetric::default());
    };
    let Ok(text) = fs::read_to_string(path) else {
        return (UsageCoreMetric::default(), UsageCoreMetric::default());
    };
    parse_claude_history(&text, unix_now())
}

fn parse_claude_history(text: &str, now: i64) -> (UsageCoreMetric, UsageCoreMetric) {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
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
    (
        metric_if_recent(
            normalized_metric(five_hour, Duration::from_secs(5 * 60 * 60)),
            now,
            CLAUDE_HISTORY_MAX_AGE,
        ),
        metric_if_recent(
            normalized_metric(seven_day, Duration::from_secs(7 * 24 * 60 * 60)),
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

pub(crate) fn load_claude_next_reset(claude_data_dir: &Path) -> i64 {
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

#[cfg(test)]
mod tests {
    use super::*;
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

#[cfg(test)]
mod history_errors {
    use super::*;
    #[test]
    fn malformed_and_stale_history_hide_both_windows() {
        for text in [
            "not json",
            r#"{"samples":[{"t":1000000,"u":{"fh":42,"sd":84}}]}"#,
        ] {
            let (five, seven) = parse_claude_history(text, 10000);
            assert_eq!((five.available, seven.available), (0, 0));
        }
    }
}
