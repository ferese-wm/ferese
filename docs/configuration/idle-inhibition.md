# Idle inhibition

Ferese keeps the session awake during fullscreen playback when an application
reports `Playing` through MPRIS. Pausing or stopping playback releases this
automatic inhibition. A window must have content visible on an awake display;
switching away, closing it, locking the session or sleeping its display releases
inhibition too.

Disable the default fullscreen playback policy with:

```kdl
idle-inhibit {
    fullscreen-playback #false
}
```

Use window rules for presentations, games or other apps that should keep the
session awake while visible:

```kdl
window-rule app-id="org.example.Presentation" idle-inhibit="visible"
```

| `idle-inhibit` rule | Behavior |
| --- | --- |
| `visible` | Inhibit while visible, regardless of playback state |
| `fullscreen` | Inhibit while visible and fullscreen |
| `playing` | Inhibit while visible and its MPRIS player reports `Playing` |
| `fullscreen-playing` | Inhibit while visible, fullscreen and its MPRIS player reports `Playing` |
| `none` | Disable automatic inhibition for this window |

MPRIS identifies an application, not an individual video window. Ferese matches
the player's process ID first, then its `DesktopEntry` against the window's app
ID. If several windows match, automatic playback inhibition stays off. MPRIS
also does not distinguish audio from video, so fullscreen audio playback can
inhibit too. Players without MPRIS can request inhibition through the existing
Wayland or portal protocols; a fullscreen window alone does not prove playback.

These settings control automatic inhibition. They do not override an
application's own Wayland or portal request, and they do not block manual
locking or suspend. Run `feresectl idle-inhibition` to inspect the current
inhibition state and discovered players. Changes take effect on config reload.
