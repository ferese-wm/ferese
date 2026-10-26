# Panel composition

Ferese uses one panel configuration across all outputs, with each output laying
out its own workspace controls and focused-window title. Open **Settings →
Panel** to choose a style and position, then **Edit panel** to arrange
items in Start, Center and End groups. The composition preview stays visible
while you edit, and changes save automatically.

Choose style, opacity and corners for the whole panel; groups control layout and
spacing. The available styles are Floating, Capsules, Notch, Edge to edge,
Fused islands, Adaptive and Morph. Capsules and Fused islands join neighboring
**groups within a zone**, so a zone with one group remains a single island.
Adaptive follows window state independently on each output, while Morph combines
the panel into a centered hub with expandable workspace, media and status views.

Most panels default to 36 logical pixels high, Morph to 34, and detached styles
use 8-pixel margins unless your configuration overrides them. The packaged
configuration includes its own geometry overrides. For the complete options and
examples, see the [panel configuration guide](../panel-configuration.md).

Add a **Night light** item in the panel editor for an on/off switch and warmth
slider. In Morph, the button stays beside the tabs when the panel is expanded,
even if you remove the item. See [color temperature](outputs.md#color-temperature)
for the saved settings and display requirements.

```kdl
panel "main" {
    edge "top"
    style "floating"
    geometry { height 36; edge-margin 8; window-clearance 4; }
    start {
        group "navigation" {
            item "overview" kind="overview" overflow="never"
            item "workspaces" kind="workspaces" style="dots" overflow="never"
        }
    }
    center {
        group "title" {
            item "title" kind="focused-window" overflow="never"
        }
    }
    end {
        group "status" {
            item "quick-settings" kind="quick-settings"
            item "battery" kind="battery" percentage=#true
            item "clock" kind="clock" representation="compact"
        }
    }
}
```

If you omit the panel block, Ferese uses its default composition; if you write
one, its groups and items define the contents, and empty zones stay empty. Only
one panel block is accepted, and group and item IDs must be unique within it.
Use `edge "bottom"` to move the same arrangement to the bottom, with popups
opening toward the desktop and edge-relative corners mirroring vertically.
