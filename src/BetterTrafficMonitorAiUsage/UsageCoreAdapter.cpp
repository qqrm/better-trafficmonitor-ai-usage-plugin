#include "UsageCoreAdapter.h"

#include <algorithm>
#include <chrono>

constexpr unsigned long long REFRESH_INTERVAL_MS = 10ULL * 1000ULL;

CUsageCoreAdapter::CUsageCoreAdapter(RefreshFunction refresh, ClockFunction clock)
    : m_refresh(refresh), m_clock(clock)
{
}

unsigned long long CUsageCoreAdapter::MonotonicMilliseconds()
{
    return static_cast<unsigned long long>(std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count());
}

CUsageCoreAdapter& UsageCoreAdapterInstance()
{
    static CUsageCoreAdapter instance;
    return instance;
}

void CUsageCoreAdapter::RefreshIfNeeded()
{
    // A second polling caller must not queue behind slow provider I/O.
    std::unique_lock<std::mutex> refresh_lock(m_refresh_mutex, std::try_to_lock);
    if (!refresh_lock.owns_lock())
        return;

    const unsigned long long now = m_clock();
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        if (m_has_refreshed && now >= m_last_refresh_tick
            && now - m_last_refresh_tick < REFRESH_INTERVAL_MS)
            return;
    }

    // Readers keep displaying the last complete snapshot while sources poll.
    // DataRequired itself remains synchronous on TrafficMonitor's monitor thread.
    UsageCoreSnapshot snapshot{};
    const bool refreshed = m_refresh(&snapshot) != 0;
    const unsigned long long completed = m_clock();
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        if (refreshed)
            m_snapshot = snapshot;
        m_last_refresh_tick = completed;
        m_has_refreshed = true;
    }
}

UsageCoreSnapshot CUsageCoreAdapter::GetSnapshot() const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    return m_snapshot;
}

const UsageCoreMetric& CUsageCoreAdapter::MetricFor(UsageWindow window) const
{
    switch (window)
    {
    case UsageWindow::Claude5h: return m_snapshot.claude_5h;
    case UsageWindow::Claude7d: return m_snapshot.claude_7d;
    case UsageWindow::Codex7d: return m_snapshot.codex_7d;
    case UsageWindow::ZCode5h: return m_snapshot.zcode_5h;
    case UsageWindow::ZCode7d: return m_snapshot.zcode_7d;
    }
    return m_snapshot.codex_7d;
}

UsageMetric CUsageCoreAdapter::GetMetric(UsageWindow window) const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    const UsageCoreMetric& source = MetricFor(window);
    return UsageMetric{ source.available != 0, source.used_percentage, source.reset_at_unix_seconds };
}

long long CUsageCoreAdapter::GetClaudeNextResetAtUnixSeconds() const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    return m_snapshot.claude_next_reset_at_unix_seconds;
}

long long CUsageCoreAdapter::GetZCodeNextResetAtUnixSeconds() const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    return m_snapshot.zcode_next_reset_at_unix_seconds;
}

long long CUsageCoreAdapter::GetZCodeUsedUnits(UsageWindow window) const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    switch (window)
    {
    case UsageWindow::ZCode5h: return m_snapshot.zcode_5h_used_units;
    case UsageWindow::ZCode7d: return m_snapshot.zcode_7d_used_units;
    default: break;
    }
    return 0;
}

long long CUsageCoreAdapter::GetZCodeLimitUnits(UsageWindow window) const
{
    std::lock_guard<std::mutex> lock(m_mutex);
    switch (window)
    {
    case UsageWindow::ZCode5h: return m_snapshot.zcode_5h_limit_units;
    case UsageWindow::ZCode7d: return m_snapshot.zcode_7d_limit_units;
    default: break;
    }
    return 0;
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
