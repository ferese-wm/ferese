# Shortcuts and gestures

`Super` is usually the Windows key. These are the built-in defaults. Open
**Settings → Keyboard & mouse → Shortcuts** to change bindings and gestures or disable the login
guide. Your saved bindings can override these defaults.
The [example config](../packaging/config.kdl) adds the app, media and arrow-key
shortcuts listed below.

## Move and resize with the mouse

| Action | Result |
| --- | --- |
| Super + left-button drag on a floating window | Move the window |
| Super + right-button drag on a floating window | Resize the edge or corner selected by the starting 3×3 region |
| Hold Shift during a move or resize | Bypass edge snapping |
| Super + Shift + Space | Switch the focused window between tiled and floating |

You can drag from inside the window; a title bar or resize border is not required.
To place an app freely, toggle it to floating, then Super-drag it.
The central resize region selects the nearest corner. Moving and resizing snap
to nearby edges; continuing to drag beyond 20 logical pixels releases the snap.

## Focus, move, and resize

`H`, `J`, `K`, and `L` mean left, down, up, and right.

| Shortcut | Action |
| --- | --- |
| Super + H / J / K / L | Focus in that direction |
| Super + Backspace | Return to the last focused window, including on another workspace or monitor |
| Alt + Tab | Preview recent windows in focus order |
| Alt + Shift + Tab | Preview recent windows in reverse focus order |
| Super + Shift + H / J / K / L | Move the focused window in that direction |
| Super + Ctrl + H / J / K / L | Resize in that direction |
| Super + Q | Close the focused window |
| Super + F | Maximize or restore the window |
| Super + Shift + F | Enter or leave fullscreen |

Maximize fills the available workspace. Fullscreen also hides the bar and window
decorations.

Keep Alt held to cycle through recent windows. Release Alt to select the previewed
window, or press Escape to cancel. Repeating Super + Backspace toggles between the
two most recently focused windows.

## Columns and layouts

| Shortcut | Action |
| --- | --- |
| Super + R | Cycle column width |
| Super + C | Center the focused column |
| Super + [ | Place the window in a neighbouring column |
| Super + ] | Extract the window into its own column |
| Super + M | Switch scrolling / tree layout |
| Super + Tab | Open or close overview |

In scrolling mode, **Super + [** uses the left neighbour when available, otherwise
the right. Try it with two terminals to place them vertically in one column.
**Super + ]** separates the focused window again.

In overview, select a workspace to see its windows, then click the window you
want. Directional focus keys move selection; Enter activates it and Escape closes
overview.

## Workspaces

| Shortcut | Action |
| --- | --- |
| Super + 1–9 | Switch to that workspace |
| Super + Shift + 1–9 | Move the window to that workspace |
| Super + Escape | Toggle between the last two visited workspaces on this monitor |

Workspaces are created as needed and numbered separately on each monitor.
Workspace shortcuts select a workspace on the focused monitor.

To make pressing the current workspace's shortcut toggle back too, enable
**Settings → Windows → Toggle back with the same workspace shortcut**. It is off
by default. The action can also be bound as `workspace-back-and-forth` or run with
`feresectl workspace-back-and-forth`.

## Desktop actions

| Shortcut | Action |
| --- | --- |
| Super + Enter | Open the configured terminal (`foot` by default) |
| Print Screen | Capture the active monitor and open Satty |
| Super + Shift + S | Select a screenshot area and open Satty |
| Super + Shift + E | Ask to log out |
| Super + F1 | Open or close the shortcut hint |
| Display Fn key (`XF86Display`) | Open or close the display-mode chooser |

Super + F1 works even when the guide at login is disabled. It opens on the
focused monitor. Bind `toggle-keybinding-guide` to change the shortcut, or run
`feresectl toggle-keybinding-guide`.

The display icon appears in the bar when an external monitor is connected,
including when that monitor is disabled or mirrored. Click it to choose Internal
only, External only, Extend or Mirror. Arrow keys or 1–4 select a mode; Enter
applies it. Tab moves between controls and Escape cancels. After applying, choose
Keep before the confirmation timeout or Revert to restore the last confirmed
configuration. Bind `toggle-display-mode` if your keyboard uses another key.

In Satty, Enter saves the edited screenshot and copies it to the clipboard;
Escape discards it. See [Configuration](configuration.md#commands-and-bindings)
for dependencies and custom screenshot commands.

## Touchpad gestures

| Three-finger swipe | Action on release |
| --- | --- |
| Up / down | Drag to the next / previous workspace on this monitor; release to settle |
| Left / right | Drag the window row; release to focus the window to the right / left |

Short or cancelled navigation swipes slide back. Diagonal swipes do not navigate.
Gestures can use the same actions
as keyboard bindings, including overview or moving a window. Four- and five-finger
bindings are supported too.

## Reserved and context controls

These controls are handled by the compositor rather than configurable bindings.

| Control | Action |
| --- | --- |
| Enter in overview | Activate the selected window |
| Escape in overview | Close overview |
| Escape during Alt + Tab | Cancel the preview without changing focus |
| Release Alt during Alt + Tab | Focus the previewed window |
| Ctrl + Alt + Escape | Release an active shortcut inhibitor or input-capture session |
| Ctrl + Alt + F1–F12 | Switch virtual terminals in a direct session |

Virtual-terminal switching is unavailable in nested previews.

## Example-config shortcuts

The [example config](../packaging/config.kdl) adds these to the built-in defaults.
App and hardware controls require their configured programs to be installed.

| Shortcut | Action |
| --- | --- |
| Super + B | Open Firefox |
| Super + Z | Open Zed |
| Super + Alt + L | Lock with `ferese-lock` |
| Super + Left / Down / Up / Right | Focus in that direction |
| Super + Ctrl + Left / Down / Up / Right | Move the focused window in that direction |
| Super + Alt + Left / Down / Up / Right | Resize in that direction |
| Brightness up / down | Adjust brightness with `brightnessctl` |
| Volume up / down | Adjust volume with `wpctl` |
| Audio mute | Toggle output mute with `wpctl` |
| Microphone mute | Toggle microphone mute with `wpctl` |

## Add a shortcut

For example, bind a lock command and use a swipe to open overview:

```kdl
commands {
    lock "ferese-lock"
}
binding "Super+Alt+L" "spawn" "lock"
binding "Swipe3Up" "toggle-overview"
```

These are examples, not defaults. To disable a built-in binding:

```kdl
binding "Super+Q" disabled=#true
```

See [Commands and bindings](configuration.md#commands-and-bindings) for action
names, arguments, and physical-key matching.
