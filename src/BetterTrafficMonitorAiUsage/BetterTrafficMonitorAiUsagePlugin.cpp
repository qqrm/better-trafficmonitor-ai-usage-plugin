#include "pch.h"
#include "BetterTrafficMonitorAiUsagePlugin.h"
#include "DisplayOptionsDialog.h"
#include "ProviderMarkAssets.h"

#include <algorithm>
#include <atomic>
#include <cstring>
#include <vector>

#pragma comment(lib, "msimg32.lib")

namespace
{
enum class UsageProvider
{
    Claude,
    Codex,
};

enum class GraphDisplayMode
{
    Remaining,
    Used,
};

enum class SingleItemLayout
{
    Center,
    Top,
    Bottom,
    Stretch,
};

enum class ColorMode
{
    Monochrome,
    Adaptive,
    AlwaysColored,
};

std::atomic<GraphDisplayMode> g_graph_display_mode{ GraphDisplayMode::Remaining };
std::atomic<SingleItemLayout> g_single_item_layout{ SingleItemLayout::Center };
std::atomic<ColorMode> g_color_mode{ ColorMode::Adaptive };
std::atomic_bool g_claude_weekly_first{ true };

constexpr int SINGLE_ITEM_HEIGHT_THRESHOLD = 24;

bool IsSingleItemCell(int height)
{
    return height > SINGLE_ITEM_HEIGHT_THRESHOLD;
}

bool UseClaudeColor(int height)
{
    switch (g_color_mode.load(std::memory_order_relaxed))
    {
    case ColorMode::Monochrome:
        return false;
    case ColorMode::AlwaysColored:
        return true;
    case ColorMode::Adaptive:
    default:
        return !IsSingleItemCell(height);
    }
}

double DisplayedPercentage(double used_percentage)
{
    const double bounded_used = max(0.0, min(100.0, used_percentage));
    return g_graph_display_mode.load(std::memory_order_relaxed) == GraphDisplayMode::Remaining
        ? 100.0 - bounded_used
        : bounded_used;
}

struct UsageDrawStyle
{
    COLORREF fill{};
    COLORREF track{};
    COLORREF border{};
    COLORREF text_on_track{};
    COLORREF text_on_fill{};
    COLORREF unavailable_text{};
    COLORREF guide{};
};

UsageDrawStyle GetUsageDrawStyle(UsageProvider provider, bool dark_mode)
{
    if (provider == UsageProvider::Claude)
    {
        return dark_mode
            ? UsageDrawStyle{ RGB(220, 123, 67), RGB(61, 53, 48), RGB(115, 92, 78), RGB(244, 240, 236), RGB(244, 240, 236), RGB(161, 137, 121), RGB(91, 78, 69) }
            : UsageDrawStyle{ RGB(198, 91, 37), RGB(232, 232, 232), RGB(162, 162, 162), RGB(42, 42, 42), RGB(42, 42, 42), RGB(128, 128, 128), RGB(204, 204, 204) };
    }

    return dark_mode
        ? UsageDrawStyle{ RGB(224, 224, 224), RGB(58, 58, 58), RGB(112, 112, 112), RGB(244, 244, 244), RGB(244, 244, 244), RGB(145, 145, 145), RGB(84, 84, 84) }
        : UsageDrawStyle{ RGB(46, 46, 46), RGB(204, 204, 204), RGB(147, 147, 147), RGB(32, 32, 32), RGB(32, 32, 32), RGB(118, 118, 118), RGB(181, 181, 181) };
}

int MeasureTextWidth(CDC* pDC, const wchar_t* text)
{
    if (pDC == nullptr || text == nullptr || *text == L'\0')
        return 0;

    return pDC->GetTextExtent(text).cx;
}

std::wstring FormatDisplayedPercentage(bool available, double used_percentage)
{
    if (!available)
        return L"--";

    wchar_t buffer[16]{};
    swprintf_s(buffer, L"%.0f%%", DisplayedPercentage(used_percentage));
    return buffer;
}

std::wstring FormatResetTime(long long reset_at_unix_seconds)
{
    if (reset_at_unix_seconds <= 0)
        return L"unavailable";

    const CTime reset_at(static_cast<__time64_t>(reset_at_unix_seconds));
    return std::wstring(reset_at.Format(L"%Y-%m-%d %H:%M local"));
}

std::wstring FormatTimeUntilReset(long long reset_at_unix_seconds)
{
    if (reset_at_unix_seconds <= 0)
        return L"--";

    const long long now = static_cast<long long>(CTime::GetCurrentTime().GetTime());
    const long long remaining_seconds = reset_at_unix_seconds - now;
    if (remaining_seconds <= 0)
        return L"0m";

    constexpr long long minute = 60LL;
    constexpr long long hour = 60LL * minute;
    constexpr long long day = 24LL * hour;
    if (remaining_seconds >= day)
        return std::to_wstring(remaining_seconds / day) + L"d";
    if (remaining_seconds >= hour)
        return std::to_wstring(remaining_seconds / hour) + L"h";
    return std::to_wstring(remaining_seconds / minute) + L"m";
}

std::wstring FormatLimitDetails(const UsageMetric& metric)
{
    if (!metric.available)
        return L"unavailable (no recent local data)";

    const double used = max(0.0, min(100.0, metric.percentage));
    wchar_t buffer[48]{};
    swprintf_s(buffer, L"used %.0f%%, %.0f%% remaining", used, 100.0 - used);
    return buffer;
}

float GetUsageRatio(const UsageMetric& metric)
{
    if (!metric.available)
        return 0.0f;

    return static_cast<float>(DisplayedPercentage(metric.percentage) / 100.0);
}

class DibSurface
{
public:
    DibSurface(HDC reference, int width, int height)
        : m_width(width)
        , m_height(height)
    {
        if (reference == nullptr || width <= 0 || height <= 0)
            return;

        m_dc = CreateCompatibleDC(reference);
        if (m_dc == nullptr)
            return;

        BITMAPINFO bitmap_info{};
        bitmap_info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        bitmap_info.bmiHeader.biWidth = width;
        bitmap_info.bmiHeader.biHeight = -height;
        bitmap_info.bmiHeader.biPlanes = 1;
        bitmap_info.bmiHeader.biBitCount = 32;
        bitmap_info.bmiHeader.biCompression = BI_RGB;
        m_bitmap = CreateDIBSection(m_dc, &bitmap_info, DIB_RGB_COLORS, reinterpret_cast<void**>(&m_pixels), nullptr, 0);
        if (m_bitmap == nullptr || m_pixels == nullptr)
            return;

        m_previous_bitmap = SelectObject(m_dc, m_bitmap);
        std::memset(m_pixels, 0, static_cast<size_t>(width) * static_cast<size_t>(height) * sizeof(std::uint32_t));
    }

