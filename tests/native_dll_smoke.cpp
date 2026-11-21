// Load the packaged production DLL. This is an ABI/GDI smoke host, not TrafficMonitor.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <algorithm>
#include <climits>
#include <cstring>
#include <iostream>
#include <set>
#include <stdexcept>
#include <string>

// The upstream SDK's default virtual no-op callbacks name unused parameters.
// Keep the vendored ABI header intact; own code is still compiled with /W4 /WX.
#pragma warning(push)
#pragma warning(disable: 4100)
#include "PluginInterface.h"
#pragma warning(pop)

static void Require(bool condition, const char* message)
{
    if (!condition) throw std::runtime_error(message);
}

class SmokeHost final : public ITrafficMonitor
{
public:
    explicit SmokeHost(const wchar_t* directory) : directory_(directory) {}
    int GetAPIVersion() override { return 1; }
    const wchar_t* GetVersion() override { return L"native-dll-smoke"; }
    double GetMonitorValue(MonitorItem) override { return 0.0; }
    const wchar_t* GetMonitorValueString(MonitorItem, int) override { return L""; }
    void ShowNotifyMessage(const wchar_t*) override {}
    unsigned short GetLanguageId() const override { return 0x409; }
    const wchar_t* GetPluginConfigDir() const override { return directory_.c_str(); }
    int GetDPI(DPIType) const override { return 96; }
    unsigned int GetThemeColor() const override { return RGB(40, 80, 120); }
    const wchar_t* GetStringRes(const wchar_t*, const wchar_t*) override { return L""; }
private:
    std::wstring directory_;
};

struct Module
{
    HMODULE value{};
    ~Module() { if (value != nullptr) FreeLibrary(value); }
};

struct Canvas
{
    HDC dc{};
    HBITMAP bitmap{};
    HGDIOBJ previous{};
    DWORD* pixels{};
    static constexpr int width = 256;
    static constexpr int height = 64;
    ~Canvas()
    {
        if (previous != nullptr) SelectObject(dc, previous);
        if (bitmap != nullptr) DeleteObject(bitmap);
        if (dc != nullptr) DeleteDC(dc);
    }
    void Create()
    {
        dc = CreateCompatibleDC(nullptr);
        if (dc == nullptr) throw std::runtime_error("CreateCompatibleDC failed");
        BITMAPINFO info{};
        info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        void* memory = nullptr;
        bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &memory, nullptr, 0);
        if (bitmap == nullptr || memory == nullptr)
            throw std::runtime_error("CreateDIBSection failed");
        pixels = static_cast<DWORD*>(memory);
        const HGDIOBJ selected = SelectObject(dc, bitmap);
        if (selected == nullptr || selected == HGDI_ERROR)
            throw std::runtime_error("SelectObject failed");
        previous = selected;
    }
};

int wmain(int argc, wchar_t** argv)
{
    try
    {
        Require(argc == 4, "usage: native_dll_smoke DLL config-directory expected-version");
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        Module module{ LoadLibraryW(argv[1]) };
        Require(module.value != nullptr, "production DLL failed to load");
        const FARPROC entry = GetProcAddress(module.value, "TMPluginGetInstance");
        Require(entry != nullptr, "TMPluginGetInstance export is missing");
        using Factory = ITMPlugin* (*)();
        static_assert(sizeof(Factory) == sizeof(entry));
        Factory factory{};
        std::memcpy(&factory, &entry, sizeof(factory));
        ITMPlugin* plugin = factory();
        if (plugin == nullptr) throw std::runtime_error("plugin factory returned null");
        Require(plugin == factory(), "plugin singleton is invalid");
        Require(plugin->GetAPIVersion() == 7, "unexpected TrafficMonitor plugin ABI");
        Require(std::wstring(plugin->GetInfo(ITMPlugin::TMI_VERSION)) == argv[3], "reported version differs from package");
        plugin->OnInitialize(nullptr);
        SmokeHost host(argv[2]);
        plugin->OnInitialize(&host);
        for (const int invalid : { INT_MIN, -1, 3, INT_MAX })
            Require(plugin->GetItem(invalid) == nullptr, "out-of-range item index did not return null");

        Canvas canvas;
        canvas.Create();
        std::set<std::wstring> identifiers;
        for (int index = 0; index < 3; ++index)
        {
            IPluginItem* item = plugin->GetItem(index);
            if (item == nullptr) throw std::runtime_error("configured provider item is missing");
            const std::wstring id(item->GetItemId());
            Require(!id.empty() && identifiers.insert(id).second, "provider IDs must be nonempty and unique");
            Require(item->GetItemName() != nullptr && item->GetItemValueText() != nullptr, "null item text");
            Require(item->GetItemValueSampleText() != nullptr, "null sample text");
            Require(item->IsCustomDraw() && item->GetItemWidth() > 0, "invalid drawing contract");
            Require(item->GetItemWidthEx(canvas.dc) > 0, "invalid device-context width");
            item->DrawItem(nullptr, 0, 0, 0, 0, false);
            item->DrawItem(canvas.dc, 0, 0, 0, -1, false);
            item->DrawItem(canvas.dc, 0, 0, 1, 1, false);
            for (const bool dark : { false, true })
            {
                constexpr DWORD sentinel = 0x11335577;
                std::fill_n(canvas.pixels, Canvas::width * Canvas::height, sentinel);
                item->DrawItem(canvas.dc, 0, 0, Canvas::width, Canvas::height, dark);
                GdiFlush();
                Require(std::any_of(canvas.pixels, canvas.pixels + Canvas::width * Canvas::height,
                    [](DWORD pixel) { return pixel != sentinel; }), "DrawItem painted no pixels");
            }
        }
        Require(plugin->GetTooltipInfo() != nullptr, "tooltip is null");

        const std::wstring config = std::wstring(argv[2]) + L"\\BetterTrafficMonitorAiUsage.ini";
        Require(WritePrivateProfileStringW(L"Display", L"ShowClaude", L"0", config.c_str()) != FALSE,
            "cannot write isolated smoke configuration");
        plugin->OnInitialize(&host);
        Require(plugin->GetItem(0) != nullptr && std::wstring(plugin->GetItem(0)->GetItemId()) == L"BetterCodexUsage7Days",
            "hidden provider still occupies the first item slot");
        Require(plugin->GetItem(1) != nullptr && plugin->GetItem(2) == nullptr,
            "provider visibility did not update item bounds");
        std::cout << "PASS production DLL: factory, API, version, index bounds, provider IDs, "
                     "initialization, cached text, GDI drawing, visibility configuration\n";
        return 0;
    }
    catch (const std::exception& error)
    {
        std::cerr << "FAIL production DLL: " << error.what() << '\n';
        return 1;
    }
}
