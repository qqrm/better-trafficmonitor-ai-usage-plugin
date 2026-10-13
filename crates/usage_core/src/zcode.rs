//! ZCode quota adapter; explicit-clock parsing is independent of HTTP and credentials.
use crate::UsageCoreMetric;
use crate::history::{Sample, unix_now};
#[cfg(windows)]
use crate::history::{metric_if_recent, normalized_metric};
use serde_json::Value;
use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Duration};

const ZCODE_FIVE_HOUR_WINDOW: Duration = Duration::from_secs(5 * 60 * 60);
const ZCODE_WEEKLY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const ZCODE_FRESH_MAX_AGE: Duration = Duration::from_secs(30 * 60);
const ZCODE_WEEKLY_FRESH_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);
const ZCODE_MAX_PERSISTED_HISTORY_POINTS: usize = 1024;
const ZCODE_RESET_MARGIN: Duration = Duration::from_secs(60 * 60);

fn zcode_home() -> Option<PathBuf> {
    env::var_os("ZCODE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".zcode")))
}

pub(crate) struct ZCodeUsage {
    pub(crate) five_hour: UsageCoreMetric,
    pub(crate) seven_day: UsageCoreMetric,
    pub(crate) next_reset_at: i64,
    pub(crate) five_hour_used_units: i64,
    pub(crate) five_hour_limit_units: i64,
    pub(crate) seven_day_used_units: i64,
    pub(crate) seven_day_limit_units: i64,
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

impl ZCodeLiveLimit {
    // A pool cannot reset further out than its own window. z.ai has been seen
    // reporting an anchor days beyond the five-hour horizon, which the plugin
    // then displayed as "6d" next to the 5h percentage; keep the numbers but
    // drop the impossible countdown ("--") instead.
    fn with_bounded_reset(mut self, window: Duration, now: i64) -> Self {
        let horizon = now + (window + ZCODE_RESET_MARGIN).as_secs() as i64;
        if self.reset_at_unix_seconds > horizon {
            self.reset_at_unix_seconds = 0;
        }
        self
    }
}

struct ZCodeLiveQuota {
    five_hour: Option<ZCodeLiveLimit>,
    week: Option<ZCodeLiveLimit>,
}

fn read_live_limit(value: &Value) -> Option<ZCodeLiveLimit> {
    let used_units = value.get("currentValue")?.as_i64()?;
    let limit_units = value.get("usage")?.as_i64()?;
    // z.ai sporadically omits `percentage`; the credit counts are always there.
    let percentage = match value.get("percentage").and_then(Value::as_f64) {
        Some(percentage) => percentage,
        None if limit_units > 0 => used_units as f64 * 100.0 / limit_units as f64,
        None => return None,
    };
    Some(ZCodeLiveLimit {
        percentage,
        used_units,
        limit_units,
        reset_at_unix_seconds: value.get("nextResetTime")?.as_i64()? / 1000,
    })
}

fn parse_zcode_quota(data: &str) -> Option<ZCodeLiveQuota> {
    parse_zcode_quota_at(data, unix_now())
}

fn parse_zcode_quota_at(data: &str, now: i64) -> Option<ZCodeLiveQuota> {
    let root = serde_json::from_str::<Value>(data).ok()?;
    let limits = root.get("data")?.get("limits")?.as_array()?;
    // unit 3 counts hours and number 5 selects the five-hour pool; unit 6
    // spans one week. Each slot takes only its own pool: a missing or
    // malformed five-hour entry must hide that row, not show weekly numbers
    // on it.
    let is_five_hour = |value: &Value| {
        value.get("number").and_then(Value::as_i64) == Some(5)
            || value.get("unit").and_then(Value::as_i64) == Some(3)
    };
    let five_hour = limits
        .iter()
        .filter(|value| is_five_hour(value))
        .filter_map(read_live_limit)
        .map(|limit| limit.with_bounded_reset(ZCODE_FIVE_HOUR_WINDOW, now))
        .max_by_key(|limit| limit.limit_units);
    let week = limits
        .iter()
        .filter(|value| !is_five_hour(value))
        .filter_map(read_live_limit)
        .map(|limit| limit.with_bounded_reset(ZCODE_WEEKLY_WINDOW, now))
        .max_by_key(|limit| limit.limit_units);
    if five_hour.is_none() && week.is_none() {
        return None;
    }
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
    if fs::write(
        &temporary,
        persisted_zcode_history_json(five_hour, seven_day, units),
    )
    .is_err()
    {
        return;
    }
    if fs::rename(&temporary, &path).is_err() {
        let _ = fs::remove_file(&path);
        let _ = fs::rename(temporary, path);
    }
}

pub(crate) fn load_zcode() -> ZCodeUsage {
    #[cfg(windows)]
    let now = unix_now();

    // The latest z.ai response is the only display source. When a request
    // fails, the last stored response stays on display until it goes stale.
    #[cfg(windows)]
    if let Some(quota) = fetch_zcode_live_quota() {
        let (persisted_five_hour, persisted_seven_day, persisted_units) =
            load_persisted_zcode_history();
        // A pool missing from this response keeps the previously stored
        // samples, so its row ages out gracefully instead of jumping to
        // another pool's numbers.
        let five_hour_samples = match quota.five_hour {
            Some(limit) => merge_zcode_samples(
                persisted_five_hour,
                vec![Sample {
                    timestamp: now,
                    used: limit.percentage,
                    reset_at: limit.reset_at_unix_seconds,
                }],
                ZCODE_FIVE_HOUR_WINDOW,
                ZCODE_MAX_PERSISTED_HISTORY_POINTS,
            ),
            None => persisted_five_hour,
        };
        let seven_day_samples = match quota.week {
            Some(limit) => merge_zcode_samples(
                persisted_seven_day,
                vec![Sample {
                    timestamp: now,
                    used: limit.percentage,
                    reset_at: limit.reset_at_unix_seconds,
                }],
                ZCODE_WEEKLY_WINDOW,
                ZCODE_MAX_PERSISTED_HISTORY_POINTS,
            ),
            None => persisted_seven_day,
        };
        let units = ZCodeUnits {
            five_hour_used: quota
                .five_hour
                .map(|limit| limit.used_units)
                .unwrap_or_default(),
            five_hour_limit: quota
                .five_hour
                .map(|limit| limit.limit_units)
                .unwrap_or_default(),
            week_used: quota.week.map(|limit| limit.used_units).unwrap_or_default(),
            week_limit: quota
                .week
                .map(|limit| limit.limit_units)
                .unwrap_or_default(),
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
            next_reset_at: quota
                .five_hour
                .map(|limit| limit.reset_at_unix_seconds)
                .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zcode_persisted_history_round_trips_with_units() {
        let json = persisted_zcode_history_json(
            &[Sample {
                timestamp: 123,
                used: 18.0,
                reset_at: 456,
            }],
            &[Sample {
                timestamp: 789,
                used: 24.0,
                reset_at: 0,
            }],
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

        let five_hour = quota.five_hour.expect("five hour");
        assert_eq!(five_hour.used_units, 1645);
        assert_eq!(five_hour.limit_units, 12000);
        assert_eq!(five_hour.percentage, 13.0);
        assert_eq!(five_hour.reset_at_unix_seconds, 1_789_540_647);
        let week = quota.week.expect("week");
        assert_eq!(week.used_units, 13667);
        assert_eq!(week.limit_units, 60000);
        assert_eq!(week.percentage, 22.0);
        assert_eq!(week.reset_at_unix_seconds, 1_790_096_970);
    }

    #[test]
    fn zcode_quota_response_without_five_hour_pool_leaves_the_slot_empty() {
        let data = r#"{"code":200,"data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":9,"number":4,"usage":60000,"currentValue":1,"remaining":59999,"percentage":1,"nextResetTime":2000000},
            {"type":"CREDIT_LIMIT","unit":9,"number":2,"usage":12000,"currentValue":2,"remaining":11998,"percentage":2,"nextResetTime":3000000}
        ]}}"#;

        let quota = parse_zcode_quota(data).expect("quota");

        // No pool is marked as the five-hour window, so the 5h slot stays
        // empty instead of borrowing another pool's numbers.
        assert!(quota.five_hour.is_none());
        assert_eq!(quota.week.expect("week").used_units, 1);
    }

    #[test]
    fn zcode_missing_five_hour_entry_is_not_replaced_by_the_weekly_pool() {
        let data = r#"{"code":200,"data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":140000,"currentValue":60624,"remaining":79375,"percentage":43,"nextResetTime":1791322367999}
        ]}}"#;

        let quota = parse_zcode_quota(data).expect("quota");

        assert!(quota.five_hour.is_none());
        assert_eq!(quota.week.expect("week").percentage, 43.0);
    }

    #[test]
    fn zcode_reset_beyond_its_own_window_hides_the_countdown() {
        let six_days_out_ms = (unix_now() + 6 * 24 * 60 * 60) * 1000;
        let data = format!(
            r#"{{"code":200,"data":{{"limits":[
                {{"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":28000,"currentValue":0,"remaining":28000,"percentage":0,"nextResetTime":{six_days_out_ms}}},
                {{"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":140000,"currentValue":60624,"remaining":79375,"percentage":43,"nextResetTime":{six_days_out_ms}}}
            ]}}}}"#
        );

        let quota = parse_zcode_quota(&data).expect("quota");

        // Six days is a legal weekly anchor but impossible for the five-hour
        // window: keep the weekly countdown, drop the 5h one.
        assert_eq!(quota.five_hour.expect("five hour").reset_at_unix_seconds, 0);
        assert!(quota.week.expect("week").reset_at_unix_seconds > 0);
    }

    #[test]
    fn zcode_missing_percentage_is_derived_from_credit_counts() {
        let data = r#"{"code":200,"data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":28000,"currentValue":7000,"remaining":21000,"nextResetTime":1790807478487}
        ]}}"#;

        let quota = parse_zcode_quota(data).expect("quota");

        let five_hour = quota.five_hour.expect("five hour");
        assert_eq!(five_hour.percentage, 25.0);
        assert_eq!(five_hour.used_units, 7000);
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
                Sample {
                    timestamp: now - 600,
                    used: 10.0,
                    reset_at: 0,
                },
                Sample {
                    timestamp: now - 200,
                    used: 20.0,
                    reset_at: 0,
                },
            ],
            vec![Sample {
                timestamp: now - 200,
                used: 40.0,
                reset_at: 0,
            }],
            Duration::from_secs(300),
            1024,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].timestamp, now - 200);
        assert_eq!(merged[0].used, 40.0);
    }
}