    ~DibSurface()
    {
        if (m_dc != nullptr && m_previous_bitmap != nullptr)
            SelectObject(m_dc, m_previous_bitmap);
        if (m_bitmap != nullptr)
            DeleteObject(m_bitmap);
        if (m_dc != nullptr)
            DeleteDC(m_dc);
    }

    bool IsValid() const
    {
        return m_dc != nullptr && m_bitmap != nullptr && m_pixels != nullptr;
    }

    HDC Dc() const { return m_dc; }
    std::uint32_t* Pixels() const { return m_pixels; }
    int Width() const { return m_width; }
    int Height() const { return m_height; }

private:
    int m_width{};
    int m_height{};
    HDC m_dc{};
    HBITMAP m_bitmap{};
    HGDIOBJ m_previous_bitmap{};
    std::uint32_t* m_pixels{};
};

std::uint32_t PremultipliedPixel(COLORREF color, std::uint8_t alpha)
{
    const std::uint32_t red = GetRValue(color) * alpha / 255U;
    const std::uint32_t green = GetGValue(color) * alpha / 255U;
    const std::uint32_t blue = GetBValue(color) * alpha / 255U;
    return static_cast<std::uint32_t>(alpha) << 24 | red << 16 | green << 8 | blue;
}

void BlendSurface(CDC* target, const CRect& destination, const DibSurface& source)
{
    if (target == nullptr || destination.IsRectEmpty() || !source.IsValid())
        return;

    BLENDFUNCTION blend{};
    blend.BlendOp = AC_SRC_OVER;
    blend.SourceConstantAlpha = 255;
    blend.AlphaFormat = AC_SRC_ALPHA;
    AlphaBlend(target->GetSafeHdc(), destination.left, destination.top, destination.Width(), destination.Height(),
        source.Dc(), 0, 0, source.Width(), source.Height(), blend);
}

void DrawProviderMark(CDC* pDC, UsageProvider provider, const CRect& rect, COLORREF color)
{
    if (pDC == nullptr || rect.IsRectEmpty())
        return;

    DibSurface surface(pDC->GetSafeHdc(), PROVIDER_MARK_SIZE, PROVIDER_MARK_SIZE);
    if (!surface.IsValid())
        return;

    const std::uint8_t* alpha_mask = provider == UsageProvider::Claude ? CLAUDE_MARK_ALPHA : OPENAI_MARK_ALPHA;
    for (int index = 0; index < PROVIDER_MARK_SIZE * PROVIDER_MARK_SIZE; ++index)
        surface.Pixels()[index] = PremultipliedPixel(color, alpha_mask[index]);
    BlendSurface(pDC, rect, surface);
}

void DrawTranslucentPolygon(CDC* pDC, const CRect& rect, const std::vector<POINT>& points, COLORREF color, std::uint8_t opacity)
{
    if (pDC == nullptr || rect.IsRectEmpty() || points.size() < 3 || opacity == 0)
        return;

    constexpr int ANTIALIAS_SCALE = 4;
    DibSurface mask(pDC->GetSafeHdc(), rect.Width() * ANTIALIAS_SCALE, rect.Height() * ANTIALIAS_SCALE);
    DibSurface surface(pDC->GetSafeHdc(), rect.Width(), rect.Height());
    if (!mask.IsValid() || !surface.IsValid())
        return;

    std::vector<POINT> scaled_points;
    scaled_points.reserve(points.size());
    for (const POINT& point : points)
    {
        scaled_points.push_back({
            (point.x - rect.left) * ANTIALIAS_SCALE,
            (point.y - rect.top) * ANTIALIAS_SCALE,
        });
    }

    HRGN region = CreatePolygonRgn(scaled_points.data(), static_cast<int>(scaled_points.size()), WINDING);
    HBRUSH brush = CreateSolidBrush(RGB(255, 255, 255));
    if (region != nullptr && brush != nullptr)
        FillRgn(mask.Dc(), region, brush);
    if (brush != nullptr)
        DeleteObject(brush);
    if (region != nullptr)
        DeleteObject(region);

    for (int y = 0; y < surface.Height(); ++y)
    {
        for (int x = 0; x < surface.Width(); ++x)
        {
            int covered{};
            for (int sample_y = 0; sample_y < ANTIALIAS_SCALE; ++sample_y)
            {
                for (int sample_x = 0; sample_x < ANTIALIAS_SCALE; ++sample_x)
                {
                    const int mask_x = x * ANTIALIAS_SCALE + sample_x;
                    const int mask_y = y * ANTIALIAS_SCALE + sample_y;
                    if ((mask.Pixels()[mask_y * mask.Width() + mask_x] & 0x00FFFFFFU) != 0)
                        ++covered;
                }
            }
            const std::uint8_t alpha = static_cast<std::uint8_t>(opacity * covered / (ANTIALIAS_SCALE * ANTIALIAS_SCALE));
            surface.Pixels()[y * surface.Width() + x] = PremultipliedPixel(color, alpha);
        }
    }
    BlendSurface(pDC, rect, surface);
}

void DrawProviderMarkOnUsageBackground(
    CDC* pDC,
    UsageProvider provider,
    const CRect& icon_rect,
    const UsageDrawStyle& style,
    int fill_right)
{
    const int saved_dc = pDC->SaveDC();
    pDC->IntersectClipRect(icon_rect.left, icon_rect.top, fill_right, icon_rect.bottom);
    DrawProviderMark(pDC, provider, icon_rect, style.text_on_fill);
    pDC->RestoreDC(saved_dc);

    const int restored_dc = pDC->SaveDC();
    pDC->IntersectClipRect(max(icon_rect.left, fill_right), icon_rect.top, icon_rect.right, icon_rect.bottom);
    DrawProviderMark(pDC, provider, icon_rect, style.text_on_track);
    pDC->RestoreDC(restored_dc);
}

void DrawTextOnUsageBackground(
    CDC* pDC,
    const wchar_t* text,
    const CRect& text_rect,
    UINT format,
    const UsageDrawStyle& style,
    bool available,
    int fill_right)
{
    if (pDC == nullptr || text == nullptr || text_rect.IsRectEmpty())
        return;

    if (!available)
    {
        const COLORREF old_text_color = pDC->SetTextColor(style.unavailable_text);
        pDC->DrawTextW(text, -1, const_cast<CRect*>(&text_rect), format);
        pDC->SetTextColor(old_text_color);
        return;
    }

    const int saved_dc = pDC->SaveDC();
    pDC->IntersectClipRect(text_rect.left, text_rect.top, fill_right, text_rect.bottom);
    const COLORREF old_text_color = pDC->SetTextColor(style.text_on_fill);
    pDC->DrawTextW(text, -1, const_cast<CRect*>(&text_rect), format);
    pDC->SetTextColor(old_text_color);
    pDC->RestoreDC(saved_dc);

    const int restored_dc = pDC->SaveDC();
    pDC->IntersectClipRect(max(text_rect.left, fill_right), text_rect.top, text_rect.right, text_rect.bottom);
    const COLORREF old_track_text_color = pDC->SetTextColor(style.text_on_track);
    pDC->DrawTextW(text, -1, const_cast<CRect*>(&text_rect), format);
    pDC->SetTextColor(old_track_text_color);
    pDC->RestoreDC(restored_dc);
}

void DrawUsageItem(
    CDC* pDC,
    UsageProvider provider,
    const UsageDrawStyle& style,
    const wchar_t* period_text,
    const wchar_t* value_text,
    const wchar_t* value_sample_text,
    bool available,
    float ratio,
    int x,
    int y,
    int w,
    int h)
{
    if (pDC == nullptr || period_text == nullptr || value_text == nullptr || value_sample_text == nullptr || w <= 0 || h <= 0)
        return;

    const int padding = 3;
    const int gap = 4;
    const int icon_size = max(10, min(14, h - padding * 2));
    const int value_width = max(MeasureTextWidth(pDC, value_text), MeasureTextWidth(pDC, value_sample_text));
    const CRect item_rect(x, y, x + w, y + h);
    if (item_rect.IsRectEmpty())
        return;

    const int fill_right = item_rect.left + static_cast<int>(item_rect.Width() * ratio + 0.5f);
    pDC->FillSolidRect(item_rect, style.track);
    if (available && fill_right > item_rect.left)
        pDC->FillSolidRect(item_rect.left, item_rect.top, min(fill_right, item_rect.right), item_rect.bottom, style.fill);
    CBrush border_brush(style.border);
    pDC->FrameRect(&item_rect, &border_brush);

    const int icon_top = item_rect.top + max(0, (item_rect.Height() - icon_size) / 2);
    const CRect icon_rect(item_rect.left + padding, icon_top, item_rect.left + padding + icon_size, icon_top + icon_size);
    const int content_left = icon_rect.right + gap;
    const int value_left = item_rect.right - padding - value_width;
    const CRect period_rect(content_left, item_rect.top, max(content_left, value_left - gap), item_rect.bottom);
    const CRect value_rect(value_left, item_rect.top, item_rect.right - padding, item_rect.bottom);

    const int old_bk_mode = pDC->SetBkMode(TRANSPARENT);
    DrawProviderMarkOnUsageBackground(pDC, provider, icon_rect, style, available ? fill_right : item_rect.left);
    DrawTextOnUsageBackground(pDC, period_text, period_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, style, available, fill_right);
    DrawTextOnUsageBackground(pDC, value_text, value_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, style, available, fill_right);
    pDC->SetBkMode(old_bk_mode);
}

struct UsageHistoryPoint
{
    long long timestamp_unix_seconds{};
    double percentage{};
};

CRect StableContentRect(int x, int y, int w, int h)
{
    constexpr int MAX_CONTENT_HEIGHT = 18;
    if (IsSingleItemCell(h) && g_single_item_layout.load(std::memory_order_relaxed) == SingleItemLayout::Stretch)
        return CRect(x, y, x + w, y + h);

    const int content_height = min(h, MAX_CONTENT_HEIGHT);
    int content_top = y + max(0, (h - content_height) / 2);
    if (IsSingleItemCell(h))
    {
        switch (g_single_item_layout.load(std::memory_order_relaxed))
        {
        case SingleItemLayout::Top:
            content_top = y;
            break;
        case SingleItemLayout::Bottom:
            content_top = y + h - content_height;
            break;
        case SingleItemLayout::Center:
        case SingleItemLayout::Stretch:
        default:
            break;
        }
    }
    return CRect(x, content_top, x + w, content_top + content_height);
}

std::wstring BuildLaneText(const wchar_t* label, const std::wstring& value_text)
{
    std::wstring text(label);
    text += L"  ";
    text += value_text;
    return text;
}

void DrawUsageHistoryLane(
    CDC* pDC,
    const CRect& lane_rect,
    const UsageDrawStyle& style,
    const std::wstring& text,
    bool available,
    const std::vector<UsageHistoryPoint>& history,
    long long window_seconds,
    COLORREF trace_color)
{
    if (pDC == nullptr || lane_rect.IsRectEmpty())
        return;

    // TrafficMonitor's native graphs are bottom-aligned inside a transparent
    // item cell rather than filling it edge to edge.
    const int top_inset = max(2, lane_rect.Height() / 5);
    const CRect plot_rect(lane_rect.left, lane_rect.top + top_inset, lane_rect.right, lane_rect.bottom);

    if (available && !history.empty() && window_seconds > 0)
    {
        const long long newest = history.back().timestamp_unix_seconds;
        const long long requested_oldest = newest - window_seconds;
        long long oldest = newest;
        for (const UsageHistoryPoint& sample : history)
        {
            if (sample.timestamp_unix_seconds >= requested_oldest)
            {
                oldest = sample.timestamp_unix_seconds;
                break;
            }
        }
        const long long span = max(1LL, newest - oldest);
        std::vector<POINT> points;
        points.reserve(history.size());
        for (const UsageHistoryPoint& sample : history)
        {
            if (sample.timestamp_unix_seconds < requested_oldest)
                continue;
            const double relative_time = static_cast<double>(sample.timestamp_unix_seconds - oldest) / static_cast<double>(span);
            const double bounded_time = max(0.0, min(1.0, relative_time));
            const int point_x = plot_rect.left + 1 + static_cast<int>((plot_rect.Width() - 3) * bounded_time + 0.5);
            const double display_ratio = DisplayedPercentage(sample.percentage) / 100.0;
            const int point_y = plot_rect.bottom - 1 - static_cast<int>((plot_rect.Height() - 2) * display_ratio + 0.5);
            points.push_back({ point_x, point_y });
        }

        if (!points.empty())
        {
            std::vector<POINT> fill_points;
            fill_points.reserve(points.size() + 2);
            fill_points.push_back({ points.front().x, plot_rect.bottom });
            for (const POINT& point : points)
                fill_points.push_back(point);
            fill_points.push_back({ points.back().x, plot_rect.bottom });
            DrawTranslucentPolygon(pDC, plot_rect, fill_points, trace_color, 72);
        }
    }

    const int old_bk_mode = pDC->SetBkMode(TRANSPARENT);
    const COLORREF old_text_color = pDC->SetTextColor(available ? style.text_on_track : style.unavailable_text);
    CRect text_rect(lane_rect.left + 3, lane_rect.top, lane_rect.right - 3, lane_rect.bottom);
    pDC->DrawTextW(text.c_str(), -1, &text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    pDC->SetTextColor(old_text_color);
    pDC->SetBkMode(old_bk_mode);
}

void DrawClaudeOverview(CDC* pDC, int x, int y, int w, int h, bool dark_mode)
{
    if (pDC == nullptr || w <= 0 || h <= 0)
        return;

    const UsageDrawStyle style = GetUsageDrawStyle(UsageProvider::Claude, dark_mode);
    const CRect item_rect = StableContentRect(x, y, w, h);
    if (item_rect.IsRectEmpty())
        return;

    const int icon_size = max(10, min(14, item_rect.Height() - 2));
    const int icon_top = item_rect.top + (item_rect.Height() - icon_size) / 2;
    const bool use_color = UseClaudeColor(h);
    const COLORREF neutral_color = GetUsageDrawStyle(UsageProvider::Codex, dark_mode).fill;
    const COLORREF mark_color = use_color ? style.fill : neutral_color;
    DrawProviderMark(pDC, UsageProvider::Claude, CRect(item_rect.left, icon_top, item_rect.left + icon_size, icon_top + icon_size), mark_color);

    const CRect lane_rect(item_rect.left + icon_size + 4, item_rect.top, item_rect.right, item_rect.bottom);

    const UsageMetric five_hour = g_usage_core.GetMetric(UsageWindow::Claude5h);
    const UsageMetric seven_day = g_usage_core.GetMetric(UsageWindow::Claude7d);
    std::vector<UsageHistoryPoint> five_hour_history;
    for (const auto& point : g_usage_core.GetHistory(UsageWindow::Claude5h))
        five_hour_history.push_back({ point.timestamp_unix_seconds, point.percentage });
    std::vector<UsageHistoryPoint> seven_day_history;
    for (const auto& point : g_usage_core.GetHistory(UsageWindow::Claude7d))
        seven_day_history.push_back({ point.timestamp_unix_seconds, point.percentage });

    const COLORREF five_hour_color = use_color
        ? (dark_mode ? RGB(249, 174, 122) : RGB(202, 90, 34))
        : neutral_color;
    // The long window is the neutral background context. The bright orange
    // foreground is reserved for the actionable five-hour burn-out trace.
    const COLORREF seven_day_color = (dark_mode ? RGB(198, 198, 198) : RGB(126, 126, 126));
    const COLORREF seven_day_text_color = GetUsageDrawStyle(UsageProvider::Codex, dark_mode).text_on_track;
    DrawUsageHistoryLane(pDC, lane_rect, style, L"", seven_day.available, seven_day_history,
        7LL * 24LL * 60LL * 60LL, seven_day_color);
    DrawUsageHistoryLane(pDC, lane_rect, style, L"", five_hour.available, five_hour_history,
        5LL * 60LL * 60LL, five_hour_color);

    const std::wstring seven_day_text = FormatDisplayedPercentage(seven_day.available, seven_day.percentage);
    const std::wstring five_hour_text = FormatDisplayedPercentage(five_hour.available, five_hour.percentage);
    const bool weekly_first = g_claude_weekly_first.load(std::memory_order_relaxed);
    const std::wstring& first_text = weekly_first ? seven_day_text : five_hour_text;
    const std::wstring& second_text = weekly_first ? five_hour_text : seven_day_text;
    const bool first_available = weekly_first ? seven_day.available : five_hour.available;
    const bool second_available = weekly_first ? five_hour.available : seven_day.available;
    const COLORREF first_color = weekly_first ? seven_day_text_color : five_hour_color;
    const COLORREF second_color = weekly_first ? five_hour_color : seven_day_text_color;
    const int old_bk_mode = pDC->SetBkMode(TRANSPARENT);
    CRect text_rect(lane_rect.left + 3, lane_rect.top, lane_rect.right - 3, lane_rect.bottom);
    COLORREF old_text_color = pDC->SetTextColor(first_available ? first_color : style.unavailable_text);
    pDC->DrawTextW(first_text.c_str(), -1, &text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    text_rect.left += MeasureTextWidth(pDC, first_text.c_str()) + 3;
    pDC->SetTextColor(style.text_on_track);
    pDC->DrawTextW(L"/", -1, &text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    text_rect.left += MeasureTextWidth(pDC, L"/") + 3;
    pDC->SetTextColor(second_available ? second_color : style.unavailable_text);
    pDC->DrawTextW(second_text.c_str(), -1, &text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    pDC->SetTextColor(old_text_color);
    pDC->SetBkMode(old_bk_mode);
}

void DrawCodexOverview(CDC* pDC, int x, int y, int w, int h, bool dark_mode)
{
    if (pDC == nullptr || w <= 0 || h <= 0)
        return;

    const UsageDrawStyle style = GetUsageDrawStyle(UsageProvider::Codex, dark_mode);
    const CRect item_rect = StableContentRect(x, y, w, h);
    if (item_rect.IsRectEmpty())
        return;

    const int icon_size = max(10, min(14, item_rect.Height() - 2));
    const int icon_top = item_rect.top + (item_rect.Height() - icon_size) / 2;
    DrawProviderMark(pDC, UsageProvider::Codex, CRect(item_rect.left, icon_top, item_rect.left + icon_size, icon_top + icon_size), style.fill);

    std::vector<UsageHistoryPoint> history;
    for (const auto& point : g_usage_core.GetHistory(UsageWindow::Codex7d))
        history.push_back({ point.timestamp_unix_seconds, point.percentage });
    const UsageMetric metric = g_usage_core.GetMetric(UsageWindow::Codex7d);
    const std::wstring display_text = FormatDisplayedPercentage(metric.available, metric.percentage)
        + L" / " + FormatTimeUntilReset(metric.reset_at_unix_seconds);
    const CRect lane_rect(item_rect.left + icon_size + 4, item_rect.top, item_rect.right, item_rect.bottom);
    DrawUsageHistoryLane(pDC, lane_rect, style, display_text,
        metric.available, history, 7LL * 24LL * 60LL * 60LL, style.fill);
}

void DrawProviderHistoryItem(
    CDC* pDC,
    UsageProvider provider,
    bool draw_mark,
    const UsageDrawStyle& style,
    const std::wstring& text,
    bool available,
    const std::vector<UsageHistoryPoint>& history,
    long long window_seconds,
    int x,
    int y,
    int w,
    int h)
{
    if (pDC == nullptr || w <= 0 || h <= 0)
        return;

    const CRect item_rect(x + 1, y + 1, x + w - 1, y + h - 1);
    if (item_rect.IsRectEmpty())
        return;

    const int mark_size = max(12, min(26, item_rect.Height() - 2));
    const int mark_top = item_rect.top + (item_rect.Height() - mark_size) / 2;
    if (draw_mark)
        DrawProviderMark(pDC, provider, CRect(item_rect.left, mark_top, item_rect.left + mark_size, mark_top + mark_size), style.fill);

    // Reserve this same gutter even for Claude 7d so all three tracks have
    // exactly the same geometry once TrafficMonitor places the items.
    const CRect lane_rect(item_rect.left + mark_size + 4, item_rect.top, item_rect.right, item_rect.bottom);
    DrawUsageHistoryLane(pDC, lane_rect, style, text, available, history, window_seconds, style.fill);
}
}

CClaudeUsageItem::CClaudeUsageItem(UsageWindow window)
    : m_window(window)
{
}

const wchar_t* CClaudeUsageItem::GetItemName() const
{
    return L"Claude Usage";
}

const wchar_t* CClaudeUsageItem::GetItemId() const
{
    return (m_window == UsageWindow::Claude5h ? L"BetterClaudeUsage5Hours" : L"BetterClaudeUsage7Days");
}

const wchar_t* CClaudeUsageItem::GetItemLableText() const
{
    return (m_window == UsageWindow::Claude5h ? L"5h" : L"7d");
}

const wchar_t* CClaudeUsageItem::GetItemValueText() const
{
    const UsageMetric seven_day = g_usage_core.GetMetric(UsageWindow::Claude7d);
    const UsageMetric five_hour = g_usage_core.GetMetric(UsageWindow::Claude5h);
    const bool weekly_first = g_claude_weekly_first.load(std::memory_order_relaxed);
    m_value_text_cache = FormatDisplayedPercentage(
        weekly_first ? seven_day.available : five_hour.available,
        weekly_first ? seven_day.percentage : five_hour.percentage);
    m_value_text_cache += L" / ";
    m_value_text_cache += FormatDisplayedPercentage(
        weekly_first ? five_hour.available : seven_day.available,
        weekly_first ? five_hour.percentage : seven_day.percentage);
    return m_value_text_cache.c_str();
}

const wchar_t* CClaudeUsageItem::GetItemValueSampleText() const
{
    return L"99.9% / 99.9%";
}

bool CClaudeUsageItem::IsCustomDraw() const
{
    return true;
}

int CClaudeUsageItem::GetItemWidth() const
{
    return 90;
}

int CClaudeUsageItem::GetItemWidthEx(void* hDC) const
{
    CDC* pDC = CDC::FromHandle(static_cast<HDC>(hDC));
    if (pDC == nullptr)
        return GetItemWidth();

    return 90;
}

void CClaudeUsageItem::DrawItem(void* hDC, int x, int y, int w, int h, bool dark_mode)
{
    CDC* pDC = CDC::FromHandle(static_cast<HDC>(hDC));
    if (pDC == nullptr || w <= 0 || h <= 0)
        return;

    DrawClaudeOverview(pDC, x, y, w, h, dark_mode);
}

CCodexUsageItem::CCodexUsageItem(UsageWindow window)
    : m_window(window)
{
}

const wchar_t* CCodexUsageItem::GetItemName() const
{
    return L"Codex Usage";
}

const wchar_t* CCodexUsageItem::GetItemId() const
{
    return (m_window == UsageWindow::Codex7d ? L"BetterCodexUsage7Days" : L"BetterCodexUsage5Hours");
}

const wchar_t* CCodexUsageItem::GetItemLableText() const
{
    return (m_window == UsageWindow::Codex7d ? L"7d" : L"5h");
}

const wchar_t* CCodexUsageItem::GetItemValueText() const
{
    const UsageMetric metric = g_usage_core.GetMetric(UsageWindow::Codex7d);
    m_value_text_cache = FormatDisplayedPercentage(metric.available, metric.percentage);
    m_value_text_cache += L" / ";
    m_value_text_cache += FormatTimeUntilReset(metric.reset_at_unix_seconds);
    return m_value_text_cache.c_str();
}

const wchar_t* CCodexUsageItem::GetItemValueSampleText() const
{
    return L"99.9% / 7d";
}

bool CCodexUsageItem::IsCustomDraw() const
{
    return true;
}

int CCodexUsageItem::GetItemWidth() const
{
    return 90;
}

int CCodexUsageItem::GetItemWidthEx(void* hDC) const
{
    CDC* pDC = CDC::FromHandle(static_cast<HDC>(hDC));
    if (pDC == nullptr)
        return GetItemWidth();

    return 90;
}

void CCodexUsageItem::DrawItem(void* hDC, int x, int y, int w, int h, bool dark_mode)
{
    CDC* pDC = CDC::FromHandle(static_cast<HDC>(hDC));
    if (pDC == nullptr || w <= 0 || h <= 0)
        return;

    DrawCodexOverview(pDC, x, y, w, h, dark_mode);
}

CBetterTrafficMonitorAiUsagePlugin& CBetterTrafficMonitorAiUsagePlugin::Instance()
{
    static CBetterTrafficMonitorAiUsagePlugin instance;
    return instance;
}

IPluginItem* CBetterTrafficMonitorAiUsagePlugin::GetItem(int index)
{
    switch (index)
    {
    case 0:
        return &m_five_hour_item;
    case 1:
        return &m_codex_seven_day_item;
    default:
        return nullptr;
    }
}

void CBetterTrafficMonitorAiUsagePlugin::OnInitialize(ITrafficMonitor* pApp)
{
    if (pApp == nullptr || pApp->GetPluginConfigDir() == nullptr)
        return;

    m_config_path = pApp->GetPluginConfigDir();
    if (!m_config_path.empty() && m_config_path.back() != L'\\')
        m_config_path += L'\\';
    m_config_path += L"BetterTrafficMonitorAiUsage.ini";

    const int mode = GetPrivateProfileIntW(L"Display", L"Mode", 0, m_config_path.c_str());
    const int layout = GetPrivateProfileIntW(L"Display", L"SingleItemLayout", 0, m_config_path.c_str());
    const int color = GetPrivateProfileIntW(L"Display", L"ColorMode", 1, m_config_path.c_str());
    const int claude_weekly_first = GetPrivateProfileIntW(L"Display", L"ClaudeWeeklyFirst", 1, m_config_path.c_str());
    g_graph_display_mode.store(mode == 1 ? GraphDisplayMode::Used : GraphDisplayMode::Remaining, std::memory_order_relaxed);
    g_single_item_layout.store(layout >= 0 && layout <= 3 ? static_cast<SingleItemLayout>(layout) : SingleItemLayout::Center, std::memory_order_relaxed);
    g_color_mode.store(color >= 0 && color <= 2 ? static_cast<ColorMode>(color) : ColorMode::Adaptive, std::memory_order_relaxed);
    g_claude_weekly_first.store(claude_weekly_first != 0, std::memory_order_relaxed);
}

void CBetterTrafficMonitorAiUsagePlugin::DataRequired()
{
    g_usage_core.RefreshIfNeeded();
}

ITMPlugin::OptionReturn CBetterTrafficMonitorAiUsagePlugin::ShowOptionsDialog(void* hParent)
{
    // TrafficMonitor calls this method from the host executable. Switch MFC's
    // module state before looking up the dialog resource embedded in this DLL.
    AFX_MANAGE_STATE(AfxGetStaticModuleState());

    const GraphDisplayMode current_graph = g_graph_display_mode.load(std::memory_order_relaxed);
    const SingleItemLayout current_layout = g_single_item_layout.load(std::memory_order_relaxed);
    const ColorMode current_color = g_color_mode.load(std::memory_order_relaxed);
    const bool current_claude_weekly_first = g_claude_weekly_first.load(std::memory_order_relaxed);
    CDisplayOptionsDialog dialog(
        static_cast<int>(current_graph),
        static_cast<int>(current_layout),
        static_cast<int>(current_color),
        current_claude_weekly_first,
        CWnd::FromHandle(static_cast<HWND>(hParent)));
    if (dialog.DoModal() != IDOK)
        return OR_OPTION_UNCHANGED;

    const GraphDisplayMode chosen_graph = static_cast<GraphDisplayMode>(dialog.GraphMode());
    const SingleItemLayout chosen_layout = static_cast<SingleItemLayout>(dialog.SingleItemLayout());
    const ColorMode chosen_color = static_cast<ColorMode>(dialog.ColorMode());
    const bool chosen_claude_weekly_first = dialog.ClaudeWeeklyFirst();
    if (chosen_graph == current_graph && chosen_layout == current_layout && chosen_color == current_color
        && chosen_claude_weekly_first == current_claude_weekly_first)
        return OR_OPTION_UNCHANGED;

    g_graph_display_mode.store(chosen_graph, std::memory_order_relaxed);
    g_single_item_layout.store(chosen_layout, std::memory_order_relaxed);
    g_color_mode.store(chosen_color, std::memory_order_relaxed);
    g_claude_weekly_first.store(chosen_claude_weekly_first, std::memory_order_relaxed);
    if (!m_config_path.empty())
    {
        WritePrivateProfileStringW(L"Display", L"Mode", chosen_graph == GraphDisplayMode::Used ? L"1" : L"0", m_config_path.c_str());
        WritePrivateProfileStringW(L"Display", L"SingleItemLayout", std::to_wstring(dialog.SingleItemLayout()).c_str(), m_config_path.c_str());
        WritePrivateProfileStringW(L"Display", L"ColorMode", std::to_wstring(dialog.ColorMode()).c_str(), m_config_path.c_str());
        WritePrivateProfileStringW(L"Display", L"ClaudeWeeklyFirst", chosen_claude_weekly_first ? L"1" : L"0", m_config_path.c_str());
    }
    return OR_OPTION_CHANGED;
}

const wchar_t* CBetterTrafficMonitorAiUsagePlugin::GetInfo(PluginInfoIndex index)
{
    static std::wstring value;
    switch (index)
    {
    case TMI_NAME:
        value = L"Better TrafficMonitor AI Usage";
        break;
    case TMI_DESCRIPTION:
        value = L"Local Claude Desktop and Codex usage limits for TrafficMonitor.";
        break;
    case TMI_AUTHOR:
        value = L"QQRM";
        break;
    case TMI_COPYRIGHT:
        value = L"Copyright (C) 2026 Better TrafficMonitor AI Usage contributors";
        break;
    case TMI_VERSION:
        value = L"1.2.1";
        break;
    case TMI_URL:
        value = L"https://github.com/qqrm/better-trafficmonitor-ai-usage-plugin";
        break;
    default:
        value.clear();
        break;
    }
    return value.c_str();
}

const wchar_t* CBetterTrafficMonitorAiUsagePlugin::GetTooltipInfo()
{
    g_usage_core.RefreshIfNeeded();

    const UsageMetric claude_five_hour = g_usage_core.GetMetric(UsageWindow::Claude5h);
    const UsageMetric claude_seven_day = g_usage_core.GetMetric(UsageWindow::Claude7d);
    const UsageMetric codex_seven_day = g_usage_core.GetMetric(UsageWindow::Codex7d);
    if (g_claude_weekly_first.load(std::memory_order_relaxed))
    {
        m_tooltip_text_cache = L"Claude 7d: " + FormatLimitDetails(claude_seven_day);
        m_tooltip_text_cache += L"\nClaude 5h: " + FormatLimitDetails(claude_five_hour);
    }
    else
    {
        m_tooltip_text_cache = L"Claude 5h: " + FormatLimitDetails(claude_five_hour);
        m_tooltip_text_cache += L"\nClaude 7d: " + FormatLimitDetails(claude_seven_day);
    }
    m_tooltip_text_cache += L"\nClaude next reset: " + FormatResetTime(g_usage_core.GetClaudeNextResetAtUnixSeconds());
    m_tooltip_text_cache += L"\n\nCodex 7d: " + FormatLimitDetails(codex_seven_day) + L"; resets: " + FormatResetTime(codex_seven_day.reset_at_unix_seconds);
    return m_tooltip_text_cache.c_str();
}

ITMPlugin* TMPluginGetInstance()
{
    AFX_MANAGE_STATE(AfxGetStaticModuleState());
    return &CBetterTrafficMonitorAiUsagePlugin::Instance();
}
