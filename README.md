# Switch

A fast, lightweight, modern Alt-Tab replacement for Windows 10 and 11.

![Switch icon](assets/switch.png)

- **Takes over Alt+Tab**: Shift+Alt+Tab goes back, Ctrl+Alt+Tab keeps it open. You can also add a custom shortcut, or make every session stay open until Enter
- **Alt+`** cycles through the windows of the app you're in
- **Live previews** from DWM thumbnails, the compositor feature the taskbar uses, so they're live and cost almost nothing
- **Three layouts**: Grid, single Row, or a compact List with a large live preview of the selected window (Ctrl+L cycles them)
- **Fuzzy type-to-search**: "vsc" finds Visual Studio Code, and matched letters are highlighted
- **Grouping**: off, windows of the same app kept together, or collapsed to one tile per app (Ctrl+G cycles them). Sort by recent, app or title
- **Window actions** from the keyboard, the hover buttons or the right-click menu: close, close every window of an app, minimize, maximize, pin on top, move to another monitor, new window, open file location, hide the app
- **Multi-monitor and virtual desktops**: open on the cursor's monitor, the active window's monitor or the primary one; show all monitors or only the current one; optionally include other virtual desktops
- **Customizable look**: System, Light, Dark or pure-Black theme; Windows or custom accent color; corner radius; text size; selection style; animation speed; desktop dimming; position (center, top or bottom); labels; icons; search bar; key hints. The settings window has a live preview
- **Lightweight**: about 0.7 MB, with no runtime and nothing running while the switcher is hidden. Memory goes back to Windows when idle, and animations are vsync-paced
- **Tray app** with a sidebar settings window, Start with Windows, Restart as administrator, and a **built-in updater** that shows release notes and can install updates automatically

## Install

1. Download `Switch.exe` from [Releases](https://github.com/larosa023/Switch/releases/latest).
2. Put it in a folder you own, e.g. `%LOCALAPPDATA%\Programs\Switch\` (the updater has to be able to replace it, so avoid `Program Files`).
3. Run it. Switch lives in the tray. Turn on **Start with Windows** in Settings.

It's a single exe of under 1 MB, with no installer and no runtime. Settings are kept in `%APPDATA%\Switch\settings.json`.

## Using it

| Key | Action |
| --- | --- |
| Alt+Tab / Shift+Alt+Tab | Open, then move forward or back. Release Alt to switch |
| Ctrl+Alt+Tab | Open and keep it open (sticky) |
| Alt+` | Windows of the current app only |
| Type anything | Fuzzy search. Typing makes the session sticky, so you can let go of Alt |
| Enter / click | Switch to the selected window |
| Arrows, Tab, PgUp/PgDn, Home/End | Move the selection |
| Esc | Clear the search, then close |
| Del, Ctrl+W, middle-click | Close the window (Ctrl+Shift+W closes every window of that app) |
| Ctrl+M / Ctrl+Up | Minimize / maximize or restore |
| Ctrl+Left / Ctrl+Right | Move the window to the previous / next monitor |
| Ctrl+P | Pin the window always on top (toggle) |
| Ctrl+N / Ctrl+O | New window of the app / open its file location |
| Ctrl+H | Hide this app from Switch (undo in Settings → Hidden apps) |
| Ctrl+G / Ctrl+L | Cycle grouping / layout |
| Right-click, Menu key | Every action for that window |

Command line options: `--open` (open the switcher, handy for mouse buttons or a Stream Deck), `--settings`, `--quit`, `--background`.

## Updates

With **Check for updates automatically** on, Switch checks GitHub Releases about 20 seconds after it starts and then every 6 hours. When a new version is out, a notification appears and Settings → Updates shows the release notes. Click the notification, or use the tray menu or Settings, to install. You can also have updates install automatically. The download only goes ahead if its SHA-256 matches `Switch.exe.sha256` from the same release. Switch then swaps the exe and restarts itself.

### Publishing a release

1. Bump `version` in `Cargo.toml` and commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`
3. The Release workflow builds `Switch.exe`, writes `Switch.exe.sha256` and publishes both. Every installed copy will pick the release up.

## Building

**On Windows** (recommended):

```powershell
winget install Rustlang.Rustup   # once
cargo build --release            # -> target\release\Switch.exe
```

**Cross-compiling from Linux/macOS:** install `mingw-w64`, run `rustup target add x86_64-pc-windows-gnu`, then `./build.sh`.

## How it works

| Piece | Approach |
| --- | --- |
| Language | Rust with the official `windows` crate. Native Win32, no framework, no web view |
| Alt+Tab capture | `WH_KEYBOARD_LL` hook on its own high-priority thread, so a busy UI can never delay your typing. It swallows Tab while Alt is held, and a dummy keystroke stops the focused app's menu bar from activating when Alt is released |
| Previews | `DwmRegisterThumbnail`: live, GPU-composited, and no capture or copying |
| Drawing | Direct2D and DirectWrite on a pre-created hidden window, so the first open is instant |
| Idle cost | Nothing runs while hidden apart from the 6-hour update check. Animation frames are paced by `DwmFlush` (vsync) and stop once motion settles. Ten seconds after the switcher closes, its working set is handed back to Windows |
| Updater | WinHTTP (honours the system proxy, adds no TLS library), SHA-256 verified, atomic rename swap |

```
src/
  main.rs         entry point, single instance, tray icon and menu, update notifications
  hook.rs         low-level keyboard hook and session state shared with the UI
  switcher.rs     overlay window: grid/row/list layouts, grouping, DWM thumbnails, actions, animation
  fuzzy.rs        fuzzy matcher and highlight ranges for search
  winlist.rs      Alt-Tab-eligible window enumeration, app names and icons, window actions
  settings_ui.rs  custom-drawn settings window: sidebar pages, sliders, text fields, live preview
  gfx.rs          Direct2D/DirectWrite/WIC helpers, theme palette, vector glyphs
  config.rs       settings.json and Start with Windows (HKCU Run key)
  updater.rs      GitHub Releases check, verified download, self-replace and relaunch
  util.rs         wide strings, registry, logging
assets/           icon and app manifest (Per-Monitor-V2 DPI)
.github/workflows CI build and tag-triggered releases
```

## Known limitations

- **Elevated windows.** When an app running as administrator is focused, Windows doesn't deliver its keystrokes to a non-elevated hook, so the built-in Alt+Tab appears instead. Use Settings → General → **Restart as admin** to work around this.
- **SmartScreen.** The exe isn't code-signed yet, so the first run shows "Windows protected your PC". Choose *More info → Run anyway*.
- Minimized windows whose app doesn't keep a preview show their icon in place of a live thumbnail.

Problems are logged to `%APPDATA%\Switch\switch.log`.
