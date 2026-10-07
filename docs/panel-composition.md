# Panel composition

Reviewed revision: `d4c8c10` (2026-10-07).

The integration covers composition, popup ownership, measured sizing, group
surfaces, overflow, KDL persistence, and a Settings editor with a composition
preview and drag/drop editor. Ferese still creates one top panel per output.
Multiple panels, output overrides, and external providers remain separate work.

## What changed

The previous bar assembled a fixed left/center/right sequence, estimated title
space by subtracting 920 pixels plus a media allowance, and scrolled its sides
when controls did not fit. Popup parent selection lived in a mutable global
`bar_surface_id`.

Arrangement is now data: a panel contains start, center, and end zones; zones
contain ordered groups; groups contain item instances. IDs belong to definitions,
while each output retains its existing Wayland surface, material binding,
fullscreen visibility, and measured presentation state. Two items can have the
same kind and distinct IDs.

Persistent types and validation live in `ferese-config::panel`, shared by the
compositor, shell, and Settings. The shell translates existing status settings
when no panel is authored. An explicit panel replaces that generated composition.
The current renderer accepts exactly one definition and instantiates it on each
output. Unsupported edge/output fields and multiple definitions are rejected.

Built-in items reuse the existing shared status/media snapshots and actions.
Creating another instance does not start another service, timer, or D-Bus
connection. `Menu` remains the popup content route; it no longer identifies the
item or its parent surface.

## Sizing and overflow

The shell measures each supported representation with Iced using the current
font, labels, and control geometry. It measures preferred content before imposing
the panel's available width. Group gaps and decoration are counted separately.
The pure resolver contains no widgets, clocks, services, or Wayland resources.

The center stays physically centered, with space reserved against the larger
side. Flexible title/workspace content yields space first, then controls use
smaller supported representations. Lower-priority items yield before higher
priorities; equal priorities follow definition order. Eligible items move to
overflow when representations cannot fit. Each resolution starts from configured
intent, so controls return to their preferred form as space returns.

`never` prevents overflow while mandatory controls fit. If the surface is too
small even for them, those items move to overflow too. The measured chevron is
placed before the end controls. At widths smaller than the chevron's decoration,
its drawing is clipped to the available surface; no layout can preserve a full
control there.

Overflow shows displaced instances in definition order and uses their normal
actions. Its list scrolls vertically. Workspace controls may scroll within their
own allocation, including in overflow; the panel itself no longer scrolls.
Only media, clock, and battery currently have multiple representations. Other
built-ins retain their existing control form.

## Popup and presentation ownership

`OpenMenu` stores a `PopoverAnchor` containing the parent surface, panel ID,
optional item ID, and rectangle. Same-parent content changes reuse the existing
popup and material binding. A parent change destroys the previous popup before
creating another, because Wayland repositioning cannot change its parent.
Repeated clicks toggle only the same instance on the same surface.

Displaced items anchor their popup at the overflow chevron rather than at
coordinates inside the overflow surface. Toast requests use the focused output's
bar as an explicit notification fallback. Output removal and fullscreen hiding
close the popup belonging to that surface. Composition reload closes the current
popup and discards cached allocation; invalid config keeps the previous config.

Material bounds and input bounds are collected separately. Decorated islands
contribute material/input bounds. Undecorated controls contribute input bounds
only. Both are clipped through scrollable viewports; gaps between islands remain
click-through. The existing surface-effects and layer-shell input-region
protocols express this without a protocol change.

Existing popup motion, theme materials, fullscreen policy, layer reservations,
service updates, and command handling remain in use. Panel items do not introduce
another animation engine or popup lifecycle.

## Persistence and Settings

KDL stores IDs positionally in `panel`, `group`, and `item` records. Zones and
record order determine placement. Portable validation checks IDs, duplicate
instances, spacing, padding, and priority before accepted config is published.
Settings uses its existing backup, validation, reload, and undo path.

