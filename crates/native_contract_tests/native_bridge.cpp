#include "UsageCoreAdapter.h"

#include <cstddef>

static_assert(sizeof(UsageCoreHistoryPoint) == 16);
static_assert(sizeof(UsageCoreMetric) == 2080);
static_assert(alignof(UsageCoreMetric) == 8);
static_assert(offsetof(UsageCoreMetric, history) == 32);
static_assert(sizeof(UsageCoreSnapshot) == 10448);
static_assert(sizeof(UsageCorePeakSettings) == 20);
static_assert(sizeof(UsageCorePeakPosition) == 24);
static_assert(offsetof(UsageCorePeakPosition, intensity) == 16);

// Only ABI access lives here. The implementation under test is the same
// UsageCoreAdapter.cpp that MSBuild links into the TrafficMonitor DLL.
extern "C" {
void* native_adapter_create(CUsageCoreAdapter::RefreshFunction refresh, CUsageCoreAdapter::ClockFunction clock)
{
    return new CUsageCoreAdapter(refresh, clock);
}

void native_adapter_destroy(void* adapter)
{
    delete static_cast<CUsageCoreAdapter*>(adapter);
}

void native_adapter_refresh(void* adapter)
{
    static_cast<CUsageCoreAdapter*>(adapter)->RefreshIfNeeded();
}

double native_adapter_percentage(const void* adapter)
{
    return static_cast<const CUsageCoreAdapter*>(adapter)->GetMetric(UsageWindow::Claude5h).percentage;
}

int64_t native_adapter_integer(const void* adapter, unsigned int field)
{
    const auto& source = *static_cast<const CUsageCoreAdapter*>(adapter);
    switch (field)
    {
    case 0: return source.GetMetric(UsageWindow::Claude5h).reset_at_unix_seconds;
    case 1: return source.GetClaudeNextResetAtUnixSeconds();
    case 2: return source.GetZCodeNextResetAtUnixSeconds();
    case 3: return source.GetZCodeUsedUnits(UsageWindow::ZCode5h);
    case 4: return source.GetZCodeLimitUnits(UsageWindow::ZCode7d);
    case 5: return static_cast<int64_t>(source.GetHistory(UsageWindow::Claude5h).size());
    case 6:
    {
        const auto history = source.GetHistory(UsageWindow::Claude5h);
        return history.empty() ? 0 : history.front().timestamp_unix_seconds;
    }
    default: return 0;
    }
}

double native_adapter_first_history_percentage(const void* adapter)
{
    const auto history = static_cast<const CUsageCoreAdapter*>(adapter)->GetHistory(UsageWindow::Claude5h);
    return history.empty() ? 0.0 : history.front().percentage;
}

void native_adapter_snapshot(const void* adapter, UsageCoreSnapshot* out)
{
    *out = static_cast<const CUsageCoreAdapter*>(adapter)->GetSnapshot();
}

std::size_t native_abi_value(unsigned int index)
{
    switch (index)
    {
    case 0: return sizeof(UsageCoreHistoryPoint);
    case 1: return sizeof(UsageCoreMetric);
    case 2: return alignof(UsageCoreMetric);
    case 3: return offsetof(UsageCoreMetric, history);
    case 4: return sizeof(UsageCoreSnapshot);
    case 5: return offsetof(UsageCoreSnapshot, claude_next_reset_at_unix_seconds);
    case 6: return offsetof(UsageCoreSnapshot, zcode_5h);
    case 7: return sizeof(UsageCorePeakSettings);
    case 8: return sizeof(UsageCorePeakPosition);
    case 9: return offsetof(UsageCorePeakPosition, intensity);
    default: return 0;
    }
}
}
