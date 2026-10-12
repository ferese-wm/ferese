# Configuration

Edit `~/.config/ferese/config.kdl` (or `$XDG_CONFIG_HOME/ferese/config.kdl`),
or open **Control Center → Settings**. Start with the [example](../packaging/config.kdl),
which includes app and media shortcuts, appearance and optional startup
and display settings. Its browser/editor shortcuts need Firefox/Zed; brightness
keys need `brightnessctl` and audio keys need `wpctl`.
This reference covers layouts, appearance, input, and desktop behavior.
See [Desktop portals](portals.md) for application integration options.
The [Live Activities guide](activities.md#configure-activities) covers task
visibility, shell detection, command rules and notification bridging.

## KDL syntax

Sections use nested braces, values follow their names, and booleans are `#true`
or `#false`. Multiple values represent arrays. Hyphenated keys are preferred;
underscore spellings remain accepted. Enum values retain their documented spelling.
A newline or semicolon ends each node. Comments use `//` or `/* ... */`.
For example:

```kdl
input {
    repeat-rate 30
    touchpad {
        swipe-threshold 80
    }
}

commands {
    terminal "foot"
}

binding "Super+Enter" "spawn" "terminal"
binding "Swipe3Up" "toggle-overview"
binding "Swipe3Down" disabled=#true
output-profile "docked" {
    output "HDMI-A-1" {
        scale 1.5
        position 0 0
    }
}
```

Lists use repeated `panel`, `group`, `item`, `binding`, `window-rule`,
`output-profile`, `output`, and `autostart` nodes. Bindings take their keys,
action, and argument positionally; profiles and outputs take a positional
name or match. Panels, groups, and items use positional IDs, while autostart
commands use positional arguments. Other fields can be properties or child nodes,
but duplicate fields are rejected. The dotted paths in the reference tables
describe nested sections.

Settings preserves comments and untouched configuration content when editing,
while normalizing indentation. Panel item annotations belong in an explicit
`metadata` section; unrecognized built-in options are rejected.

The Settings sidebar groups pages under Desktop, Interaction, and System, with
Motion under Windows and Shortcuts under Keyboard & mouse. Search includes both
views and opens the matching setting directly.

## Saving and validation

Settings saves text when you press Enter or leave the field, and sliders when
you release them, keeping `config.kdl.settings-backup` before each save. Use
Undo to restore the previous save, or Reload to read external edits without
merging unfinished drafts. To browse for a wallpaper, install `zenity`; you can
also enter its path directly.

File edits reload automatically, including atomic editor saves, so changes to
appearance, wallpaper, motion, input, bindings, rules, layouts, displays,
and login items take effect live. If a reload is invalid, Ferese reports the error
and keeps the last working configuration.

At login, Ferese loads your configuration or uses the built-in defaults if the
file is missing or invalid. For an invalid file, it shows a notification once
the desktop is ready and leaves the file untouched, so you can correct it to
match the current schema and reload your settings.

```sh
feresectl reload-config          # reload with error feedback
ferese --check-config PATH       # validate without applying
ferese-settings --config PATH   # edit a separate preview config
ferese-settings --page wifi     # open Wi-Fi controls
ferese-settings --page bluetooth # open Bluetooth controls
```

**Settings → Connections** manages Wi-Fi through NetworkManager and Bluetooth
through BlueZ. Wi-Fi supports scanning, joining open/WPA/WPA2/WPA3 networks,
hidden networks, changing saved passwords, disconnecting, and forgetting saved
networks. Enterprise and legacy Wi-Fi connections use an existing NetworkManager
profile; their certificate and authentication setup is not edited here.

Bluetooth supports adapter selection, power, discovery, pairing with PIN/passkey
or confirmation prompts, connecting, disconnecting, trust, and forgetting devices.
Discovery runs only when requested and stops after 30 seconds, when leaving
Connections, or when Settings closes. Leaving the page or closing Settings also
cancels pending pairing. Passwords are sent to NetworkManager, not saved in KDL;
pairing keys are managed by BlueZ. These controls affect the system even when
Settings is opened with a separate preview config.

The reference tables give built-in defaults; your configuration or the packaged
example may override them. Dimensions use logical pixels unless noted, and
numbers must be finite. Omit optional values rather than writing `null`, use
`#RRGGBB` or `#RRGGBBAA` for colors, and keep the live config within 60 KiB.

## Layout

[Layout](configuration/layout.md).

## Animations

[Animations](configuration/animations.md).

### Speed and reduced motion

[Speed and reduced motion](configuration/animations.md#speed-and-reduced-motion).

### Spring tuning

[Spring tuning](configuration/animations.md#spring-tuning).

## Appearance

[Appearance](configuration/appearance.md).

## Workspaces

[Workspaces](configuration/workspaces.md).

## Input

[Input](configuration/input.md).

## Commands and bindings

[Commands and bindings](configuration/bindings.md).

### Binding modes

[Binding modes](configuration/bindings.md#binding-modes).

## Window rules

[Window rules](configuration/window-rules.md).

## Displays

[Displays](configuration/outputs.md).

## Panel composition

[Panel composition](configuration/panel.md).

## Status controls

[Status controls](configuration/status.md).

### Now Playing

[Now Playing](configuration/status.md#now-playing).

## Login items and locking

[Login items and locking](configuration/startup-and-locking.md).

## Idle inhibition

[Idle inhibition](configuration/idle-inhibition.md).

## Optional session protocols

[Optional session protocols](configuration/session-protocols.md).

## X11 support (xwayland-satellite)

[X11 support (xwayland-satellite)](configuration/xwayland.md).

### Requirements and failure behavior

[Requirements and failure behavior](configuration/xwayland.md#requirements-and-failure-behavior).

### Diagnostics and recovery

[Diagnostics and recovery](configuration/xwayland.md#diagnostics-and-recovery).

### Limitations

[Limitations](configuration/xwayland.md#limitations).

## Power and logout

[Power and logout](configuration/startup-and-locking.md#power-and-logout).
