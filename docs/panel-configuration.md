# Panel configuration

Use **Settings → Panel & Shell** to change the panel's style, position and
contents, or edit the `panel` block in `~/.config/ferese/config.kdl`. Settings
writes to this file, so you can arrange controls visually and then adjust their
values in KDL. Changes apply live, and an invalid edit leaves the last working
configuration active.

Ferese uses one panel configuration across all outputs. Each output lays out
its own panel using the space and services available there; workspace controls
and the focused-window title belong to that output. If you omit the `panel`
block, Ferese supplies its default composition. Once you write one, its groups
and items define the composition, including any zones you leave empty.

## Arrange the panel in Settings

The composition preview stays above the editor while you change the panel.
It uses sample content, so it can show a battery, media player or external
display control even when that service is unavailable in your session. Its
layout adapts to the width of the Settings window using the same allocation
rules as the shell.

Choose a **Style** from the compact dropdown, then use **Position** to place the
panel at the top or bottom. The appearance controls set its background opacity,
outline and corners, while **Size & spacing** controls its height and distance
from windows and screen edges. A reset button beside a dimension clears that
override; **Reset to style** clears geometry and corner overrides together.

Choose **Edit panel** to open Arrange, where **Add group** creates a group in
Start, Center or End. Use its **Add item** menu to fill it, then select an item
to change its placement, visibility, overflow, preferred size or options
specific to that control. Select the group itself to change its placement,
item spacing and padding; before removing it, move or remove its items.

Use the drag handles to move items within or between groups, or the arrow
buttons to reorder them. A drop hint shows where the item will go, and dropping
it into an empty zone creates a group there. The move saves on release;
Escape, focus loss or a release outside a valid target cancels it.

Other controls save automatically, with sliders saving when you release them.
**Undo** restores the previous save, **Reload configuration** reads external
edits, and **Back to Panels** returns to the appearance controls.

Add **Activities** to show running commands and published task progress. Its
visibility and overflow follow the same item controls. The
[Live Activities guide](activities.md) covers shell setup and command detection.

**Remove custom panel** restores the built-in composition, style and geometry;
Undo brings back the composition you removed.

You can open this page directly with `ferese-settings --page bar`.

## Choose a style

Choose the style for the whole panel; its groups share that appearance and
control only arrangement and spacing.

| Settings name | KDL value | Appearance and behavior |
| --- | --- | --- |
| Floating | `floating` | Separate rounded islands for occupied groups, with transparent gaps between them |
| Capsules | `capsules` | One rounded capsule per occupied zone, with dividers between its groups |
| Notch | `notch` | Groups attached to the screen edge, with curved shoulders around the centered group |
| Edge to edge | `flat` | A continuous full-width background with square outer corners |
| Fused islands | `fused` | Rounded groups with connecting necks where neighboring groups are close enough |
| Adaptive | `adaptive` | Floating with ordinary windows, edge to edge when maximized, and hidden in fullscreen |
| Morph | `morph` | One centered hub that expands into workspace, media and status controls |

Capsules and Fused islands join groups **within the same zone**, so a zone with
one group appears as a single pill in either style. To see the joins, put
Overview and Workspaces in separate Start groups, or separate the status controls
and Clock into two End groups. With Capsules, those groups touch and have a
divider between them; Fused islands keeps their configured spacing and blends
nearby shapes, while Floating leaves the gaps open.

When a group has no visible items, its background disappears, and the remaining
shapes adjust as items become unavailable or move into overflow. Clicks in
transparent gaps reach the windows underneath. If the active workspace contains
a fullscreen window, the panel hides on that output and releases its reserved
space.

Changing the style preserves your composition and saved appearance overrides.
Adaptive evaluates each output independently, with a 150 ms delay between
ordinary and maximized presentations to avoid switching back and forth during
brief state changes. Entering or leaving fullscreen takes effect immediately.

## Position, size and spacing

Set `edge "top"` or `edge "bottom"` on the panel. Its controls keep the same
order, while edge-relative corners and attached shapes mirror vertically.
Popups and notifications open toward the desktop.

Dimensions use logical pixels, so output scaling applies afterward. The
`geometry` block contains optional overrides; omitting a value lets the selected
style supply it.

