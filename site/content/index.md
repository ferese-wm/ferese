# Ferese handbook

Ferese combines a window manager and desktop shell with one configuration.
This handbook covers installation, settings and desktop controls.

[Browse screenshots](screenshots.md) to see the themes, layouts and desktop controls in
use.

## Get started

1. [Install Ferese](installation.md), or try a [nested preview](installation.md#preview-and-logs).
2. Choose Light, Dark, or Auto in **Control Center**. Open **Settings** for theme files and presets.
3. Use the [configuration reference](configuration.md) for shortcuts, gestures, window rules, and displays.

## Default shortcuts

Use `Super`, usually the Windows key, for the shortcuts below, and make them your own in
**Settings → Shortcuts**.

| Control | Action |
| --- | --- |
| Super + Enter | Open a terminal |
| Super + H / J / K / L | Focus left / down / up / right |
| Super + F | Maximize the window |
| Super + Shift + F | Toggle fullscreen |
| Super + Tab | Open overview |
| Super + M | Switch scrolling / tree layout |
| Print Screen | Capture the active monitor and open Satty |
| Super + Shift + S | Select a screenshot area |
| Super + Shift + E | Ask to log out |
| Super + left-button drag | Move a floating window |
| Super + right-button drag | Resize a floating window |
| Three-finger swipe up / down | Next / previous workspace |
| Three-finger swipe left / right | Focus the window to the right / left |
| Login shortcut guide | Appears at login; disable in Settings → Shortcuts |

The [shortcuts and mouse actions guide](shortcuts.md) covers the rest, including how to
group windows, resize them and move between workspaces.

## Desktop guides

- [Lock screen](locking.md): appearance, automatic locking, and display sleep.
- [Screen sharing and recording](screen-sharing.md): share an app or display, or save a video.
- [Desktop portals](portals.md): integration with applications.
- [Animation model](animation-model.md): spring motion, geometry ownership, and resize presentation.

Configure the desktop in Settings or edit `~/.config/ferese/config.kdl` directly.
Changes apply live. Invalid edits leave the last working configuration in place.

## Troubleshooting

If you need to end a session, `feresectl request-logout` asks for confirmation and
`feresectl exit` exits immediately, so save your work first. The [logs and recovery
guide](installation.md#logout-and-recovery) helps when something stops responding.

To [report a problem](https://github.com/ferese-wm/ferese/issues), include the steps to
reproduce it, relevant logs and whether you were using a nested preview or a direct
session. See [Development](development.md) to contribute.
