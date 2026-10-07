# Configuration

Edit `~/.config/ferese/config.kdl` (or `$XDG_CONFIG_HOME/ferese/config.kdl`),
or open **Control Center → Settings**. Start with the [example](../packaging/config.kdl),
which includes app and media shortcuts, appearance, widgets, and optional startup
and display settings. Its browser/editor shortcuts need Firefox/Zed; brightness
keys need `brightnessctl` and audio keys need `wpctl`.
This reference covers layouts, appearance, input, and desktop behavior.
See [Desktop portals](portals.md) for application integration options.

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

Use repeated `binding`, `window-rule`, `output-profile`, `output`, `note` and
`autostart` nodes for lists of settings. Bindings accept keys, an action and its
argument positionally. Profiles, outputs and notes accept a positional name,
match or ID; other fields can be properties or child nodes. Autostart commands
use positional arguments. Duplicate fields are rejected.
The dotted paths in the reference tables below describe nested sections.

Settings preserves comments and custom fields when editing, while normalizing
indentation.

## Saving and validation

Settings saves text when you press Enter or leave the field, and sliders when
you release them. Undo restores the previous save. Reload reads external edits
without merging unfinished drafts. Wallpaper browsing needs `zenity`, but you
can also enter a path directly.
Settings keeps `config.kdl.settings-backup` before saving.

File edits reload automatically, including atomic editor saves. Invalid changes
keep the last working configuration. Appearance, wallpaper, motion, input,
bindings, rules, layouts, displays, widgets and login items update live.

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

Defaults below are built-in defaults, not your personal or packaged overrides.
Dimensions are logical pixels unless noted; numbers must be finite. Optional
values are omitted, not `null`. Colors use `#RRGGBB` or `#RRGGBBAA`.
Live config is limited to 60 KiB.

## Layout

| `layout` key | Type / values | Default | Meaning |
| --- | --- | --- | --- |
| `mode` | `"scrolling"`, `"tree"` | `"scrolling"` | Workspace layout |
| `inner-gap` | number ≥ 0 | `10` | Between windows |
| `outer-gap` | number ≥ 0 | `4` | Around workspace edges |
| `smart-gaps` | boolean | `false` | Remove outer gaps for one tiled window |

| `scrolling` key | Type / values | Default | Meaning |
| --- | --- | --- | --- |
| `default-column-width` | proportion > 0; or `"full"` | `0.5` | Width of new columns |
| `focus-strategy` | `"minimal"`, `"center_on_focus"`, `"paged"` | `"minimal"` | Viewport movement on focus |
| `width-presets` | array of widths | `[0.3333333333333333, 0.5, 0.6666666666666666, "full"]` | Super+R cycle; empty uses defaults |

`minimal` leaves fully visible columns in place and scrolls only enough to reveal
a hidden edge, ignoring differences of up to 0.5 logical pixels. Full-width
columns align with the viewport. Wider columns reveal their left edge when
approached from the left and their right edge when approached from the right;
an existing view inside a wider column stays in place. `center_on_focus` centers
the focused column. `paged` packs columns into viewport-sized pages: halves form pairs, thirds
form triples; mixed widths and effective minimum sizes determine actual boundaries.
Changing default width preserves manually resized columns.

## Animations

Continuum is Ferese's shared compositor and shell motion system. Set animation
speed in Settings → Motion or edit the `animations` block in
`~/.config/ferese/config.kdl`. Changes apply live. Start with `speed` if you only
want faster or slower transitions; you do not need to change the springs.

```kdl
animations {
    enabled #true
    reduced-motion #false
    speed 1.0

    spring {
        duration-ms 240.0
        bounce 0.0
        overshoot #false
    }

    viewport-spring {
        duration-ms 350.0
        bounce 0.0
        overshoot #false
    }
}
```

This example uses the built-in defaults. The packaged config sets `speed 0.9`.
You can omit either spring block to keep its defaults.

| Section / key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `animations.enabled` | boolean | `true` | Enable motion |
| `animations.reduced-motion` | boolean | `false` | Disable animated motion |
| `animations.speed` | number > 0 | `1` | Higher is faster; `0.75` is slower |
| `animations.spring.duration-ms` | number > 0 | `240` | Window and shell spring response |
| `animations.viewport-spring.duration-ms` | number > 0 | `350` | Scrolling and workspace spring response |

Both `spring` and `viewport-spring` accept these keys:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `duration-ms` | number > 0 | `240` or `350`, as above | Larger values give a slower response; not a completion deadline |
| `bounce` | number strictly between −1 and 1 | `0` | Zero is critical damping; positive values require overshoot |
| `overshoot` | boolean | `false` | Allow this spring to cross its target |

Use duration and bounce to configure spring physics. `mass`, `stiffness`,
`damping` and `damping-ratio` are internal solver parameters, not configuration
keys. Removed or misspelled animation keys are rejected. Invalid edits leave
the last accepted configuration active.

### Speed and reduced motion

`speed` changes window motion, scrolling, workspace transitions, overview,
theme transitions and shell popup, notification and hover transitions. `0.5`
gives an animation twice as much time; `2.0` gives it half as much. Overview
uses 60% of this speed when opening or closing, including when selecting a
window. There is no separate overview-speed setting.

Set `reduced-motion #true` or `enabled #false` to make these transitions
immediate. Reduced motion takes precedence over `enabled #true`; setting
`speed 0` is invalid. These settings control Ferese's animations, not animations
inside other applications.

