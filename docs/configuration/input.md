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

The touchpad device keys are booleans, all defaulting to `true`: `tap`,
`natural-scroll` and `disable-while-typing`. These apply to DRM devices; nested
previews use the host's physical input settings.

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
