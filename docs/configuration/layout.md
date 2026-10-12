# Layout

| `layout` key | Type / values | Default | Meaning |
| --- | --- | --- | --- |
| `mode` | `"scrolling"`, `"tree"` | `"scrolling"` | Workspace layout |
| `inner-gap` | number ≥ 0 | `10` | Between windows |
| `outer-gap` | number ≥ 0 | `4` | Around workspace edges |
| `smart-gaps` | boolean | `false` | Remove outer gaps for one tiled window |

| `scrolling` key | Type / values | Default | Meaning |
| --- | --- | --- | --- |
| `default-column-display` | `"stacked"`, `"tabbed"` | `"stacked"` | Display mode of new columns; reload preserves existing column choices |
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

## Tabbed columns

To open new columns as tabs:

```kdl
scrolling {
    default-column-display "tabbed"
}
```

Your existing columns stay as they are. Press **Super + Shift + T** to switch
the focused column.

See [Tabbed windows](../tabbed-windows.md) for grouping, tab selection and the default shortcuts.

## Scrolling tab indicator

| `scrolling.tab-indicator` key | Type / range | Default |
| --- | --- | --- |
| `width` | number, 1–16 | `4` |
| `segment-height` | number, `width`–128 | `24` |
| `segment-gap` | number, 0–64 | `4` |
| `hit-width` | number, `width`–64 | `12` |
| `gap-threshold` | number, `width`–128 | `8` |
| `list-width` | number, 80–640 | `220` |
| `row-height` | number, (`font-size` + 6)–80 | `28` |
| `padding` | number, 0–24 | `4` |
| `font-size` | number, 8–32 | `12` |
| `outline-width` | number, 0–8 | `0.5` |
| `flash-duration-ms` | integer, 0–60000 | `1000` |
| `active-color` | optional color | Theme accent |
| `inactive-color` | optional color | Theme border |
| `background-color` | optional color | Theme raised surface |
| `text-color` | optional color | Theme primary text |
| `outline-color` | optional color | Theme border |

Configure the scrolling indicator under `scrolling.tab-indicator`:

```kdl
scrolling {
    tab-indicator {
        width 4
        segment-height 24
        segment-gap 4
        hit-width 12
        gap-threshold 8
        list-width 220
        row-height 28
        padding 4
        font-size 12
        outline-width 0.5
        flash-duration-ms 1000
    }
}
```

Sizes are logical pixels. The hover list fits its longest title, up to
`list-width`, with an 80px minimum. Its width stays fixed while open.
`outline-width 0` hides the outline; positive widths draw at least one physical
pixel. `flash-duration-ms 0` disables keyboard feedback.
Leave colors unset to use your theme, or override them with `#RRGGBB` or
`#RRGGBBAA` values.
The selected row has a faint highlight using the text color. The rail has
fully rounded ends; the title list follows the shell corner radius.
The hover list and keyboard feedback pill follow the shell material opacity.
Solid material or Reduce transparency makes their backgrounds opaque. Text and
rail markers retain their own opacity. These controls use flat fills without
blur or shadows. Changes apply on configuration reload.