Shell animations sample motion on each surface's redraw, paced by its Wayland
frame callbacks. They request animation frames only while moving.
Menus, modals, notifications and hover fades use this path instead of a fixed
16 ms animation timer. Clock, notification-expiry and service updates keep their
own schedules.

Overview can reverse while moving, and workspace changes slide in their navigation
direction. Workspace and scrolling gestures carry their release velocity into
the settling spring. Scrolling gestures resist at viewport limits and spring back.
Window opening starts when content is available; closing waits for the application
to unmap, then animates its retained image. Input and application close requests
remain responsive while the animations finish.

Live windows, overview thumbnails and retained images share a window identity
and presentation state. Entering overview inherits the window's visible position
and velocity, including motion from a workspace slide. Resize images use the same
content mapping in overview and remain visible if the window closes during a resize.
Content, clipping and borders use the same squircle outline at the output's scale.

### Spring tuning

`spring` controls window position and size motion, fullscreen/maximize zoom and
overview entrance and dismissal, opening/closing windows, focus emphasis, shell
popups and notifications. `viewport-spring` controls the scrolling viewport,
workspace slides and the column-width animation that runs with scrolling.
Continuum shares the analytic spring solver and motion settings between the
compositor and shell. Small opacity changes (hover, dimming, theme fades) may
still use timed transitions.

Focus emphasis and shadows have separate spring responses. Emphasis uses 80% of
the main spring's response time; shadows use 120%. These ratios preserve the
configured damping ratio and overshoot policy, and follow `speed` and reduced
motion. They are built-in tuning values, not additional configuration keys.
Focused windows use the theme's full shadow. Unfocused shadows use 75% of its
vertical offset, 90% of its blur radius and 80% of its opacity, with spring motion
between those values. Shadow motion does not change the content or border geometry.

Spring settling time depends on distance, velocity and tolerances. `duration-ms`
controls the response, not a fixed completion deadline. Both default springs are
critically damped; the viewport has a slower response than windows and shell popups.

To opt into bounce for one spring:

```kdl
animations {
    spring {
        duration-ms 300.0
        bounce 0.2
        overshoot #true
    }
}
```

Bounce defaults to zero and must be strictly between -1 and 1. Positive bounce
requires `overshoot #true` in that spring block. Negative bounce adds overdamping.
Without overshoot, Ferese stops a spring at its first target crossing. Opacity
remains bounded even when geometry can overshoot. Overshoot is never enabled globally.

For the solver, mass is fixed at `1`. With `T = duration-ms / 1000`, Continuum uses
`omega = 2*pi/T`, `stiffness = omega^2` and `damping = 2*omega*zeta`.
`zeta = 1-bounce` for nonnegative bounce, otherwise `zeta = 1/(1+bounce)`.
This conversion does not guarantee the same perceptual duration or settling time
as Apple's springs.

Duration and speed must be finite and greater than zero. Bounce must be finite.
Values that overflow or underflow the derived coefficients are rejected. The
allowed bounce range keeps damping positive, so undamped oscillation cannot
prevent popup cleanup.

## Appearance

Settings → Appearance offers six paired themes: **Ferese Blue** (default),
**Catppuccin**, **Gruvbox**, **Rosé Pine**, **Tokyo Night**, and **Everforest**.
Ferese Blue uses the default accent, `#3D7BE6`, and has light and dark variants.
Choose Light, Dark, or Auto in Settings or Control Center. Auto follows local
07:00 and 19:00 boundaries unless you change its schedule.

Selections store preset names. Partial KDL files can override shared colors or
one appearance.

| Section / key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `theme.mode` | `"light"`, `"dark"`, `"auto"` | `"dark"` | Selected appearance mode |
| `theme.family` | family id | `"ferese-blue"` | Paired theme; automatically uses its light or dark variant |
| `theme.split` | boolean | `false` | Use separate light/dark families; retains both selections when disabled |
| `theme.light.family`, `theme.dark.family` | family id | `"ferese-blue"` | Stored selections used when split is enabled |
| `theme.custom-themes.<id>.file` | path | unset | Imported KDL family with explicit light and/or dark sections |
| `theme.light.preset` | preset name | `"ferese-blue-light"` | Light palette |
| `theme.dark.preset` | preset name | `"ferese-blue"` | Dark palette |
| `theme.file` | path | unset | Shared partial KDL theme |
| `theme.light.file`, `theme.dark.file` | path | unset | Partial KDL theme for one appearance |
| `theme.accent` | hex color | unset | Accent override, adjusted for readability in each appearance |
| `theme.schedule.source` | `"system"`, `"schedule"` | `"schedule"` | Follow GTK/GNOME appearance or a custom schedule; sunrise/sunset is deferred |
| `theme.schedule.timezone` | `"system"` or IANA timezone | `"system"` | Auto schedule timezone |
| `theme.schedule.light-at`, `theme.schedule.dark-at` | `HH:MM` | `"07:00"`, `"19:00"` | Auto boundaries; times must differ |
| `theme.accessibility.increase-contrast` | boolean | `false` | Strengthen text and borders |
| `theme.accessibility.reduce-transparency` | boolean | `false` | Solid surfaces; skip background blur |
| `appearance.corner-radius` | number ≥ 0 | unset | Legacy fallback for shell radius |
| `appearance.focus-effect.enabled` | boolean | `true` | Apply window opacity and dimming; disabling keeps the configured values |
| `appearance.focus-effect.active-opacity` | number 0–1 | `1` | Active application opacity, including fullscreen |
| `appearance.focus-effect.inactive-opacity` | number 0–1 | `1` | Other application window opacity |
| `appearance.focus-effect.inactive-dim` | number 0–1 | `0` | Darkening strength for inactive windows; 0 disables |
| `appearance.focus-effect.duration-ms` | number ≥ 0 | `150` | Shared opacity and dimming transition; 0 snaps |
| `theme.typography.font-family` | string | `"Inter"` | Shell, Settings and overview font |
| `theme.background.path` | string | matching light/dark wallpaper | Shared wallpaper; an explicit path overrides both bundled defaults |
| `theme.light.background.path`, `theme.dark.background.path` | string | shared path, or matching default | Wallpaper for one appearance; overrides the shared path |
| `theme.background.mode` | `"fill"`, `"fit"` | `"fill"` | Crop or letterbox |
| `theme.light.background.mode`, `theme.dark.background.mode` | `"fill"`, `"fit"` | shared placement | Image placement for one appearance |
| `theme.material.style` | `"solid"`, `"translucent"` | `"solid"` | Shell background material |
| `theme.material.opacity` | number 0–1 | `0.78` | Shared shell background opacity for bars, menus, popovers, notifications and themed dialogs. Text/icons stay opaque; 0 hides the material. Solid mode is always opaque |
| `theme.material.tint-strength` | number 0–1 | Dark `0.5`, light `1` | Color strength over the blurred backdrop |
| `theme.material.blur-radius` | number ≥ 0 | `12` | Translucent backdrop blur, capped at 32; 0 disables |

