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

// Match the named Rust layout table. Include every declared field: a same-size
// field reorder must not hide behind an unchanged sizeof or selected offset.
static constexpr std::size_t abi_values[] = {
    sizeof(UsageCoreHistoryPoint), alignof(UsageCoreHistoryPoint),
    offsetof(UsageCoreHistoryPoint, timestamp_unix_seconds),
    offsetof(UsageCoreHistoryPoint, used_percentage),
    sizeof(UsageCoreMetric), alignof(UsageCoreMetric),
    offsetof(UsageCoreMetric, available),
    offsetof(UsageCoreMetric, _padding),
    offsetof(UsageCoreMetric, used_percentage),
    offsetof(UsageCoreMetric, reset_at_unix_seconds),
    offsetof(UsageCoreMetric, history_len),
    offsetof(UsageCoreMetric, _reserved),
    offsetof(UsageCoreMetric, history),
    sizeof(UsageCoreSnapshot), alignof(UsageCoreSnapshot),
    offsetof(UsageCoreSnapshot, claude_5h),
    offsetof(UsageCoreSnapshot, claude_7d),
    offsetof(UsageCoreSnapshot, codex_7d),
    offsetof(UsageCoreSnapshot, claude_next_reset_at_unix_seconds),
    offsetof(UsageCoreSnapshot, zcode_5h),
    offsetof(UsageCoreSnapshot, zcode_7d),
    offsetof(UsageCoreSnapshot, zcode_next_reset_at_unix_seconds),
    offsetof(UsageCoreSnapshot, zcode_5h_used_units),
    offsetof(UsageCoreSnapshot, zcode_5h_limit_units),
    offsetof(UsageCoreSnapshot, zcode_7d_used_units),
    offsetof(UsageCoreSnapshot, zcode_7d_limit_units),
    sizeof(UsageCorePeakSettings), alignof(UsageCorePeakSettings),
    offsetof(UsageCorePeakSettings, enabled),
    offsetof(UsageCorePeakSettings, start_minute),
    offsetof(UsageCorePeakSettings, end_minute),
    offsetof(UsageCorePeakSettings, utc_offset_minutes),
    offsetof(UsageCorePeakSettings, weekdays_only),
    sizeof(UsageCorePeakPosition), alignof(UsageCorePeakPosition),
    offsetof(UsageCorePeakPosition, inside),
    offsetof(UsageCorePeakPosition, minutes_until_start),
    offsetof(UsageCorePeakPosition, minutes_until_end),
    offsetof(UsageCorePeakPosition, reserved),
    offsetof(UsageCorePeakPosition, intensity),
};

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
    case 7: return source.GetZCodeLimitUnits(UsageWindow::ZCode5h);
    case 8: return source.GetZCodeUsedUnits(UsageWindow::ZCode7d);
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

void native_adapter_observe_metric(const void* adapter, unsigned int window_index,
    int64_t* integer_fields, double* floating_fields)
{
    constexpr UsageWindow windows[] = { UsageWindow::Claude5h, UsageWindow::Claude7d,
        UsageWindow::Codex7d, UsageWindow::ZCode5h, UsageWindow::ZCode7d };
    if (window_index >= sizeof(windows) / sizeof(windows[0]))
        return;
    const auto& source = *static_cast<const CUsageCoreAdapter*>(adapter);
    const auto metric = source.GetMetric(windows[window_index]);
    const auto history = source.GetHistory(windows[window_index]);
    integer_fields[0] = metric.available ? 1 : 0;
    integer_fields[1] = metric.reset_at_unix_seconds;
    integer_fields[2] = static_cast<int64_t>(history.size());
    integer_fields[3] = history.empty() ? 0 : history.front().timestamp_unix_seconds;
    integer_fields[4] = history.empty() ? 0 : history.back().timestamp_unix_seconds;
    floating_fields[0] = metric.percentage;
    floating_fields[1] = history.empty() ? 0.0 : history.front().percentage;
    floating_fields[2] = history.empty() ? 0.0 : history.back().percentage;
}

std::size_t native_abi_count()
{
    return sizeof(abi_values) / sizeof(abi_values[0]);
}

std::size_t native_abi_value(unsigned int index)
{
    return index < native_abi_count() ? abi_values[index] : 0;
}
}
