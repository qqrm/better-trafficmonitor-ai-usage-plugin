#include "pch.h"
#include "UsageCoreAdapter.h"

#include <algorithm>

constexpr unsigned long long REFRESH_INTERVAL_MS = 30ULL * 1000ULL;

CUsageCoreAdapter& UsageCoreAdapterInstance()
{
    static CUsageCoreAdapter instance;
    return instance;
}

void CUsageCoreAdapter::RefreshIfNeeded()
{
    const unsigned long long now = GetTickCount64();
    std::lock_guard<std::mutex> lock(m_mutex);
    if (m_last_refresh_tick != 0 && now - m_last_refresh_tick < REFRESH_INTERVAL_MS)
        return;

    UsageCoreSnapshot snapshot{};
    if (usage_core_refresh(&snapshot) != 0)
        m_snapshot = snapshot;
    m_last_refresh_tick = now;
}

const UsageCoreMetric& CUsageCoreAdapter::MetricFor(UsageWindow window) const
{
    switch (window)
    {
    case UsageWindow::Claude5h: return m_snapshot.claude_5h;
    case UsageWindow::Claude7d: return m_snapshot.claude_7d;
    case UsageWindow::Codex7d: return m_snapshot.codex_7d;
    }
    return m_snapshot.codex_7d;
}

UsageMetric CUsageCoreAdapter::GetMetric(UsageWindow window) const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    const UsageCoreMetric& source = MetricFor(window);
    return UsageMetric{ source.available != 0, source.used_percentage, source.reset_at_unix_seconds };
}

std::vector<UsageHistoryPoint> CUsageCoreAdapter::GetHistory(UsageWindow window) const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    const UsageCoreMetric& source = MetricFor(window);
    const unsigned int count = std::min<unsigned int>(source.history_len, USAGE_CORE_MAX_HISTORY_POINTS);
    std::vector<UsageHistoryPoint> result;
    result.reserve(count);
    for (unsigned int index = 0; index < count; ++index)
        result.push_back({ source.history[index].timestamp_unix_seconds, source.history[index].used_percentage });
    return result;
}