Window opacity follows the active application. Giving keyboard focus to a shell
menu does not change it. Overview selection changes the highlight, while window
opacity still follows activation; dimming fades out as overview opens. Fullscreen
windows use the same opacity settings. Opening and closing fades multiply window
opacity. Reduced motion disables the focus transition.

Settings → Wallpaper shows the current image and both bundled defaults. Use
**Apply to** to change both modes or just one. **Match appearance** restores the
bundled light and dark pair. Custom images can also be selected separately in KDL:

```kdl
theme {
    light {
        background { path "~/Pictures/day.png"; }
    }
    dark {
        background { path "~/Pictures/night.jpg"; }
    }
}
```

The wallpaper follows manual light/dark changes and automatic appearance changes.
A shared `theme.background.path` applies to both modes unless a per-mode path
is set.

| `theme.colors` key | Default | Meaning |
| --- | --- | --- |
| `surface-base` | `"#111821"` | Material/card base color |
| `text-primary` | `"#F4F7FB"` | Main text |
| `text-muted` | `"#8793A2"` | Secondary text |
| `accent` | `"#3D7BE6"` | Active controls and solid focus ring |
| `border` | `"#FFFFFF18"` | Unfocused window border |
| `shadow` | `"#00000055"` | Window shadow color |

Filled controls choose a contrasting text color automatically.

| `theme.geometry` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `border-width` | number ≥ 0 | `1` | Window border thickness |
| `focus-ring-width` | number ≥ 0 | `2` | Focused border thickness |
| `window-radius` | number ≥ 0 | `14` | Managed-window corners, independent of the shell |
| `shell-radius` | number ≥ 0 | `14` | All shell surfaces, cards, widgets and interaction backgrounds; fractional radii are preserved; 0 makes them square |
| `top-bar-height` | number > 0 | `28` | Menu-bar height |
| `top-bar-margin-top` | integer ≥ 0 | `0` | Space above bar |
| `top-bar-window-gap` | integer ≥ 0 | `0` | Clearance below bar |
| `top-bar-margin-horizontal` | integer ≥ 0 | `0` | Bar side margins |
| `top-bar-radius` | number ≥ 0 | unset | Legacy fallback for shell radius |
| `panel-padding` | number ≥ 0 | `12` | Bar inner padding |
| `control-gap` | number ≥ 0 | `12` | Right-side control spacing |

| Section / key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `theme.surface.bar.background` | color | `"#1C202EF2"` | Shell fallback background; compositor materials use `surface-base` |
| `theme.surface.bar.text-primary` | color | `"#F0F3FA"` | Bar text/icons |
| `theme.surface.bar.text-muted` | color | `"#AAB4C7"` | Inactive bar foreground |
| `theme.shadow.soft.offset-y` | number | `4` | Window shadow vertical offset |
| `theme.shadow.soft.blur` | number ≥ 0 | `18` | Window shadow softness |
| `theme.shadow.soft.opacity` | number 0–1 | `0.20` | Window shadow strength |

Solid materials omit blur; translucent materials blur behind the surface color.
True fullscreen removes decorations. Focus transitions never scale the content.

Window focus borders and selected controls use an accent gradient by default.
Built-in themes supply the second color; when a theme has one accent color,
Ferese derives a nearby lighter or darker shade. Changing the accent replaces
the automatic gradient. Inactive window borders derive their gradient from the
border color and keep its transparency.
Automatic accent endpoints are adjusted for contrast against the theme's surfaces.

In Settings → Appearance, choose **Gradient** or **Solid** for **Accent style**
and **Inactive border style**. In KDL, `theme.focus-ring.style` and
`theme.border.style` accept `"auto"` (default) or `"solid"`. Solid disables the
gradient, including any explicit endpoints, without deleting them from the config.

For custom endpoints, set `theme.focus-ring.gradient` or `theme.border.gradient`.
Both require `from` and `to` colors; `angle` is optional and defaults to `0`.
Angles are clockwise: 0 is left-to-right, 90 top-to-bottom. Explicit gradients
take precedence over automatically derived colors. The automatic angle is 135°.

