#pragma once

#include "usage_core.h"

#include <mutex>
#include <string>
#include <vector>

enum class UsageWindow { Claude5h, Claude7d, Codex7d };

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
    void RefreshIfNeeded();
    UsageMetric GetMetric(UsageWindow window) const;
    long long GetClaudeNextResetAtUnixSeconds() const;
    std::vector<UsageHistoryPoint> GetHistory(UsageWindow window) const;
private:
    const UsageCoreMetric& MetricFor(UsageWindow window) const;

    mutable std::mutex m_mutex;
    UsageCoreSnapshot m_snapshot{};
    unsigned long long m_last_refresh_tick{};
};

#define g_usage_core UsageCoreAdapterInstance()
CUsageCoreAdapter& UsageCoreAdapterInstance();
