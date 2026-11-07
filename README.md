# Switch

A fast, lightweight, modern Alt-Tab replacement for Windows 10 and 11.

![Switch icon](assets/switch.png)

- **Takes over Alt+Tab** (Shift+Alt+Tab goes backwards, Ctrl+Alt+Tab keeps it open), with an optional custom shortcut
- **Large live previews** using DWM thumbnails, the same compositor feature the taskbar uses, so previews are live and cost almost nothing
- **Type to search** by window title or app name as soon as the switcher is open
- **Group by app** (Ctrl+G), grid or single-row layout, three preview sizes
- **Keyboard first**: arrows, Tab, Home/End, Enter, Esc. Mouse works too (click, hover buttons, middle-click to close, wheel to move)
- **Window actions**: close, minimize, pin on top, or hide an app from Switch entirely
- **Multi-monitor**: opens on the monitor with your cursor and shows windows from all monitors or only that one
- **Themes**: System, Light or Dark, with the Windows accent color or a preset
- **Tray app** with a settings window, Start with Windows, and a **built-in updater**

## Install

1. Download `Switch.exe` from [Releases](https://github.com/larosa023/Switch/releases/latest).
2. Put it in a folder you own, e.g. `%LOCALAPPDATA%\Programs\Switch\` (the updater has to be able to replace it, so avoid `Program Files`).
3. Run it. Switch lives in the tray. Turn on **Start with Windows** in Settings.

It's a single ~700 KB exe with no installer and no runtime. Settings are kept in `%APPDATA%\Switch\settings.json`.

## Using it

| Key | Action |
| --- | --- |
| Alt+Tab / Shift+Alt+Tab | Open, then move forward or back. Release Alt to switch |
| Ctrl+Alt+Tab | Open and keep it open (sticky) |
| Type anything | Search. Typing makes the session sticky, so you can let go of Alt |
| Enter / click | Switch to the selected window |
| Arrows, Tab, Home, End | Move the selection |
| Esc | Clear the search, then close |
| Del, Ctrl+W, middle-click | Close the window |
| Ctrl+M | Minimize the window |
| Ctrl+P | Pin the window always on top (toggle) |
| Ctrl+H | Hide this app from Switch (undo in Settings → Hidden apps) |
| Ctrl+G | Toggle grouping by app |

`Switch.exe --open` opens the switcher in sticky mode, which is handy for mouse buttons or a Stream Deck.

**Custom shortcut.** Pick one in Settings. Shortcuts that include Alt behave like Alt+Tab (switch on release). Others stay open until you press Enter or Esc or click.

## Updates

With **Check for updates automatically** on, Switch checks GitHub Releases about 20 seconds after it starts and then every 6 hours. When a new version is out, a notification appears. Click it, or use the tray menu or Settings, to install. The download only goes ahead if its SHA-256 matches `Switch.exe.sha256` from the same release. Switch then swaps the exe and restarts itself.

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
| Idle cost | Zero timers while hidden apart from the 6-hour update check. Animation timers only run during the ~150 ms fade and selection glide |
| Updater | WinHTTP (honours the system proxy, adds no TLS library), SHA-256 verified, atomic rename swap |

```
src/
  main.rs         entry point, single instance, tray icon and menu, update notifications
  hook.rs         low-level keyboard hook and session state shared with the UI
  switcher.rs     overlay window: layout, search, grouping, DWM thumbnails, input, rendering
  winlist.rs      Alt-Tab-eligible window enumeration, app names and icons, window actions
  settings_ui.rs  custom-drawn settings window
  gfx.rs          Direct2D/DirectWrite/WIC helpers, theme palette, vector glyphs
  config.rs       settings.json and Start with Windows (HKCU Run key)
  updater.rs      GitHub Releases check, verified download, self-replace and relaunch
  util.rs         wide strings, registry, logging
assets/           icon and app manifest (Per-Monitor-V2 DPI)
.github/workflows CI build and tag-triggered releases
```

## Known limitations

- **Elevated windows.** When an app running as administrator is focused, Windows doesn't deliver its keystrokes to a non-elevated hook, so the built-in Alt+Tab appears instead. Running Switch as administrator works around this.
- **SmartScreen.** The exe isn't code-signed yet, so the first run shows "Windows protected your PC". Choose *More info → Run anyway*.
- Minimized windows whose app doesn't keep a preview show their icon in place of a live thumbnail.

Problems are logged to `%APPDATA%\Switch\switch.log`.
