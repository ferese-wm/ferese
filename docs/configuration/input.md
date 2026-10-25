# Input

| `input` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `focus-follows-mouse` | boolean | `false` | Focus visible windows under the pointer without raising them or scrolling; click or use keyboard focus to reveal a window |
| `xkb-layout` | nonempty string | `"us"` | XKB layout |
| `xkb-variant` | string | `""` | XKB variant |
| `xkb-options` | string array | `[]` | XKB options |
| `repeat-rate` | integer > 0 | `25` | Repeats per second |
| `repeat-delay-ms` | integer ≥ 0 | `600` | Delay before repeat |

With focus-follows-mouse enabled, hovering a visible part of a window changes
keyboard focus without raising it or moving the scrolling viewport. Clicking or
using directional keyboard focus brings the selected window into view.

In Overview (`Super+Tab`), click a window preview or a miniature inside a workspace
card to activate that exact window. Hover highlights the target; directional
focus keys move the selection, Enter activates it, and Escape dismisses Overview.
Clicking a workspace card's background switches workspaces while keeping Overview
open. Previews follow layout order and use the configured shell font and colors.

The touchpad keys `tap`, `natural-scroll` and `disable-while-typing` default to
`true`. Set `disable-while-mouse-connected #true` to pause a built-in touchpad
while an external mouse is connected. It defaults to `false` and uses libinput's
external-mouse policy on supported touchpads, including when a mouse is plugged
in or removed during the session. The toggle is also in **Settings → Keyboard & mouse**.
These device settings apply to DRM sessions; nested previews use the host's
physical input settings.

Touchpad `accel-speed` is independent of the mouse setting. It accepts −1.0 to
1.0 and defaults to 0.0. Both speed controls are available in Settings and reload
immediately in a hardware session.

```kdl
input {
    touchpad {
        accel-speed 0.0
        disable-while-mouse-connected #true
    }
    mouse {
        natural-scroll #false
        accel-speed 0.0
        // accel-profile "flat"
        left-handed #false
        middle-emulation #false
        scroll-factor 1.0
        drag-threshold 4.0
        scroll-cooldown-ms 150
    }
}
```

Mouse settings are separate from touchpad settings and reload live. `accel-speed`
accepts -1.0 to 1.0; `accel-profile` accepts `adaptive` or `flat`, and follows the
device default when omitted. `scroll-factor` scales application wheel scrolling
(0.01–100). `drag-threshold` is the distance before a window drag starts (0–100
logical pixels). `scroll-cooldown-ms` limits repeated wheel bindings (0–5000 ms);
zero allows each accumulated tick to run its action.

Workspace swipes and left/right window-focus swipes follow your fingers and settle
when released. Up goes to the next workspace on the
current monitor, down to the previous; left focuses the window to the right,
and right focuses the window to the left. Workspace swipes skip workspaces owned
by other monitors and stop at the first/last workspace. Short or cancelled navigation
swipes slide back; diagonal swipes do not navigate. Navigation pauses while locked, during
window grabs, or when an application inhibits shortcuts.

Assign swipes in **Settings → Keyboard & mouse → Shortcuts** using the same actions and arguments as
keyboard bindings. Choose a "Customize swipe" button to add an override. The
gesture keys are `Swipe3Up`, `Swipe3Down`, `Swipe3Left`, and `Swipe3Right`; 4 or 5
fingers are also supported. Changes reload live:

```kdl
binding keys="Swipe3Up" action="toggle-overview"
binding keys="Swipe3Left" action="move" argument="left"
binding keys="Swipe3Down" disabled=#true
```

`spawn` uses a named entry from `commands`, just like keyboard shortcuts. To
disable a direction in Settings, set its action to `none` and leave Argument
empty. In KDL, `disabled=#true` removes the default binding. Swipes do not use
keyboard modifiers or physical key matching. The trigger is automatically
recognized from its name. Gesture navigation uses native touchpad events in a
hardware session.

Adjust recognition distance in **Settings → Keyboard & mouse**, or with
`swipe-threshold 80` under `input.touchpad` (integer 16–1000 logical pixels).

## Mouse bindings

`Super+MouseLeft` drags a window and `Super+MouseRight` resizes it. Both work with
floating windows and tiled windows in scrolling or tree layouts. Drag a tiled
window to another window's edge to place it beside that window, or to its center
to join a tab group. Top and bottom drops into a tabbed column show its windows as
rows. Drag across monitors to move to the workspace shown there.
While dragging, right-click to switch between floating and tiling. Scroll or hold
the pointer at a horizontal screen edge to reach other scrolling columns.
Escape cancels the operation; releasing the initiating button finishes it.
Fullscreen windows keep their current layout until you leave fullscreen.

Mouse bindings use the same actions, overrides and named binding modes as keyboard
bindings. Ordinary actions use the current keyboard focus; `pointer-move` and
`pointer-resize` operate on the window under the pointer. Client popup grabs,
pointer constraints, shortcut inhibition and session locking take precedence.

```kdl
binding "Alt+MouseLeft" action="pointer-move"
binding "Alt+MouseRight" action="pointer-resize"
binding "Super+MouseLeft" disabled=#true
binding "Super+WheelUp" action="workspace-previous"
binding "Super+WheelDown" action="workspace-next"
binding "Super+TouchpadScrollLeft" action="focus" argument="left"
binding "Super+MouseBack" action="focus-last-window"
```

Buttons include `MouseLeft`, `MouseRight`, `MouseMiddle`, `MouseBack` and
`MouseForward`, or `mouse:272` through `mouse:287` for Linux mouse button codes.
Wheel directions are `WheelUp`, `WheelDown`, `WheelLeft` and `WheelRight`.
Touchpad scroll bindings use the `TouchpadScroll` prefix with the same directions.
Only a bound scroll axis is intercepted; the other axis still reaches the client.
High-resolution wheel steps accumulate into ticks. An intercepted touchpad scroll
keeps its event ownership until the gesture stops, even if modifiers change.
