# Changelog

## 0.3.0

**New**
- Launch apps from search: typing also finds installed apps (desktop and Store apps), shown after matching windows. Press Enter to open one
- Number keys: press 1–9 to jump straight to a window, with number badges on the first nine
- Snap from the switcher: Ctrl+Shift+Left / Right puts the window on the left or right half of its screen
- Side by side: Ctrl+S puts the selected window next to the one you came from
- Favorite apps: Ctrl+F (or the right-click menu, or Settings → Apps). Their windows lead the list and get a ★
- Hot corner: rest the cursor in a screen corner to open Switch
- Mouse button: the Back or Forward side button opens Switch; press it again to switch
- Start with Windows "As admin": runs elevated at sign-in through Task Scheduler, so Alt+Tab works over admin apps with no prompt each time
- Beta versions: an opt-in channel for pre-releases
- Export and import settings (Settings → About)

**Refinements**
- The window Alt+Tab flips to is chosen by recent use, so favorites, sorting and grouping never change it
- Ctrl+G and Ctrl+L now remember your choice
- Narrow tiles show only the close button, so titles stay readable
- The Apps page (formerly Hidden apps) manages favorites and hidden apps together, with one-click buttons for running apps
- Updater handles pre-release versions correctly (0.4.0 is newer than 0.4.0-beta.2)
- The Shortcuts page lists every new key

**Faster and lighter**
- Process paths are cached per window, so Alt+Tab no longer opens every process on each press
- The mouse hook only exists while a mouse trigger is turned on
- Settings → About shows how long the last open took, measured from start to the first frame

**Other**
- Crashes are written to the log with their location
- Releases are published automatically when the version changes

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
