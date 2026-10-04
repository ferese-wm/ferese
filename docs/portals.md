# Desktop portals

Applications use desktop portals to request screen sharing, screenshots, wallpaper
changes and other desktop services. Ferese supplies themed consent dialogs, while
GTK handles file, print and other standard dialogs.

Install `xdg-desktop-portal`, `xdg-desktop-portal-gtk`, and Ferese's backend through
[Installation](installation.md). The session launcher starts the services with
the correct display environment.

## Supported services

| Service | Behavior |
| --- | --- |
| Settings | Color scheme, accent, contrast, and reduced-motion preferences |
| Screenshot | Screen, area, window, active-window capture, and color picking |
| Wallpaper | Preview and approval for desktop, lock screen, or both |
| ScreenCast | Display and window sharing through PipeWire |
| Background | Permission to run in the background and startup integration |
| USB | Device information and approval for requested access |
| GlobalShortcuts | App shortcut selection, conflict checks, and activation |
| Inhibit | App requests to delay logout, suspend, or idle actions |
| InputCapture | Approved keyboard/pointer capture for input-sharing applications |
| Lockdown | Administrator restrictions on printing, saving, handlers, location, camera, microphone, and sound |
| FileChooser, Print, Access, Account, AppChooser, DynamicLauncher, Notification | GTK backend |

RemoteDesktop and its session Clipboard integration are not implemented.
This does not affect ordinary application clipboard use.

## Consent and capture

Screenshot and screen-sharing requests need approval. Display-sharing permissions
can be remembered when the application requests it and the user opts in. Window
sharing always asks again. See [Screen sharing](screen-sharing.md).
Window captures contain the selected app and its subsurfaces; separate popups,
other apps, shell layers, and compositor decorations are excluded.

InputCapture starts at an approved barrier at the edge of a display. Press
**Ctrl+Alt+Escape** to regain local control. Capture stops when you lock the session,
disconnect the receiver, change displays or change the keyboard layout. Touch and
remembered InputCapture grants are not supported. InputCapture forwards local
input; it does not inject remote input.

Wallpaper approval copies the image into Ferese's data directory and updates the
active light or dark appearance in the config. The other appearance keeps its
wallpaper. A desktop-only change keeps the current lock-screen image, and a
lock-screen-only change keeps the desktop image. Set `theme.background.lock-path`
for a shared lock-screen image, or override it under `theme.light.background` and
`theme.dark.background`. Without a lock-screen path, it follows the desktop wallpaper.

Global shortcuts cannot replace Ferese bindings or reserved escape/console keys.
They stop when the app session closes. They never activate on the lock screen.
Power confirmations show app inhibitors before proceeding. Ferese has no
user-switch operation.

## Portal selection

The installer writes `/usr/share/xdg-desktop-portal/ferese-portals.conf`.
Overrides in `~/.config/xdg-desktop-portal/` may take precedence. If a request opens
the wrong dialog or no source appears, check those overrides and confirm that
`XDG_CURRENT_DESKTOP` includes `Ferese`.

For capture issues, see [screen-sharing troubleshooting](screen-sharing.md#troubleshooting).
For backend contracts and integration checks, see [Development](development.md#portal-checks).
