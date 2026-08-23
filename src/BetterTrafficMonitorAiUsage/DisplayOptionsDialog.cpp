#include "pch.h"
#include "DisplayOptionsDialog.h"

CDisplayOptionsDialog::CDisplayOptionsDialog(int graph_mode, int single_item_layout, int color_mode, CWnd* parent)
    : CDialogEx(IDD_DISPLAY_OPTIONS, parent)
    , m_graph_mode(graph_mode)
    , m_single_item_layout(single_item_layout)
    , m_color_mode(color_mode)
{
}

BOOL CDisplayOptionsDialog::OnInitDialog()
{
    CDialogEx::OnInitDialog();
    CheckRadioButton(IDC_GRAPH_REMAINING, IDC_GRAPH_USED, IDC_GRAPH_REMAINING + m_graph_mode);
    CheckRadioButton(IDC_LAYOUT_CENTER, IDC_LAYOUT_STRETCH, IDC_LAYOUT_CENTER + m_single_item_layout);
    CheckRadioButton(IDC_COLOR_MONOCHROME, IDC_COLOR_ALWAYS, IDC_COLOR_MONOCHROME + m_color_mode);
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

    CDialogEx::OnOK();
}
