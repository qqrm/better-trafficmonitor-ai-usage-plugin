#pragma once

#include <afxdialogex.h>
#include "resource.h"

class CDisplayOptionsDialog final : public CDialogEx
{
public:
    CDisplayOptionsDialog(int graph_mode, int single_item_layout, int color_mode, bool claude_weekly_first, CWnd* parent = nullptr);

    int GraphMode() const { return m_graph_mode; }
    int SingleItemLayout() const { return m_single_item_layout; }
    int ColorMode() const { return m_color_mode; }
    bool ClaudeWeeklyFirst() const { return m_claude_weekly_first; }

protected:
    BOOL OnInitDialog() override;
    void OnOK() override;

private:
    int m_graph_mode{};
    int m_single_item_layout{};
    int m_color_mode{};
    bool m_claude_weekly_first{};
};
