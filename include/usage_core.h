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
} UsageCoreSnapshot;

#ifdef __cplusplus
extern "C" {
#endif
int32_t usage_core_refresh(UsageCoreSnapshot* out);
#ifdef __cplusplus
}
#endif