| `geometry` key | Default | Accepted values | Meaning |
| --- | --- | --- | --- |
| `height` | 36; Morph uses 34 | Number 24–128 | Panel height, or Morph's collapsed height |
| `edge-margin` | 8 for detached styles; 0 for Notch and Edge to edge | Integer 0–4096 | Distance from the selected screen edge |
| `side-margins` | 8 for detached styles; 0 for Notch and Edge to edge | Integer 0–4096 | Requested inset at both ends of the panel |
| `window-clearance` | 0 | Integer 0–4096 | Space between the panel and tiled or maximized windows |
| `inner-padding` | 12 | Number 0–64 | Space inside the row before its first and after its last group |

Notch and Edge to edge use zero edge and side margins even if you have saved
overrides for them. Those values remain in the file and apply again when you
choose a detached style. Attached Morph also uses zero edge margin, while its
saved floating margin returns when attachment is disabled.

Ferese uses the requested side margins only while the content fits. It measures
the controls on each output and reduces the inset when the output narrows or
more content appears, so the margins do not cause additional overflow. Settings
limits the slider to the available display and preview measurements; KDL accepts
the wider range shown above.

All panel styles reserve their height plus window clearance. At the panel edge,
clearance replaces the layout's outer gap; the other edges keep their usual gaps.
Morph reserves its collapsed height, and expanded views overlay windows without
pushing them down.

## Background, outline and corners

`background-opacity` accepts a number from 0 to 1 and affects the panel
background without fading its text or icons. Omit it to inherit shell opacity,
or use the reset control beside **Background opacity** in Settings. It does not
change the opacity of popups, notifications or application windows.

