# Changelog

## 0.2.0

**New**
- List layout with a large live preview of the selected window, alongside Grid and Row. Ctrl+L cycles layouts
- Fuzzy search with highlighted matches ("vsc" finds Visual Studio Code); results are ranked by match quality
- Alt+` switches between windows of the current app
- Grouping now has three modes: Off, Together and Collapse (one tile per app with a count badge). Ctrl+G cycles them
- Right-click menu (and the Menu key) with every window action
- New window actions: maximize/restore, move to the next or previous monitor, close every window of an app, open a new window, open the file location
- Optional windows from other virtual desktops, marked with a badge
- Optional desktop dimming behind the switcher
- Sort by recent, app or title; start on the previous or the current window; wrap-around toggle
- Open on the cursor's monitor, the active window's monitor or the primary monitor
- Select-on-hover and action-button toggles
- Keyboard hint bar, PgUp/PgDn, and Ctrl+Backspace to clear the search

**Settings**
- Redesigned settings window with sidebar pages: General, Shortcuts, Appearance, Layout, Windows, Hidden apps, Updates, About
- Live preview of the switcher on the Appearance and Layout pages
- Pure Black theme, custom hex accent color, corner radius, text size, selection style, animation speed (Off/Fast/Normal/Relaxed), position (center/top/bottom), labels (title/app/both), icon and search-bar visibility
- Sliders for preview size, quick-switch delay, corner radius and dim strength
- Hidden apps page: hide running apps with one click, or add any app by file name
- "Release Alt to switch" can be turned off so every session stays open
- Updates page shows release notes and when the last check ran; optional automatic install; notifications can be turned off
- About page with live memory usage, log and settings shortcuts, and reset to defaults
- Restart as administrator, a master on/off switch, and a choice of what clicking the tray icon does

**Faster and lighter**
- Animations are paced by the compositor (DwmFlush) for smooth motion at any refresh rate, and add a subtle zoom on open
- Idle memory goes back to Windows 10 seconds after the switcher closes
- The panel is sized once per session, so typing never resizes it

**Other**
- `--settings` and `--quit` command-line options
- Settings from 0.1 migrate automatically

## 0.1.0

First release: Alt+Tab takeover, live previews, type-to-search, grouping, window actions, multi-monitor, themes, tray, settings and the built-in updater.
