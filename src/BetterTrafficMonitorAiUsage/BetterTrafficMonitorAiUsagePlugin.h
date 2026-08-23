#pragma once

#include "UsageCoreAdapter.h"
#include "PluginInterface.h"

class CClaudeUsageItem : public IPluginItem
{
public:
    explicit CClaudeUsageItem(UsageWindow window);

    const wchar_t* GetItemName() const override;
    const wchar_t* GetItemId() const override;
    const wchar_t* GetItemLableText() const override;
    const wchar_t* GetItemValueText() const override;
    const wchar_t* GetItemValueSampleText() const override;
    bool IsCustomDraw() const override;
    int GetItemWidth() const override;
    int GetItemWidthEx(void* hDC) const override;
    void DrawItem(void* hDC, int x, int y, int w, int h, bool dark_mode) override;

private:
    UsageWindow m_window;
    mutable std::wstring m_value_text_cache;
};

class CCodexUsageItem : public IPluginItem
{
public:
    explicit CCodexUsageItem(UsageWindow window);

    const wchar_t* GetItemName() const override;
    const wchar_t* GetItemId() const override;
    const wchar_t* GetItemLableText() const override;
    const wchar_t* GetItemValueText() const override;
    const wchar_t* GetItemValueSampleText() const override;
    bool IsCustomDraw() const override;
    int GetItemWidth() const override;
    int GetItemWidthEx(void* hDC) const override;
    void DrawItem(void* hDC, int x, int y, int w, int h, bool dark_mode) override;

private:
    UsageWindow m_window;
    mutable std::wstring m_value_text_cache;
};

class CBetterTrafficMonitorAiUsagePlugin : public ITMPlugin
{
private:
    CBetterTrafficMonitorAiUsagePlugin() = default;

public:
    static CBetterTrafficMonitorAiUsagePlugin& Instance();

    IPluginItem* GetItem(int index) override;
    void OnInitialize(ITrafficMonitor* pApp) override;
    void DataRequired() override;
    OptionReturn ShowOptionsDialog(void* hParent) override;
    const wchar_t* GetInfo(PluginInfoIndex index) override;
    const wchar_t* GetTooltipInfo() override;

private:
    CClaudeUsageItem m_five_hour_item{ UsageWindow::Claude5h };
    CCodexUsageItem m_codex_seven_day_item{ UsageWindow::Codex7d };
    std::wstring m_config_path;
    std::wstring m_tooltip_text_cache;
};

#ifdef __cplusplus
extern "C" {
#endif
    __declspec(dllexport) ITMPlugin* TMPluginGetInstance();
#ifdef __cplusplus
}
#endif
