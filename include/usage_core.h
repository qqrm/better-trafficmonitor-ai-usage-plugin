#pragma once

#include <stdint.h>

#define USAGE_CORE_MAX_HISTORY_POINTS 128

typedef struct UsageCoreHistoryPoint {
    int64_t timestamp_unix_seconds;
    double used_percentage;
} UsageCoreHistoryPoint;

typedef struct UsageCoreMetric {
    uint8_t available;
    uint8_t _padding[7];
    double used_percentage;
    int64_t reset_at_unix_seconds;
    uint32_t history_len;
    uint32_t _reserved;
    UsageCoreHistoryPoint history[USAGE_CORE_MAX_HISTORY_POINTS];
} UsageCoreMetric;

typedef struct UsageCoreSnapshot {
    UsageCoreMetric claude_5h;
    UsageCoreMetric claude_7d;
    UsageCoreMetric codex_7d;
    int64_t claude_next_reset_at_unix_seconds;
    UsageCoreMetric zcode_5h;
    UsageCoreMetric zcode_7d;
    int64_t zcode_next_reset_at_unix_seconds;
    int64_t zcode_5h_used_units;
    int64_t zcode_5h_limit_units;
    int64_t zcode_7d_used_units;
    int64_t zcode_7d_limit_units;
} UsageCoreSnapshot;

typedef struct UsageCorePeakSettings {
    int32_t enabled;
    int32_t start_minute;
    int32_t end_minute;
    int32_t utc_offset_minutes;
    int32_t weekdays_only;
} UsageCorePeakSettings;

typedef struct UsageCorePeakPosition {
    int32_t inside;
    int32_t minutes_until_start;
    int32_t minutes_until_end;
    int32_t reserved;
    double intensity;
} UsageCorePeakPosition;

#ifdef __cplusplus
extern "C" {
#endif
int32_t usage_core_refresh(UsageCoreSnapshot* out);
// UTF-16 length excludes NUL. Invalid input leaves the setting at fallback.
int32_t usage_core_parse_minutes(const uint16_t* text, uint32_t length, int32_t fallback);
// Capacity includes NUL; returns written length, or zero with an empty output.
uint32_t usage_core_format_utc_offset(int32_t minutes, uint16_t* out, uint32_t capacity);
int32_t usage_core_peak_position(int64_t unix_seconds, const UsageCorePeakSettings* settings, UsageCorePeakPosition* out);
#ifdef __cplusplus
}
#endif
