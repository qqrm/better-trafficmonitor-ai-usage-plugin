#pragma once

#include "usage_core.h"

#include <mutex>
#include <string>
#include <vector>

enum class UsageWindow { Claude5h, Claude7d, Codex7d, ZCode5h, ZCode7d };

struct UsageMetric {
    bool available{};
    double percentage{};
    long long reset_at_unix_seconds{};
};

struct UsageHistoryPoint {
    long long timestamp_unix_seconds{};
    double percentage{};
};

class CUsageCoreAdapter {
public:
    using RefreshFunction = int32_t (*)(UsageCoreSnapshot*);
    using ClockFunction = unsigned long long (*)();

    explicit CUsageCoreAdapter(RefreshFunction refresh = usage_core_refresh,
        ClockFunction clock = MonotonicMilliseconds);
    void RefreshIfNeeded();
    UsageCoreSnapshot GetSnapshot() const;
    UsageMetric GetMetric(UsageWindow window) const;
    long long GetClaudeNextResetAtUnixSeconds() const;
    long long GetZCodeNextResetAtUnixSeconds() const;
    // Credit units of the z.ai coding plan from the latest server response.
    long long GetZCodeUsedUnits(UsageWindow window) const;
    long long GetZCodeLimitUnits(UsageWindow window) const;
    std::vector<UsageHistoryPoint> GetHistory(UsageWindow window) const;
private:
    static unsigned long long MonotonicMilliseconds();
    const UsageCoreMetric& MetricFor(UsageWindow window) const;

    const RefreshFunction m_refresh;
    const ClockFunction m_clock;
    std::mutex m_refresh_mutex;
    mutable std::mutex m_mutex;
    UsageCoreSnapshot m_snapshot{};
    unsigned long long m_last_refresh_tick{};
    bool m_has_refreshed{};
};

#define g_usage_core UsageCoreAdapterInstance()
CUsageCoreAdapter& UsageCoreAdapterInstance();