```kdl
theme {
    focus-ring {
        gradient {
            from "#e5c890"
            to "#b98d58"
            angle 135.0
        }
    }
}
```

For solid colors:

```kdl
theme {
    focus-ring { style "solid"; }
    border { style "solid"; }
}
```

## Workspaces

Each monitor has an independent list of workspaces, numbered from 1. Its bar,
overview strip, and numeric shortcuts use that same order. Numbers change when
empty workspaces are removed; internal workspace IDs stay stable.

One empty workspace is always available at the end of each monitor's list.
Opening or moving a window there creates another spare. An empty workspace stays
while displayed or participating in a swipe, then disappears after you leave it.
Selecting a number beyond the current list selects the trailing empty workspace.
Window-rule workspace numbers also refer to positions on the window's monitor.

`Super+Escape` returns to the previous surviving workspace on the focused monitor.
History does not keep abandoned empty workspaces alive. With no previous workspace,
the action does nothing. Disconnecting a monitor clears its history; occupied
workspaces move to another monitor and can return when it reconnects.

Enable **Settings → Windows → Toggle back with the same workspace shortcut**, or:

```kdl
workspaces {
    auto-back-and-forth #true
}
```

This defaults to `#false`. When enabled, pressing a `workspace` binding for the
current workspace returns to the previous one. Explicit `feresectl workspace N`
commands and overview selection still select the requested workspace directly.
Use `feresectl workspace-back-and-forth` for an explicit toggle, or bind
`workspace-back-and-forth` to another key or gesture. The action takes no argument.

`Super+BackSpace` focuses the last focused window, including windows on other
workspaces or monitors. Repeating it toggles between the two most recently
focused windows. Layer surfaces such as launchers do not replace this history.

`Alt+Tab` previews windows in focus order using overview. Keep Alt held and press
Tab to advance, or Shift+Tab to go backward. Release Alt to focus the selection;
Escape cancels without changing focus. The order stays fixed until selection is
committed. Closed windows are skipped and newly opened windows join the next
cycle. Preview transitions follow the configured animation speed and reduced
motion settings.

Bind `focus-last-window`, `focus-mru-next`, or `focus-mru-previous` to customize
these shortcuts. MRU bindings with Alt held preview until Alt is released;
bindings without Alt, gestures, and the corresponding `feresectl` commands
switch immediately. All three actions take no arguments.

## Input

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

Assign swipes in **Settings → Shortcuts** using the same actions and arguments as
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

## Commands and bindings

See [Shortcuts and gestures](shortcuts.md) for the full default keymap and mouse actions.

`commands` maps arbitrary names to nonempty argument arrays. The built-in
`terminal "foot"` can be overridden. Commands run directly, without a shell.

| `binding` key | Type / values | Default |
| --- | --- | --- |
| `keys` | chord or gesture, e.g. `"Super+Enter"`, `"Swipe3Up"` | required |
| `match` | `"keysym"`, `"physical"` | `"keysym"` |
| `action` | action name below | required unless disabled |
| `argument` | string | required only for actions listed below |
| `disabled` | boolean | `false` |

Modifiers: `Super`/`Logo`/`Mod4`, `Ctrl`/`Control`, `Alt`, `Shift`.
Keys use XKB keysyms (`Enter`, letters, `[`/`]`) or physical XKB names (`AD06`).
A binding replaces the same normalized chord/match mode. Duplicate bindings are
invalid. A disabled binding must omit `action` and `argument`.

| Actions | Argument |
| --- | --- |
| `spawn` | Name in `commands` |
| `focus`, `move`, `resize` | `"left"`, `"right"`, `"up"`, `"down"` |
| `workspace`, `move-to-workspace` | Workspace number string, 1–255 |
| `workspace-next`, `workspace-previous` | None; next/previous workspace on this monitor |
| `workspace-back-and-forth` | None; return to the previous workspace on this monitor |
| `focus-last-window` | None; return to the last focused window |
| `focus-floating` | None; focus the last focused window in the opposite tiled/floating layer on this workspace |
| `focus-mru-next`, `focus-mru-previous` | None; cycle windows in focus order |
| `none` | None; ignore this trigger |
| `close`, `exit`, `toggle-maximized`, `toggle-fullscreen`, `toggle-layout`, `cycle-column-width`, `center-column`, `consume`, `expel`, `toggle-floating`, `toggle-overview` | None |

```kdl
commands {
    terminal "foot"
}
binding keys="Super+Enter" action="spawn" argument="terminal"
binding keys="Super+V" action="focus-floating"
```

`focus-floating` switches focus between tiled and floating windows without
changing placement. It stays on the active workspace and does nothing when the
opposite layer has no candidate. `toggle-floating` changes window placement.

Defaults: Super+Enter terminal; Super+Q close; Super+H/J/K/L focus;
Super+Shift+H/J/K/L move; Super+Ctrl+H/J/K/L resize; Super+1–9 workspace;
Super+Shift+1–9 move to workspace; Super+R width cycle; Super+C center;
Super+[/] consume/expel; Super+F maximize; Super+Shift+F fullscreen;
Super+M layout; Super+Shift+Space floating; Super+V focus tiled/floating; Super+Tab overview;
Super+Escape previous visited workspace;
Super+BackSpace last focused window; Alt+Tab/Alt+Shift+Tab MRU switching;
Super+Shift+S area screenshot; Print Screen whole-screen screenshot;
Super+Shift+E logout confirmation.

