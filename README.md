# Desktop Indicator

A tiny native (Rust, ~150 KB, no runtime) virtual desktop indicator that sits on the Windows 11 taskbar.

- Shows a numbered pill per virtual desktop; the current one is filled with your accent colour.
- **Click** a number to switch to that desktop; **scroll** over it to move left/right.
- Optional **notification dot** on desktops where an app is requesting attention (flashing taskbar button).
- Placement **Automatic**: left side when taskbar icons are centred, centre when icons are left-aligned
  (follows *Settings → Personalization → Taskbar → Taskbar alignment* live). Can be forced to Left/Centre.
- Matches Windows styling live: the system UI font (Segoe UI Variable on Windows 11, or your custom system font),
  the accent palette shade Windows uses for the current theme, readable text on the accent, light/dark mode,
  "Show accent colour on taskbar", high-contrast themes, DPI and taskbar position.
- One indicator per taskbar, on every monitor that shows one (*Show my taskbar on all displays*).
- Hides while a full-screen app is in the foreground.

Settings are in the tray icon menu (or right-click the indicator): notification dots, position, start with Windows, exit.

## Build

Requires Rust (`winget install Rustlang.Rustup`) and the MSVC linker
(`winget install Microsoft.VisualStudio.2022.BuildTools` with the *Desktop development with C++* workload).

```powershell
cargo build --release
.\target\release\desktop-indicator.exe
```

## How it works

| Feature | Mechanism |
| --- | --- |
| Desktop list / current | `HKCU\...\Explorer\VirtualDesktops` (`VirtualDesktopIDs`, `CurrentVirtualDesktop`), polled every 200 ms |
| Switching | Simulated `Ctrl+Win+←/→` (stable across builds, unlike the undocumented internal COM API) |
| Notification dots | Shell hook `HSHELL_FLASH` + documented `IVirtualDesktopManager::GetWindowDesktopId`; cleared when the window is activated or closed |
| Drawing | Per-pixel-alpha layered window owned by `Shell_TrayWnd`, so it stays above the taskbar on every desktop |

Settings are stored in `HKCU\Software\DesktopIndicator`.

## Limitations

- Notification dots only appear for apps that flash their taskbar button (Teams, Slack, Discord, Outlook reminders, etc.);
  taskbar badge counts are not exposed by a public API.
- It is an overlay positioned over the taskbar, not a true taskbar component, so it can overlap the Widgets button
  if that is enabled on the left.
