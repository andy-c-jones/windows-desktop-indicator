# Desktop Indicator

<img width="97" height="44" alt="image" src="https://github.com/user-attachments/assets/dc5b036c-3a60-41ee-a2b9-b77c7c09e48a" />

<img width="263" height="174" alt="image" src="https://github.com/user-attachments/assets/1dc97821-97c8-4148-91be-37ec8542dfbf" />


A tiny native (Rust, ~150 KB, no runtime) virtual desktop indicator that sits on the Windows 11 taskbar.

- Shows a numbered tile per virtual desktop in a Fluent style; the current one is filled with your accent colour
  and slides smoothly when you switch.
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

## Download

Grab the latest single-file exe from [Releases](https://github.com/andy-c-jones/windows-desktop-indicator/releases/latest):

- [`desktop-indicator-x64.exe`](https://github.com/andy-c-jones/windows-desktop-indicator/releases/latest/download/desktop-indicator-x64.exe) — most PCs
- [`desktop-indicator-arm64.exe`](https://github.com/andy-c-jones/windows-desktop-indicator/releases/latest/download/desktop-indicator-arm64.exe) — Windows on ARM (Snapdragon, etc.)

Put it anywhere and run it; enable *Start with Windows* from the tray menu. `SHA256SUMS.txt` is attached to each release.
The exe is not code-signed, so SmartScreen may warn on first run (*More info → Run anyway*), and Smart App Control,
if enabled, may block it.

## Build

Requires Rust (`winget install Rustlang.Rustup`) and the MSVC linker
(`winget install Microsoft.VisualStudio.2022.BuildTools` with the *Desktop development with C++* workload).

```powershell
cargo build --release
.\target\release\desktop-indicator.exe
```

## CI and releases

- **CI** (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `clippy -D warnings` and the tests on every push and PR,
  then builds x64 and arm64 executables as workflow artifacts.
- **Labeler** (`.github/labeler.yml`) labels PRs by changed files and branch prefix (`feat/…`, `fix/…`).
- **Release** is automated with [release-please](https://github.com/googleapis/release-please) and only runs on `main`
  (`.github/workflows/release.yml`). On every push to `main` it keeps a release PR (`chore(main): release x.y.z`) open
  that bumps the version in `Cargo.toml`/`Cargo.lock` and updates `CHANGELOG.md` from the Conventional Commit messages
  since the last release: `fix:` → patch, `feat:` → minor, `!`/`BREAKING CHANGE:` → major.
  Merging that PR tags `main` (`vX.Y.Z`), creates the GitHub release, runs CI and attaches both exes and checksums.
  Don't bump the version or push tags by hand. To force a specific version, add a `Release-As: 1.2.3` footer to a commit.

## Contributing

`main` is protected, so all changes go through a pull request. Commit messages and PR titles must follow
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/), and the `Conventional commits` workflow
(`.github/workflows/conventional-commits.yml`) fails PRs that don't:

```
<type>(<optional scope>)!: <description>

feat: add per-monitor placement
fix(render): clip highlight glow on high DPI
```

Allowed types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
Merge commits are exempt. Use a matching branch prefix (`feat/…`, `fix/…`) so the PR is labelled.
The commit type decides the next release version and changelog entry (see *Release* above).

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
