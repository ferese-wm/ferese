# Commands and bindings

See [Shortcuts and gestures](../shortcuts.md) for the full default keymap and mouse actions.

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
| `binding-mode` | Named mode, or `"default"` to leave it |
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

## Binding modes

Named modes provide a separate keyboard map for resizing, launching, or other
sets of actions. No modes are enabled by default. For example:

```kdl
binding "Super+Shift+r" "binding-mode" "resize"
binding "Super+Shift+Return" "binding-mode" "launch"
binding-mode "resize" {
    binding "h" "resize" "left"
    binding "j" "resize" "down"
    binding "k" "resize" "up"
    binding "l" "resize" "right"
    binding "Return" "binding-mode" "default"
}
binding-mode "launch" {
    binding "t" "spawn" "terminal"
    binding "r" "binding-mode" "resize"
    binding "Return" "binding-mode" "default"
}
```

Release the entry chord's modifiers before using an unmodified mode binding.
Modifiers keep their usual exact matching semantics; entering a mode never
changes the application's modifier state. Escape exits with any modifiers held
and cannot be overridden inside a mode. The bar shows the active name and Escape
hint, including over fullscreen windows, and restores its ordinary contents on
exit. Long names truncate to keep the exit hint visible. Mode changes reach the
shell through its existing snapshot protocol.

Entering a mode closes panel popovers and workspace tooltips. An expanded Morph
panel collapses and releases keyboard focus. Hover, activity updates, and queued
menu requests cannot reopen it while the mode indicator is displayed.
Live Activities continue updating their data, but leave the hidden Morph panel's
geometry unchanged until the mode exits.

Mode bindings replace the default keyboard map while active. Unmatched keys
reach the focused application; default compositor bindings and portal shortcuts
do not run as fallbacks. Gestures keep the default configuration. Normal shortcut
inhibition still applies, with Escape available to leave the mode. A mode remains
active across actions and focus/workspace changes until explicitly changed or
exited; launching an application does not implicitly leave it.

Each mode must have a unique name of 1–48 ASCII letters, digits, underscores, or
hyphens. `default` is reserved. Mode bindings accept the same actions and physical
key matching as ordinary bindings; swipes, disabled records, duplicate chords,
and Escape bindings are rejected, including physical keys remapped to Escape.
References to undefined modes reject the configuration before it is applied.

An accepted change to bindings, modes, commands, or input settings returns to
default and closes any focus preview. Unrelated reloads preserve the mode;
rejected reloads keep the previous configuration and mode. Locking, input capture,
and session suspension also exit.
The first press owns its matching release across these transitions. Held keys
are never reinterpreted by a new mode; consumed releases stay consumed. Repeated
presses retain their original action and repeat only while its chord still
matches. A mode/configuration transition stops that action's repeats.
`feresectl keybindings` lists the active keyboard map.

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

For a floating window, Super+Shift+H/J/K/L moves left/down/up/right by 32 logical
pixels. Super+Ctrl+H/L shrinks/grows its width; Super+Ctrl+K/J shrinks/grows its
height by the same step. Keyboard adjustments respect client size limits and
keep the window reachable in its output's work area. Fullscreen and maximized
floating windows ignore these actions.

Mouse moves and resizes snap to work-area edges and nearby visible window edges at
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