Hold Super and drag a floating window with the left mouse button to move it.
Hold Super and drag with the right mouse button to resize. The starting position
selects an edge or corner using a 3×3 grid; the center selects the nearest corner.
The selected edges stay fixed for the whole drag. These work even when an app has
no title bar or resize border.

Moving and resizing snap to work-area edges and nearby visible window edges at
10 logical pixels, then resist movement until 20 pixels from the attached edge.
Window edges are eligible only when the windows overlap along the other axis,
with a 10-pixel allowance. Hold Shift during a drag to bypass snapping.

The screenshot shortcut runs `ferese-screenshot`: drag to select an area, or
press Escape to cancel. Captures open in Satty for annotation. Press Enter to
save the edited PNG under your Pictures directory in `Screenshots` and copy it
to the clipboard; Escape discards the capture. It requires `slurp`, `satty`, and
`wl-copy` (from `wl-clipboard`); the capture itself is done by the compositor
through `feresectl screenshot`. Override the `screenshot` command to use another
screenshot tool.
Print Screen runs `ferese-screenshot --full` and captures the active monitor
without a selector, falling back to the sole enabled output when focus is
unavailable. It never silently captures every output. Fn+PrtSc works when the
keyboard emits the Print Screen key; Fn is handled by the keyboard firmware.
Override `screenshot-full` to customize this command. Both modes open Satty with
the same save and copy workflow.

`ferese-screenshot --all` captures every enabled output as one image.

Satty opens as a floating window sized for the captured image and constrained
to the workspace.

## Window rules

Native authentication and display-sharing dialogs float by default. Explicit
window rules can override that behavior.

