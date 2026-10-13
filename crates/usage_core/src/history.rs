//! Pure history normalization and freshness rules shared by providers.
use crate::{MAX_HISTORY_POINTS, UsageCoreHistoryPoint, UsageCoreMetric};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy)]
pub(crate) struct Sample {
    pub(crate) timestamp: i64,
    pub(crate) used: f64,
    pub(crate) reset_at: i64,
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn normalized_samples(
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

pub(crate) fn normalized_metric(samples: Vec<Sample>, window: Duration) -> UsageCoreMetric {
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

pub(crate) fn metric_if_recent(
    metric: UsageCoreMetric,
    now: i64,
    max_age: Duration,
) -> UsageCoreMetric {
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
