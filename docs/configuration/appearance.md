# Appearance

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
| `appearance.focus-effect.enabled` | boolean | `true` | Apply window opacity and dimming; disabling preserves the saved settings |
| `appearance.focus-effect.active-opacity` | number 0–1 | `1` | Active application opacity, including fullscreen |
| `appearance.focus-effect.inactive-opacity` | number 0–1 | `1` | Other application window opacity |
| `appearance.focus-effect.blur` | boolean | `true` | Blur behind translucent application windows using the shared blur radius |
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

**Window blur** in Settings controls blur behind both active and inactive windows.
It uses `theme.material.blur-radius` independently of the shell material style.
Fully opaque or invisible windows skip blur. Reduced transparency disables it.

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
| `surface-raised` | `"#1E2530"` | Raised surfaces, including the Morph hub |
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
| `shell-radius` | number ≥ 0 | `14` | Shell surfaces and controls, apart from panel shapes; fractional radii are preserved; 0 makes them square |
| `control-gap` | number ≥ 0 | `12` | Shared control spacing token; panel spacing belongs to its groups |

| Section / key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `theme.surface.bar.background` | color | `"#111821"` | Regular panel background color; Morph uses `surface-raised` |
| `theme.surface.bar.text-primary` | color | `"#F4F7FB"` | Panel text/icons |
| `theme.surface.bar.text-muted` | color | `"#8793A2"` | Inactive panel foreground |
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

In Settings → Appearance, **Accent style** controls the gradient used by focus
borders and selected shell controls. Settings → Windows contains **Inactive
border style** and **Focus border**. Accent style and inactive border style
offer **Gradient** or **Solid**. In KDL, `theme.focus-ring.style` and
`theme.border.style` accept `"auto"` (default) or `"solid"`. Solid disables the
gradient, including any explicit endpoints, without deleting them from the config.

Shell surface style, opacity and corner radius are in Settings → Panel & Shell.
**Background blur** in Appearance sets the blur radius for shell surfaces and windows.

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