Settings → Panel & Shell → Items → Edit panel starts from the current generated panel.
It then exposes item visibility, overflow policy, supported representation
preferences, title visibility, and battery percentage. Select an item to open its
inspector. Items can be added, removed, reordered within a group, or moved to another
existing group. Editing resolves IDs against the latest draft, validates the whole
change, and saves once. Unknown item fields and comments are retained.

Panels contains size and margin settings; Items contains the Start/Center/End
editor and a separate selected-item/group inspector. Appearance contains the
background policy and links to group surfaces. The preview stays above the
scrolling content. Tab changes do not save config. The current top panel is the
only panel listed; unsupported creation, edge selection, display targeting, and
visibility policies are not exposed.

Each zone can gain new groups. Select a group to change its zone, surface,
spacing, or padding; inspector buttons reorder groups within their zone. Removing a
group requires moving or removing its items first. Group moves preserve item IDs
and authored fields. Queued item placement follows the destination group's ID,
including after that group moves between zones.

The editor uses wrapping zone/group cards and item drag handles. Pointer motion
updates only a local drop hint. Release resolves the item, destination group, and
optional next-item ID against the current draft, validates the complete move,
and sends it through the existing save/undo path. Dropping into an empty zone
creates a group and moves the item in one save. Stale targets reject the whole
edit. Escape, focus loss, and release outside valid targets cancel without saving.
Group boundaries remain visible; no title or item-kind matching is used.

The preview updates from the draft and uses the same measured allocation widget,
representation preferences, overflow decoration, and pure resolver as the shell.
The widget now lives in `ferese-theme::panel`; it owns measurement and allocation,
not services or popup lifecycle. Settings supplies sample labels and icons, with
all services marked available. Preview clicks select items for editing; its
overflow chevron reveals displaced instances. Redraws do not save configuration.
Selecting a generated item starts customization from the current status settings.

This is a composition preview, not a pixel-for-pixel rendering of the running
bar. Service state, control content, and vertical styling differ. The editor lives
inside Settings; it does not create a separate desktop editing surface. This
Settings/editor pass leaves the running panel renderer unchanged.
A live nested preview exposed date and spacing differences
from the previous bar. Those remain for a separate visual compatibility pass.

See [Panel composition in the configuration reference](configuration.md#panel-composition)
for syntax and defaults. [Issue #2](https://github.com/ferese-wm/ferese/issues/2)
requests status overflow and visible-icon selection; this integration supplies
those controls through the shared item policy rather than a status-only list.

## Follow-up work

Multiple panels need surface ownership keyed by
panel and output, plus reservations, hotplug cleanup, fullscreen policy, and
popup placement for every supported edge. Output selectors should reuse the
existing output-profile matcher.

Providers need bounded framing, cancellation, process lifetime, and action
validation before becoming user-facing. Process isolation alone does not remove
the user's ambient permissions. A full external applet protocol needs a separate
review.

[Waybar's manual](https://github.com/Alexays/Waybar/blob/master/man/waybar.5.scd.in)
and [Plasma's scripting API](https://develop.kde.org/docs/plasma/scripting/api/)
provided examples of ordered composition and item instances. They do not establish
performance or usability comparisons for this implementation.

## Verification

Tests cover default composition and availability, duplicate kinds, KDL edits and
round-trips, empty groups, invalid IDs, config acceptance, stable item editing,
queued item/group moves and saves, group removal without item loss, inspector
selection, drag cancellation and release, empty-zone drops, queued drop saves,
wrapping editor cards, a fixed preview during scrolling, preview adaptation and
exact item selection, deterministic allocation,
priority, resizing, widget measurement, input delivery, changed-state coalescing,
undecorated input regions, scroll clipping, and popup anchor identity/removal.
Widget tests use a headless Iced renderer. They do not replace live multi-output,
font/scale, or keyboard interaction testing.

```sh
cargo test --locked --workspace --no-fail-fast
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all --check
git diff --check
```