| `window-rule` key | Type | Default / meaning |
| --- | --- | --- |
| `app-id` | nonempty string | Optional exact match; case-insensitive, `.desktop` suffix ignored |
| `title` | nonempty string | Optional exact, case-sensitive match |
| `transient` | boolean | Optional parent-dialog match |
| `workspace` | integer > 0 | Leave placement unchanged |
| `floating` | boolean | Leave placement unchanged |
| `width`, `height` | numbers > 0 | Application-chosen floating size |
| `min-width`, `min-height` | numbers > 0 | Replaces the client's advertised minimum size for that axis |
| `fullscreen` | boolean | Leave fullscreen state unchanged |
| `block-out-from-screencasts` | boolean | Exclude from captures; enabled by default for Ferese authentication dialogs |
| `idle-inhibit` | `none`, `visible`, `fullscreen`, `playing`, `fullscreen-playing` | Automatic fullscreen playback; see [Idle inhibition](#idle-inhibition) |

`window-rule app-id="spotify" min-width=500 min-height=400` replaces what the
app itself reports as its smallest size, so a configured column width wins over
an app that refuses to shrink (`min-width=1` removes the floor). Each axis is
independent; an omitted axis keeps the client's minimum.

`window-rule app-id="org.example.Private" block-out-from-screencasts=#true`
keeps matching windows visible on the display but omits them (including their
popups, subsurfaces, shadows and overview previews) from monitor and region
captures. The background or windows behind them remain visible in the capture.
Transient children inherit protection even on another output. Direct window
capture fails, ending an active window stream. Custom cursors from a client with
a protected window are also omitted until the cursor image is replaced.
Detached close-animation snapshots are omitted from captures.

This applies to portal sharing, recording, native capture protocols and screenshots:
these routes share capture buffers. The rule is checked against current app-ID,
title and parent metadata, and updates on config reload. Other placement rules
retain their existing lifecycle. Later matching rules may set the field to `#false`,
including for `dev.ferese.Authentication`; a protected parent still protects its
children. A protection change cancels deferred screenshots and window frames.
Frames already delivered to a capture client cannot be recalled.

Every rule needs at least one matcher, and supplied matchers must all match.
Rules apply in order; later fields override earlier ones. Dimensions imply
floating when `floating` is omitted. Floating apps without dimensions choose
their own size. Ferese first tries to center transient windows on the visible
part of their parent, then tries saved geometry, then the position with the least
summed overlap.
Equal-overlap candidates favor the focused window's center. If the size cannot
fit, a per-output cascade advances by 32 logical pixels and wraps to the work-area
origin. Placement respects layer-shell exclusive zones and effective minimum sizes;
an oversized window keeps its size with its top-left corner reachable.

Successful move/resize completion saves ordinary floating geometry by app ID in
`$XDG_STATE_HOME/ferese/floating.json` (normally `~/.local/state/ferese/floating.json`).
Geometry is stored with the output name and as fractions of its work area, so
placement survives resolution changes. Restores are skipped if the output is
missing or less than 25% of the window would be visible. A worker saves the
latest snapshot, combining pending updates. Completing a drag updates memory
immediately without waiting for storage.
Transient dialogs and cancelled drags do not update this memory. Floating → tiled
→ floating also restores the window's last floating geometry. Nested previews
keep drag memory in process and do not write the user's saved placements.

Title changes do not reapply placement rules. Capture privacy and idle inhibition
follow current app ID, title and parent metadata. Config rule changes apply to
existing windows.

```kdl
window-rule app-id="dev.ferese.Settings" floating=#true
```

## Displays

Each `output-profile` needs a unique, nonempty `name` and at least one `output`
entry. A profile matches when every required monitor is connected and its
optional `lid-closed` condition matches. The profile with the most required
monitors wins; ties use config order. If none matches, Ferese extends across all
usable connected monitors. Find selectors with `feresectl outputs`.

| `output-profile` key | Type | Default |
| --- | --- | --- |
| `name` | unique nonempty string | required |
| `layout` | `internal-only`, `external-only`, `extend`, `mirror` | `extend` |
| `lid-closed` | boolean | either lid state |
| `mirror-source` | selector of an enabled output in this profile | first enabled hardware key |
| `lid-policy` | `dock-or-suspend`, `ignore` | `dock-or-suspend` |
| `confirm-timeout` | seconds; `0` disables confirmation | `15` |

| `output-profile → output` key | Type | Default |
| --- | --- | --- |
| `match` | nonempty connector or persistent identity string | required; unique within profile |
| `required` | boolean | `true`; set `false` for an optional monitor |
| `enabled` | boolean | `true` |
| `mode` | `"WIDTHxHEIGHT"` or `"WIDTHxHEIGHT@HZ"` | Preferred mode |
| `auto-refresh` | boolean | `false` |
| `scale` | number > 0 | `1` |
| `transform` | enum below | `"normal"` |
| `position` | `[integer, integer]` | Automatic horizontal placement |

Transforms: `normal`, `rotate-90`, `rotate-180`, `rotate-270`, `flipped`,
`flipped-90`, `flipped-180`, `flipped-270`. In extend and mirror profiles,
unlisted monitors stay enabled with defaults. Internal-only and external-only
filter by connector type. A layout that would disable the final usable display
falls back to a working display. Profiles that explicitly disable all their
listed outputs are invalid.

Monitor identities use EDID manufacturer, product/model and serial, rather than
a hash of the entire EDID. Without a usable serial, the identity includes the
GPU's sysfs hardware path and connector. For duplicate serials, Ferese adds the
same path and connector to distinguish the monitors. If both arrive together,
both identities include those details. If the second arrives later, the first
keeps its identity and only the second gets the path and connector. Ferese
remembers that choice for the compositor session. Without serials, Ferese
identifies monitors by port and cannot tell them apart if you swap their ports.
Connector names and old `drm-edid:` selectors still work. Ambiguous or
overlapping selectors are rejected during live reload. A disconnected identity
can remain in config; it prevents matching only when its entry has
`required=#true`.

```kdl
output-profile "desk" layout="external-only" {
    output "DP-1" mode="2560x1440@60" scale=1.25
    output "eDP-1" required=#false enabled=#false
}

output-profile "mobile" layout="internal-only" {
    output "eDP-1" scale=1.75
}

output-profile "presentation" layout="mirror" mirror-source="eDP-1" {
    output "eDP-1" scale=1.75
    output "HDMI-A-1" mode="1920x1080@60"
}
```

Mirror uses the source's logical size, workspace, layers and pointer. Targets
own no workspaces or separate pointer region. Ferese fits the complete source
into each target's transformed physical mode, preserving aspect ratio and
letterboxing instead of cropping. Target scale does not resize the source
layout. Each target has its own hardware cadence; only the source releases
client frame callbacks and presentation feedback. Mirrored monitors are listed
by `outputs`, but expose one logical output to clients and the shell. If a required mirror target disconnects, Ferese selects another matching
profile or extends across all usable connected monitors. Leaving mirror
restores evacuated workspaces.

```sh
feresectl outputs
feresectl output-profiles
feresectl toggle-display-mode
feresectl output-layout extend
feresectl output-profile presentation
feresectl output-confirm
feresectl output-profile auto
feresectl output-internal off
feresectl output-revert
```

The display Fn key (`XF86Display`) and the bar’s display icon open the same
display-mode chooser. The icon appears while an external monitor is connected.
`output-layout` selects a layout without requiring a named profile and keeps
the matching profile’s mode, scale and transform settings.

A manual choice overrides automatic selection until the monitor set or lid
state changes. Reload keeps it only if its profile still exists and matches.
Manual changes revert after `confirm-timeout` seconds unless confirmed;
`output-revert` restores immediately. Reverting restores the last confirmed modes,
scales, transforms and positions, even if you make several changes before
confirming. The timeout still applies if the client that made the change exits. Automatic hotplug/lid changes never wait for confirmation.
Disabling the last usable internal panel is rejected.

```kdl
output-profile name="docked" {
    output match="HDMI-A-1" scale=1.5 {
        position 0 0
    }
}
```

Settings → Displays offers the connected display's supported refresh rates at
its configured resolution. Choosing a rate saves it and disables Auto. Choosing
Auto saves the highest supported rate as the normal mode and enables automatic
switching. These choices apply immediately for the active profile and persist
across restarts.

With `auto-refresh=#true`, a discharging system battery below 30% switches the
display to a supported mode within 1 Hz of 60 Hz at the same resolution. The
configured normal mode returns on external power or when the battery reaches
35%. Power is checked every five seconds; mode changes only run when the policy
changes. Unavailable battery data restores normal refresh. If no matching 60 Hz
mode exists, normal refresh is retained. Rejected DRM mode changes retain the
current output. A refresh-rate change can briefly blank the display (about one
second on some panels), including automatic changes below 30%, recovery at 35%,
and restoration on AC. Session activation refreshes lid and connector state
before rendering; unchanged outputs retain their handles and workspaces.
Hotplug bursts settle for 150 ms before one reconciliation. Ferese records changes that arrive while the session is inactive and applies
them on activation.

```kdl
output-profile laptop {
    output eDP-1 mode="2880x1800@120" scale=1.75 auto-refresh=#true
}
```

With a usable external display, closing the lid disables internal panels after
external activation succeeds. Without one, Ferese asks logind to suspend,
respecting sleep inhibitors. `lid-policy="ignore"` disables this behavior for
the selected profile. Resume reads lid state from logind, with ACPI fallback,
before reconciling fresh hardware. Ferese releases its logind lid inhibitor on
session pause, so logind handles the lid while another session or TTY is active.
Workspaces keep their home output and return when it reconnects, unless you
reassigned or deleted them after evacuation. Floating
windows are remapped and clamped to the surviving display.

On atomic DRM, Ferese tests the complete configuration for each GPU before
changing it.
Smithay performs the real commits; logical changes follow successful hardware
application. Failed commits attempt restoration, and outputs that cannot be
restored are removed from logical state. Each GPU commits separately. If a later GPU fails, Ferese attempts to restore
the earlier GPUs. Legacy KMS lacks atomic
test-only validation. Hardware failures can still prevent every monitor from
working; Ferese tries preferred modes to recover a display instead of
intentionally turning off the last working one.

## Status controls

| `status` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `bar-layout` | `"continuous"` or `"islands"` | `"continuous"` | One bar background or separate backgrounds around its sections |
| `bar-island-padding` | number 0–32 | `4` | Horizontal space on each side between an island's background and its bordered controls, in logical pixels |
| `keybinding-guide` | boolean | `true` | Show the active shortcut guide at login until disabled in Settings → Shortcuts. |
| `window-title` | boolean | `true` | Focused window title in the bar center when space allows |
| `battery-percentage` | boolean | `true` | Show percentage beside icon |
| `low-battery-threshold` | integer 0–100 | `20` | Warning-color threshold |
| `settings-command` | argument array | `["ferese-settings"]` | Settings launcher; `[]` hides the action |

Choose **Islands** in Settings → Bar, or set it in KDL:

```kdl
status {
    bar-layout "islands"
    bar-island-padding 4
}
```

The gaps between islands are transparent and let clicks pass through. Each island uses the bar background around the existing section's border and fill, with the theme's bar radius and material opacity. Changing the bar layout leaves modal transparency unchanged; there is no need to set the shared opacity to zero. The bar reserves the same space above windows in either layout.

Use **Island side padding** in Settings → Bar to tighten the space around each section. It applies only to islands; continuous bars keep their existing padding.

### Now Playing

The media control on the right shows artwork, the track title and a play/pause
button. Click the artwork or title to open playback controls for MPRIS players.
Scroll over the control to change the player's volume, or
middle-click to raise its window when supported. The menu has previous/next
buttons, a seek slider when supported, and a player chooser. It uses the same
colors, transparency and animations as the other bar menus.

Automatic selection prefers the player that most recently started playing.
When none is playing, it selects the most recently active paused player. Players
already running at startup have no known activity order; ties use their bus names.
Stopped players hide the control. Pin a player through the chooser to keep it
selected, or ignore it to exclude it. These choices last until that player leaves
the bus or the media service reconnects. `Choose automatically` clears the pin.

The bar and media keys share one MPRIS monitor. Playback updates come from D-Bus
signals. Progress updates only while the menu is open and playback is running:
once a second, or every three seconds on battery. Album art loads while the
bar or menu is visible, on a worker with download, decode and cache limits. A placeholder
keeps its space while loading or when art is unavailable.

The default Play/Pause, Next and Previous keys use `media-play-pause`,
`media-next` and `media-previous`. These actions take no arguments. Custom
bindings can use them too:

```kdl
binding "Super+P" "media-play-pause"
```

Inspect players or control the selected one from the terminal:

```sh
feresectl media
feresectl media play-pause
feresectl media next
feresectl media previous
feresectl media pin org.mpris.MediaPlayer2.example
feresectl media ignore org.mpris.MediaPlayer2.example
feresectl media unignore org.mpris.MediaPlayer2.example
feresectl media auto
```

`unignore` restores an ignored player even when the bar control is hidden. Apps
without MPRIS support do not appear here.

## Login items and locking

| `autostart` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `command` | nonempty argument array | required | Foreground process, not a self-daemonizing command |
| `enabled` | boolean | `true` | Start service |
| `restart` | boolean | `true` | Restart after exit, with a five-second retry delay |
| `nested` | boolean | `false` | Also run in nested previews |

Unchanged services keep running on reload; removed/disabled items stop. To lock
after five minutes and before sleep:

```kdl
autostart {
    command "swayidle" "-w" "timeout" "300" "ferese-lock" "before-sleep" "ferese-lock" "lock" "ferese-lock"
}
```

The locked display dims after 30 seconds and sleeps after 120 seconds of
inactivity. Settings → Lock screen changes `lock-screen.dim-after-seconds` and
`lock-screen.sleep-after-seconds`; zero disables each action. Input wakes the
display without unlocking. Display sleep does not suspend the computer.

`ferese-lock` is the native PAM-authenticated locker. It follows your wallpaper,
colors, font and shell corner radius. Its command returns successfully only after
the compositor confirms the lock; the UI keeps running until authentication
succeeds. `--foreground` keeps the command attached to its UI process.

Use `ferese-lock --preview` for an ordinary window that never locks or checks a
password. See [Native locker](locking.md) for setup and verification. Test real
unlock in a nested compositor before enabling automatic locking. A crashed
locker leaves the session locked; recovery requires ending that session from
another TTY.

## Idle inhibition

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

## Optional session protocols

These are launch-time environment switches, not KDL keys. Enable only for
trusted clients: `FERESE_ENABLE_INPUT_METHOD=1`,
`FERESE_ENABLE_SHORTCUT_INHIBIT=1`, `FERESE_ENABLE_SCREENCOPY=1`.
The installed session launcher enables screencopy for screenshots unless
`FERESE_ENABLE_SCREENCOPY=0` is explicitly set in its environment. Setting it
to `0` disables every path that can read the screen, including the built-in
screenshot command, not just the Wayland protocol. A session launched by hand
rather than through `ferese-session` needs `FERESE_ENABLE_SCREENCOPY=1` in its
environment for screenshots to work.
Text input is available by default; Ctrl+Alt+Escape releases an active shortcut
inhibitor. Use `feresectl --help` for runtime control commands.

Shell rounding is controlled in Settings → Appearance → Shell corner radius.
The shell and compositor use the same squircle profile for fills, borders, shadows
and blur masks. Small controls cap the radius to fit their size; circles and pills
keep circular outlines. The measured profile has small tangent and curvature
discontinuities at its internal segment joins. Window rounding remains under
Settings → Windows. `theme.geometry.shell-radius` takes precedence over the old
`appearance.corner-radius` and then `theme.geometry.top-bar-radius` keys; when
none are set, the shell uses 14 px. Legacy clock/note radius fields no longer
override shell rounding.

## X11 support (xwayland-satellite)

Ferese can run legacy X11-only applications through
[xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite).
```kdl
xwayland {
    enabled #true
    startup  on-demand
    path     "xwayland-satellite"
}
```

| Key | Values | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | `true` / `false` | `true` | Run the bridge at all. `false` is a complete opt-out. |
| `startup` | `on-demand` / `eager` | `on-demand` | `on-demand` starts Satellite the first time an application connects to the X11 socket. `eager` starts it during session startup. |
| `path` | path to an executable | `xwayland-satellite` | Which binary to run. |

These keys require a new session. Reloading configuration does not restart or
reallocate the X11 service; `feresectl xwayland status` reports
`restart_required` when the running configuration no longer matches the file.

### Requirements and failure behavior

`xwayland-satellite` and an `Xwayland` binary it can find must be installed.
Satellite must be built with its `systemd` feature enabled
(`cargo build --release --features systemd`); that feature sends the `READY=1`
notification Ferese waits for. Upstream's default feature set is empty, so a
default build never sends it and every start ends in a startup failure after
Ferese's fixed 10-second budget, even though the process is running. Being
spawned is not readiness. If any of these are missing, the session still starts
as a normal Wayland desktop and `feresectl xwayland status` explains why X11 is
unavailable. Ferese never publishes a `DISPLAY` value that does not work, so
applications fail cleanly instead of hanging against an endpoint that accepts
nothing.

In a nested session, Ferese's own environment is preserved for backend
initialization, but applications it launches receive the new Ferese endpoints.
The host's `DISPLAY` is never exported to the activation environment of a nested
session, so it cannot leak into services started by your login session.

### Diagnostics and recovery

```text
feresectl xwayland status
feresectl xwayland retry
```

`status` reports whether the service is idle, starting, running, backing off, or
failed, plus the display, the Satellite PID, and whether readiness was verified.
A failed service is not restarted automatically: Ferese retries a small bounded
number of times, then stops and waits, so a broken binary cannot produce a
restart loop. Run `feresectl xwayland retry` once the cause is fixed. The
snapshot never contains the X11 cookie, so it is safe to paste into a bug
report.

`retry` can also refuse to run. If the previous service group cannot be proven
gone — for example it survived the stop sequence — Ferese keeps its cleanup
record instead of starting a second generation that would compete with
survivors for the same display, and the retry fails with a message naming what
still holds the X11 display. The endpoint is not republished in the meantime.
Run the retry again once that group has finished exiting.

Each start uses a fresh, private notification socket and only a `READY=1`
message from the Satellite process that was actually spawned is accepted, so a
stale or unrelated notification cannot be mistaken for readiness.

### Limitations

- X11 applications are ordinary Wayland windows as far as layout, focus, and
  window rules are concerned. Rules match the application identity Satellite
  reports, so they are configured the same way as for native clients.
- Native Wayland preferences are kept as they are. Ferese does not force GTK,
  Qt, Firefox, or Electron onto X11 to demonstrate support.
- Portal dialogs requested with an `x11:` parent are shown **unparented**. The
  chooser or consent prompt still opens and authorization is unchanged; Ferese
  does not yet translate an X11 window ID into a Wayland export, and inventing a
  parent would be a security problem.
- Per-monitor fractional scaling of X11 applications depends on the selected
  Satellite build and its Xsettings policy. Text sharpness, pointer coordinates,
  and popup placement should be checked on each output profile rather than
  assumed.
- Drag and drop between native and X11 applications depends on the Satellite
  build and is not part of the initial support claim.
- Sandboxed applications may need explicit permission to reach the display. A
  flatpak with only `fallback-x11` gets no X11 socket inside a Wayland session and
  cannot read the private `XAUTHORITY` file under `$XDG_RUNTIME_DIR`, so it exits
  without a window while the service itself reports healthy. See
  [Sandboxed applications](installation.md#sandboxed-applications).

## Power and logout

Power actions use a centered confirmation. **Super+Shift+E** and
`feresectl request-logout` ask to log out. `feresectl exit` ends the session
immediately.
