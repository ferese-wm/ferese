# Window rules

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
| `idle-inhibit` | `none`, `visible`, `fullscreen`, `playing`, `fullscreen-playing` | Automatic fullscreen playback; see [Idle inhibition](idle-inhibition.md#idle-inhibition) |

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
placement survives resolution changes. New independent windows open on the
focused output; saved geometry applies only when it belongs to that output and
at least 25% of the window would be visible. A worker saves the latest snapshot,
combining pending updates. Completing a drag updates memory
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
