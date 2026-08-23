# Better TrafficMonitor AI Usage

A native [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor) plug-in that displays local Claude and Codex usage history directly in the taskbar widget.

It reads data already stored by the desktop clients on the same Windows machine. It does not open a browser, request a web-session token, start a helper process, or make network requests.

![Claude and Codex usage graphs in the TrafficMonitor taskbar widget](docs/images/taskbar-preview.png)

## Display items

- `Claude Usage`: Claude five-hour and seven-day burn-down history in one compact row.
- `Codex Usage`: Codex seven-day burn-down history.

The graph can display either remaining capacity (100% to 0%) or used capacity (0% to 100%). Claude can be monochrome, adaptive, or always colored. A single item in a tall taskbar cell can be centered, placed at the top or bottom, or stretched.

Hovering over the widget shows each window's used and remaining percentage. It also shows the next reset time that Claude Desktop or Codex has recently persisted locally. The Claude reset is shown as a provider-wide `next reset` because the local record does not identify which displayed window it belongs to.

## Install

1. Download the x64 ZIP from the latest GitHub release.
2. Extract it into the directory containing `TrafficMonitor.exe`. The result must contain:

   ```text
   TrafficMonitor\
   ├── TrafficMonitor.exe
   └── plugins\
       └── BetterTrafficMonitorAiUsage.dll
   ```

3. Restart TrafficMonitor.
4. Open the taskbar widget's `Display Settings...` and enable `Claude Usage` and/or `Codex Usage`.
5. Open `Other Functions → Plugin Management`, select `Better TrafficMonitor AI Usage`, and click `Options...` to configure the graphs.

The plug-in currently ships for x64 TrafficMonitor. The DLL architecture must match TrafficMonitor's architecture.

## Local data sources

- Claude Desktop: `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude\plan-usage-history.json`
- Codex: `%CODEX_HOME%\sessions\**\*.jsonl`, or `%USERPROFILE%\.codex\sessions\**\*.jsonl` when `CODEX_HOME` is unset

Data is refreshed at most once every 30 seconds. Claude data older than 20 minutes is treated as unavailable. Codex history is bounded to the newest 24 session files, 2 MiB per file, and seven days of samples.

## Build

Requirements:

- Windows with Visual Studio 2022 Build Tools, the v143 C++ toolset, and MFC
- Stable Rust with the `x86_64-pc-windows-msvc` host/target

Run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-release.ps1
```

The installable ZIP and SHA-256 checksum are written to `dist\`.

## Privacy

The usage core only performs bounded local file reads. It does not authenticate to Claude or Codex, transmit credentials, invoke subprocesses, or access the network.

## Author

QQRM

Claude, Anthropic, Codex, and OpenAI names and marks belong to their respective owners. This project is not affiliated with or endorsed by Anthropic, OpenAI, or the TrafficMonitor project.