The panel uses the shared material settings under `theme.material`, including
translucency and backdrop blur. Morph uses the theme's `surface-raised` color
and suppresses the material shadow. **Reduce Transparency** takes precedence
over a panel opacity override, making the background opaque and removing blur.
See [Appearance](configuration/appearance.md#appearance) for the shared theme controls.

Set `border #true` to draw the panel outline. Its color comes from
`theme.colors.border`, and Capsules also uses that color for its dividers.
Leave it at the default `#false` for no outline.

Automatic corners follow the style and panel height. Choose **Custom** to pin
them, or return to **Automatic** to remove the override. The four controls are
**Edge left**, **Edge right**, **Outer left** and **Outer right**, so they retain
their meaning when you move the panel to the bottom.

In KDL, `corner-radius` is a pixel string using CSS-style shorthand. One value
sets every corner; two set opposite pairs; three set the first corner, the
second/fourth pair and the third; four give each corner its own value. The
stored order is edge-left, edge-right, outer-right, outer-left:

```kdl
corner-radius "4px 8px 12px 16px"
```

On a top panel, that order runs clockwise from top-left. On a bottom panel,
the same values mirror vertically. Values must be finite and nonnegative, with
`px` units except for a bare `0`; percentages and elliptical `/` syntax are
not supported. Ferese scales radii to fit when adjacent corners would overlap,
and Edge to edge keeps its outer corners square. Corner pins stay saved when
you switch styles.

## Groups and items

Place groups in Start on the left, Center in the middle or End on the right,
then arrange items within them. Center stays centered on the panel even when
the two sides have different widths. Each group and item has an ID, so Settings
keeps track of the one you are editing when you move it.

Panel IDs, group IDs and item IDs contain 1–128 ASCII letters or digits, or
`-_.:@`, and cannot start with `_`. Group IDs must be unique across the panel,
as must item IDs; different instances of the same item kind are allowed when
their IDs differ. A panel supports up to 31 authored groups.

| Scope / key | Default | Accepted values | Meaning |
| --- | --- | --- | --- |
| Zone `spacing` | 8 | Number 0–64 | Gap between groups; Capsules and Notch use zero |
| Group `spacing` | 1 | Number 0–64 | Gap between items |
| Group `padding` | `2 3` | Two integers 0–32 | Vertical padding followed by horizontal padding |
| Group `island-padding` | 4 | Number 0–32 | Extra horizontal padding for island styles |
| Item `visible` | `#true` | Boolean | Show the item when its service or hardware is available |
| Item `overflow` | `auto` | `auto`, `never`, `always` | Overflow when needed, keep visible, or always use overflow |
| Item `priority` | 50 | Integer 0–100 | Lower values yield space first |
| Item `representation` | Preferred supported form | `wide`, `compact`, `icon`, or omit | Starting form before adapting to available space |
| Item `gap-before` | Unset | Number 0–64 | Override the gap before this item within its group |
| Item `metadata` | Empty | Nested section | Custom data preserved by Settings, without shell behavior |

Horizontal padding and island padding add together for island styles. Group
padding changes the space around that group's controls; it does not give the
group a separate height, style or set of corners.
`gap-before` has no effect on the first visible item in a group.

### Item catalog

The Add item catalog contains these twelve kinds. Service-dependent controls
appear when their service or hardware is available, and a focused-window item
disappears when that output has no window title to show.

| Settings name | `kind` | Item-specific options |
| --- | --- | --- |
| Overview | `overview` | Opens the workspace overview |
| Workspaces | `workspaces` | `style`: `numbers`, `dots`, `tabs`, `window-stacks`, `app-icons`; default `dots` |
| Focused window | `focused-window` | Use the shared `visible` field to show or hide the title |
| Media | `media` | Full, Compact and Icon representations |
| Quick Settings | `quick-settings` | Opens Control Center |
| Network | `network` | Opens Wi-Fi controls |
| Audio | `audio` | Opens audio controls |
| Recording | `recording` | Opens recording controls or stops the active recording |
| Notifications | `notifications` | Opens notifications |
| Battery | `battery` | `percentage`: boolean, default `#true` |
| Clock | `clock` | Full, Compact and Icon representations |
| Display mode | `display-mode` | Appears when an external display is connected |

Workspace styles change the regular panel's indicators. Numbers shows workspace
numbers and Tabs shows their names, while Dots uses a pill for the active
workspace. Window stacks uses
small window outlines, and App icons shows up to two distinct running apps.
Hovering an indicator shows its workspace name and window count. Clock
instances share the shell's clock format and timezone.

Unknown options are rejected, so put custom annotations inside `metadata`:

```kdl
item "desk-clock" kind="clock" {
    metadata { note "Clock beside the audio control"; }
}
```

### Adaptation and overflow

In the inspector, **Full**, **Compact** and **Icon** correspond to `wide`,
`compact` and `icon`. Media and Clock support all three; Battery with percentage
supports Full and Icon. Workspaces and Focused window have one form that can
shrink, while other controls use icons. An unsupported preference falls back
to the item's normal form.

When space runs short, lower-priority items yield first, with their order in the
composition breaking ties. Flexible content can shrink, and items can switch
to smaller supported forms; overflow keeps the remaining controls accessible
from the chevron. **Keep visible** maps to `never`, **When needed** to `auto`,
and **Always** to `always`. Even `never` can move into overflow if the mandatory
controls cannot fit at their minimum sizes.

The overflow chevron appears before the End controls, and selecting an entry
opens that item's usual controls. The panel itself does not scroll; workspace
indicators may scroll within the space they receive, while long window titles
end with an ellipsis.

## Morph

Morph combines the panel into one centered hub. Alongside a compact workspace
strip, it shows artwork, the track title and an equalizer during playback.
Otherwise it shows the focused-window title, falling back to the date when
there is no title. Recording and low-battery states take priority over ordinary
content. Hovering can expand a preview, and clicking opens the hub's Workspaces,
Music, Calendar,
Notifications or Controls view. Escape or the close button collapses it.

With an Activities item configured, running tasks can occupy the collapsed and
peek states, and the hub gains an Activities tab. **Visible activities** and
**Show activity count** in Appearance control its compact slots and `+n` badge;
see [Activities configuration](configuration/activities.md).

The Workspaces view shows miniature window layouts. Click a tile to switch
workspace, use Tab and the arrow keys to navigate it, or drag a miniature window
to another tile on the same output. Scrolling over the collapsed hub changes
workspace, with a brief workspace preview after a switch.

Morph keeps your saved composition for when you switch back to a regular panel.
The hub uses its own layouts and compact workspace dots, so Start/Center/End
positions, group padding and the regular workspace indicator style do not apply
inside it. `inner-padding` and `side-margins` also belong to the regular panel;
Morph uses its own content spacing and width budgets. Item settings still
determine which configured controls appear in its Controls view and shortcuts.

Turn on **Attach to screen edge** to join the hub to the chosen edge with curved
shoulders, or set `morph { attached #true; }`. **Preview on hover** maps to
`peek-on-hover`; disabling it leaves click-to-open behavior available. These
two switches appear in Settings when Morph is selected, while the size values
below can be adjusted in KDL.

| `morph` key | Default | Meaning |
| --- | --- | --- |
| `attached` | `#false` | Join the hub to the selected screen edge |
| `peek-on-hover` | `#true` | Show a preview when the pointer enters |
| `idle-width` | 150 | Collapsed content width budget |
| `peek-width` | 260 | Hover preview width budget |
| `peek-height` | 44 | Hover preview height |
| `toast-width` | 320 | Notification toast width |
| `toast-height` | 48 | Notification toast height |
| `expanded-width` | 340 | Width of expanded views |
| `workspaces-height` | 176 | Workspace view height |
| `media-height` | 170 | Music view height |
| `calendar-height` | 230 | Calendar view height |
| `notifications-height` | 176 | Notifications view height |
| `controls-height` | 248 | Maximum Controls view height before scrolling |
| `expanded-radius` | 26 | Default corners for expanded views |

All width and height values must be finite numbers from 24 to 1024, and
`expanded-radius` accepts 0–128. The collapsed hub and workspace feedback fit
their content, Controls may open at a shorter height, and all views stay within
the available output area. A configured width therefore allows room for content
without forcing empty space around it.

For ordinary media and date previews, the maximum width is `peek-width` clamped
to 200–280 pixels. Activity previews choose their own width, and workspace
feedback can be narrower. If you have pinned the panel's corners, those values
override `expanded-radius`.

Morph follows animation speed and reduced motion. As it expands, the area that
accepts clicks follows its shape while the space kept clear of windows stays
unchanged; if you interrupt a transition, it continues from the shape already
on screen.

## Examples

### Joined groups with a centered title

This composition puts Overview and Workspaces in separate Start groups so
Capsules can join them with a divider. The title stays centered, and the two
End groups keep status controls together while separating the clock.

```kdl
panel "main" {
    edge "top"
    style "capsules"
    border #true
    background-opacity 0.85
    geometry {
        height 36
        edge-margin 8
        side-margins 8
        window-clearance 4
    }
    start {
        group "overview" {
            item "overview" kind="overview" overflow="never" priority=100
        }
        group "navigation" {
            item "workspaces" kind="workspaces" style="dots" overflow="never" priority=100
        }
    }
    center {
        group "title" {
            padding 0 0
            item "title" kind="focused-window" overflow="never"
        }
    }
    end {
        group "status" {
            item "media" kind="media" representation="wide"
            item "quick-settings" kind="quick-settings"
            item "network" kind="network"
            item "audio" kind="audio"
            item "recording" kind="recording"
            item "notifications" kind="notifications"
            item "battery" kind="battery" percentage=#true
        }
        group "time" {
            item "clock" kind="clock" representation="compact"
            item "display" kind="display-mode"
        }
    }
}
```

Change only `style` to `floating` or `fused` to compare the same composition
with separate or blended islands, or use `adaptive` to let window state choose
between Floating and Edge to edge.

### An attached Morph hub

Keep the groups from the previous example, set `style "morph"`, and add this
section inside the same panel block. Changing `edge` to `bottom` mirrors the
attached shape without changing the profile.

```kdl
morph {
    attached #true
    peek-on-hover #true
    expanded-width 360
    media-height 180
    controls-height 248
}
```

To return to a floating hub, set `attached #false`; its saved edge margin then
applies again. The profile stays in the file when you choose another style, so
you can return to Morph without re-entering these values.

## Validate a file

Save your edit and run `ferese --check-config ~/.config/ferese/config.kdl` to
validate it without applying changes. The file accepts one `panel` block;
unknown fields, duplicate IDs, unavailable styles and out-of-range dimensions
are errors. See [Saving and validation](configuration.md#saving-and-validation)
for how Settings and live reloads handle a failed validation.
