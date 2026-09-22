#pragma once

#include <afxdialogex.h>
#include "resource.h"

class CDisplayOptionsDialog final : public CDialogEx
{
public:
    CDisplayOptionsDialog(int graph_mode, int single_item_layout, int color_mode, bool claude_weekly_first,
        bool zcode_peak_enabled, int zcode_peak_start_minute, int zcode_peak_end_minute,
        int zcode_peak_utc_offset_minutes, bool zcode_peak_weekdays_only,
        bool show_claude, bool show_codex, bool show_zcode, CWnd* parent = nullptr);

    int GraphMode() const { return m_graph_mode; }
    int SingleItemLayout() const { return m_single_item_layout; }
    int ColorMode() const { return m_color_mode; }
    bool ClaudeWeeklyFirst() const { return m_claude_weekly_first; }
    bool ZCodePeakEnabled() const { return m_zcode_peak_enabled; }
    int ZCodePeakStartMinute() const { return m_zcode_peak_start_minute; }
    int ZCodePeakEndMinute() const { return m_zcode_peak_end_minute; }
    int ZCodePeakUtcOffsetMinutes() const { return m_zcode_peak_utc_offset_minutes; }
    bool ZCodePeakWeekdaysOnly() const { return m_zcode_peak_weekdays_only; }
    bool ShowClaude() const { return m_show_claude; }
    bool ShowCodex() const { return m_show_codex; }
    bool ShowZCode() const { return m_show_zcode; }

protected:
    BOOL OnInitDialog() override;
    void OnOK() override;

private:
    int m_graph_mode{};
    int m_single_item_layout{};
    int m_color_mode{};
    bool m_claude_weekly_first{};
    bool m_zcode_peak_enabled{};
    int m_zcode_peak_start_minute{};
    int m_zcode_peak_end_minute{};
    int m_zcode_peak_utc_offset_minutes{};
    bool m_zcode_peak_weekdays_only{};
    bool m_show_claude{};
    bool m_show_codex{};
    bool m_show_zcode{};
};
