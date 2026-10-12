# Layout

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
