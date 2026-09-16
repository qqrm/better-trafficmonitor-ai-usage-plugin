#include "pch.h"
#include "DisplayOptionsDialog.h"

#include <cwchar>

namespace
{
void FormatMinuteOfDay(int minute, wchar_t* buffer, size_t capacity)
{
    swprintf_s(buffer, capacity, L"%02d:%02d", minute / 60, minute % 60);
}

// Accepts "9", "9:30", "09:00", "0930", with an optional sign for offsets
// ("-4:30"). Returns the value in minutes, or fallback when unparseable.
int ParseMinuteText(const wchar_t* text, int fallback)
{
    wchar_t* end = nullptr;
    const long whole = wcstol(text, &end, 10);
    if (end == text)
        return fallback;
    int minutes = static_cast<int>(whole * 60);
    if (end != nullptr && (*end == L':' || *end == L'.') && end[0] != L'\0')
    {
        wchar_t* fraction_end = nullptr;
        const long part = wcstol(end + 1, &fraction_end, 10);
        if (fraction_end != end + 1)
        {
            const bool negative = whole < 0;
            const int magnitude = static_cast<int>(part);
            minutes = negative ? minutes - magnitude : minutes + magnitude;
        }
    }
    return minutes;
}

int ReadMinuteEdit(const CDialog* dialog, int control_id, int fallback)
{
    const CWnd* control = dialog->GetDlgItem(control_id);
    if (control == nullptr)
        return fallback;
    wchar_t text[32]{};
    if (GetWindowTextW(control->GetSafeHwnd(), text, 32) <= 0)
        return fallback;
    return ParseMinuteText(text, fallback);
}

void WriteMinuteEdit(CDialog* dialog, int control_id, int minute)
{
    wchar_t buffer[16]{};
    FormatMinuteOfDay(minute, buffer, 16);
    dialog->SetDlgItemTextW(control_id, buffer);
}

void WriteOffsetEdit(CDialog* dialog, int control_id, int offset_minutes)
{
    const int sign = offset_minutes < 0 ? -1 : 1;
    const int magnitude = abs(offset_minutes);
    wchar_t buffer[16]{};
    swprintf_s(buffer, L"%+03d:%02d", sign * (magnitude / 60), magnitude % 60);
    dialog->SetDlgItemTextW(control_id, buffer);
}
}

CDisplayOptionsDialog::CDisplayOptionsDialog(int graph_mode, int single_item_layout, int color_mode, bool claude_weekly_first,
    bool zcode_peak_enabled, int zcode_peak_start_minute, int zcode_peak_end_minute,
    int zcode_peak_utc_offset_minutes, bool zcode_peak_weekdays_only, CWnd* parent)
    : CDialogEx(IDD_DISPLAY_OPTIONS, parent)
    , m_graph_mode(graph_mode)
    , m_single_item_layout(single_item_layout)
    , m_color_mode(color_mode)
    , m_claude_weekly_first(claude_weekly_first)
    , m_zcode_peak_enabled(zcode_peak_enabled)
    , m_zcode_peak_start_minute(zcode_peak_start_minute)
    , m_zcode_peak_end_minute(zcode_peak_end_minute)
    , m_zcode_peak_utc_offset_minutes(zcode_peak_utc_offset_minutes)
    , m_zcode_peak_weekdays_only(zcode_peak_weekdays_only)
{
}

BOOL CDisplayOptionsDialog::OnInitDialog()
{
    CDialogEx::OnInitDialog();
    CheckRadioButton(IDC_GRAPH_REMAINING, IDC_GRAPH_USED, IDC_GRAPH_REMAINING + m_graph_mode);
    CheckRadioButton(IDC_LAYOUT_CENTER, IDC_LAYOUT_STRETCH, IDC_LAYOUT_CENTER + m_single_item_layout);
    CheckRadioButton(IDC_COLOR_MONOCHROME, IDC_COLOR_ALWAYS, IDC_COLOR_MONOCHROME + m_color_mode);
    CheckDlgButton(IDC_CLAUDE_WEEKLY_FIRST, m_claude_weekly_first ? BST_CHECKED : BST_UNCHECKED);
    CheckDlgButton(IDC_ZCODE_PEAK_ENABLED, m_zcode_peak_enabled ? BST_CHECKED : BST_UNCHECKED);
    CheckDlgButton(IDC_ZCODE_PEAK_WEEKDAYS, m_zcode_peak_weekdays_only ? BST_CHECKED : BST_UNCHECKED);
    WriteMinuteEdit(this, IDC_ZCODE_PEAK_START, m_zcode_peak_start_minute);
    WriteMinuteEdit(this, IDC_ZCODE_PEAK_END, m_zcode_peak_end_minute);
    WriteOffsetEdit(this, IDC_ZCODE_PEAK_UTC, m_zcode_peak_utc_offset_minutes);
    return TRUE;
}

void CDisplayOptionsDialog::OnOK()
{
    m_graph_mode = IsDlgButtonChecked(IDC_GRAPH_USED) == BST_CHECKED ? 1 : 0;

    if (IsDlgButtonChecked(IDC_LAYOUT_TOP) == BST_CHECKED)
        m_single_item_layout = 1;
    else if (IsDlgButtonChecked(IDC_LAYOUT_BOTTOM) == BST_CHECKED)
        m_single_item_layout = 2;
    else if (IsDlgButtonChecked(IDC_LAYOUT_STRETCH) == BST_CHECKED)
        m_single_item_layout = 3;
    else
        m_single_item_layout = 0;

    if (IsDlgButtonChecked(IDC_COLOR_MONOCHROME) == BST_CHECKED)
        m_color_mode = 0;
    else if (IsDlgButtonChecked(IDC_COLOR_ALWAYS) == BST_CHECKED)
        m_color_mode = 2;
    else
        m_color_mode = 1;

    m_claude_weekly_first = IsDlgButtonChecked(IDC_CLAUDE_WEEKLY_FIRST) == BST_CHECKED;
    m_zcode_peak_enabled = IsDlgButtonChecked(IDC_ZCODE_PEAK_ENABLED) == BST_CHECKED;
    m_zcode_peak_weekdays_only = IsDlgButtonChecked(IDC_ZCODE_PEAK_WEEKDAYS) == BST_CHECKED;
    m_zcode_peak_start_minute = max(0, min(1439, ReadMinuteEdit(this, IDC_ZCODE_PEAK_START, m_zcode_peak_start_minute)));
    m_zcode_peak_end_minute = max(0, min(1439, ReadMinuteEdit(this, IDC_ZCODE_PEAK_END, m_zcode_peak_end_minute)));
    m_zcode_peak_utc_offset_minutes = max(-13 * 60, min(14 * 60, ReadMinuteEdit(this, IDC_ZCODE_PEAK_UTC, m_zcode_peak_utc_offset_minutes)));

    CDialogEx::OnOK();
}
