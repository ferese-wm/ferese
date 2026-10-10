use cosmic::Element;
use cosmic::iced::{Color, Length};
use cosmic::widget::icon as svg_icon;
pub use ferese_theme::Palette;
pub use ferese_theme::controls::{button_style, navigation_style, settings_input as input_style, surface};

use crate::Message;
use crate::schema::Page;
use crate::store::{Edit, Snapshot, set};

/// Keep focus distinct from selection and hover for keyboard navigation.
pub fn panel_button(p: Palette, selected: bool) -> cosmic::theme::Button {
    use cosmic::iced::border::Shape;
    let paint = move |hover: bool, focused: bool, enabled: bool| {
        let highlighted = hover || focused;
        let background = if selected && enabled {
            ferese_theme::mix(p.card, p.accent, if highlighted { 0.28 } else { 0.16 })
        } else if highlighted && enabled {
            ferese_theme::mix(p.card, p.text, 0.09)
        } else {
            p.card
        };
        cosmic::widget::button::Style {
            shape: Some(Shape::Continuous),
            background: Some(background.into()),
            text_color: Some(if enabled {
                ferese_theme::foreground(background, p.text)
            } else {
                p.muted
            }),
            icon_color: Some(if enabled { p.text } else { p.muted }),
            border_radius: 8.into(),
            border_width: if focused {
                2.
            } else if selected {
                1.
            } else {
                0.
            },
            border_color: p.accent.scale_alpha(if focused { 1. } else { 0.45 }),
            outline_width: 0.,
            outline: None,
            ..Default::default()
        }
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |focused, _| paint(false, focused, true)),
        hovered: Box::new(move |focused, _| paint(true, focused, true)),
        pressed: Box::new(move |focused, _| paint(true, focused, true)),
        // A selected segment can be inert without losing its selection indicator.
        disabled: Box::new(move |_| paint(false, false, selected)),
    }
}

pub fn panel_select<'a>(
    content: impl Into<Element<'a, Message>>,
    p: Palette,
) -> cosmic::widget::Container<'a, Message, cosmic::Theme> {
    cosmic::widget::container(content).class(surface(ferese_theme::mix(p.card, p.text, 0.035), 7.))
}

pub fn color(value: &str, fallback: Color) -> Color {
    ferese_theme::parse_color(value).unwrap_or(fallback)
}

fn hex(c: Color) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (c.r * 255.) as u8,
        (c.g * 255.) as u8,
        (c.b * 255.) as u8
    )
}

pub fn native_theme(_snapshot: Option<&Snapshot>) -> cosmic::Theme {
    Palette::from_resolved(&ferese_theme_client::service::current().presented).native_theme()
}

pub fn icon(page: Page, tint: Color) -> svg_icon::Icon {
    ferese_theme::icons::outline(page.icon(), tint, 19)
}

pub fn brand_icon() -> svg_icon::Icon {
    cosmic::widget::icon::from_svg_bytes(ferese_theme::icons::APPLICATION)
        .symbolic(false)
        .icon()
        .size(32)
        .content_fit(cosmic::iced::ContentFit::Contain)
}

pub fn action_icon(path: &str, tint: Color) -> svg_icon::Icon {
    ferese_theme::icons::outline(path, tint, 16)
}

#[cfg(test)]
pub use ferese_config::presets::PRESETS;

pub fn split(snapshot: &Snapshot) -> bool {
    snapshot
        .item("theme.split")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| {
            ferese_config::families::family_id(&snapshot.string("theme.light.preset", "ferese-blue-light"))
                != ferese_config::families::family_id(&snapshot.string("theme.dark.preset", "ferese-blue"))
        })
}

pub fn family_selection(snapshot: &Snapshot, appearance: Option<ferese_config::theme::Appearance>) -> String {
    let Some(appearance) = appearance else {
        return snapshot.string(
            "theme.family",
            ferese_config::families::family_id(&snapshot.string("theme.dark.preset", "ferese-blue")),
        );
    };
    let kind = if appearance == ferese_config::theme::Appearance::Light {
        "light"
    } else {
        "dark"
    };
    let preset = snapshot.string(
        &format!("theme.{kind}.preset"),
        if kind == "light" {
            "ferese-blue-light"
        } else {
            "ferese-blue"
        },
    );
    snapshot.string(
        &format!("theme.{kind}.family"),
        ferese_config::families::family_id(&preset),
    )
}

pub fn choose_family(id: &str, appearance: Option<ferese_config::theme::Appearance>) -> Vec<Edit> {
    let kinds: &[&str] = match appearance {
        Some(ferese_config::theme::Appearance::Light) => &["light"],
        Some(ferese_config::theme::Appearance::Dark) => &["dark"],
        None => &["light", "dark"],
    };
    let mut edits = vec![set(
        &appearance.map_or_else(
            || "theme.family".into(),
            |a| {
                format!(
                    "theme.{}.family",
                    if a == ferese_config::theme::Appearance::Light {
                        "light"
                    } else {
                        "dark"
                    }
                )
            },
        ),
        id,
    )];
    for kind in kinds {
        for token in ["colors", "surface", "border", "focus_ring"] {
            edits.push(Edit::Unset(format!("theme.{kind}.{token}")));
        }
    }
    edits
}

pub fn toggle_split(snapshot: &Snapshot, enabled: bool, active: ferese_config::theme::Appearance) -> Vec<Edit> {
    let mut edits = vec![];
    if enabled {
        let id = family_selection(snapshot, None);
        for kind in ["light", "dark"] {
            if snapshot.item(&format!("theme.{kind}.family")).is_none() {
                let selection = if snapshot.item("theme.family").is_some() {
                    id.clone()
                } else {
                    family_selection(
                        snapshot,
                        Some(if kind == "light" {
                            ferese_config::theme::Appearance::Light
                        } else {
                            ferese_config::theme::Appearance::Dark
                        }),
                    )
                };
                edits.push(set(&format!("theme.{kind}.family"), selection));
            }
        }
    } else {
        edits.push(set("theme.family", family_selection(snapshot, Some(active))));
    }
    edits.push(set("theme.split", enabled));
    edits
}

pub fn paint_style_overrides(snapshot: &Snapshot, edit: &Edit) -> Vec<Edit> {
    let Edit::Set(path, _) = edit else { return vec![] };
    let section = match path.as_str() {
        "theme.focus_ring.style" => "focus_ring",
        "theme.border.style" => "border",
        _ => return vec![],
    };
    ["light", "dark"]
        .into_iter()
        .map(|kind| format!("theme.{kind}.{section}.style"))
        .filter(|path| snapshot.item(path).is_some())
        .map(Edit::Unset)
        .collect()
}

#[cfg(test)]
pub fn preset(index: usize) -> Vec<Edit> {
    let Some(preset) = PRESETS.get(index) else {
        return vec![];
    };
    let kind = if preset.appearance == ferese_config::theme::Appearance::Light {
        "light"
    } else {
        "dark"
    };
    let mut edits = vec![set(&format!("theme.{kind}.preset"), preset.id)];
    for token in ["colors", "surface", "border", "focus_ring"] {
        edits.push(Edit::Unset(format!("theme.{kind}.{token}")));
    }
    edits
}

#[cfg(test)]
pub fn preset_selected(snapshot: &Snapshot, index: usize) -> bool {
    let Some(preset) = PRESETS.get(index) else { return false };
    let kind = if preset.appearance == ferese_config::theme::Appearance::Light {
        "light"
    } else {
        "dark"
    };
    snapshot.string(
        &format!("theme.{kind}.preset"),
        if kind == "dark" {
            "ferese-blue"
        } else {
            "ferese-blue-light"
        },
    ) == preset.id
        && ["colors", "surface", "border", "focus_ring"]
            .iter()
            .all(|token| snapshot.item(&format!("theme.{kind}.{token}")).is_none())
}

pub fn preview_resolved<'a>(snapshot: &Snapshot, theme: &ferese_config::theme::ResolvedTheme) -> Element<'a, Message> {
    let mut snapshot = snapshot.clone();
    snapshot.doc = snapshot.doc.with_theme(theme);
    preview(&snapshot, theme)
}

fn preview(snapshot: &Snapshot, theme: &ferese_config::theme::ResolvedTheme) -> Element<'static, Message> {
    let p = Palette::from_resolved(theme);
    let base = hex(p.sidebar);
    let card = hex(p.card);
    let accent = hex(p.accent);
    let muted = hex(p.muted);
    let gap = snapshot.number("layout.inner_gap", 8.).clamp(0., 32.);
    let radius = snapshot.number("theme.geometry.window_radius", 14.).clamp(0., 28.);
    let bar_y = snapshot.number("theme.geometry.top_bar_margin_top", 0.) * 0.5 + 12.;
    let bar_margin = snapshot.number("theme.geometry.top_bar_margin_horizontal", 0.) * 0.5 + 14.;
    let bar_height = snapshot.number("theme.geometry.top_bar_height", 30.) * 0.65;
    let bar_radius = snapshot.number("theme.geometry.shell_radius", 14.) * 0.65;
    let opacity = if snapshot.string("theme.material.style", "solid") == "translucent" {
        snapshot
            .number("theme.material.opacity", ferese_config::DEFAULT_MATERIAL_OPACITY)
            .clamp(0., 1.)
    } else {
        1.
    };
    let right = 340. + gap / 2.;
    let left_width = 288. - gap / 2.;
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="680" height="210" viewBox="0 0 680 210">
      <defs><linearGradient id="wall" x2="1" y2="1"><stop stop-color="{accent}" stop-opacity=".20"/><stop offset="1" stop-color="{base}"/></linearGradient></defs>
      <rect width="680" height="210" rx="14" fill="{base}"/><rect width="680" height="210" rx="14" fill="url(#wall)"/>
      <path d="M0 180Q160 30 350 145T680 100V210H0Z" fill="{accent}" opacity=".055"/>
      <rect x="{bar_margin}" y="{bar_y}" width="{}" height="{bar_height}" rx="{bar_radius}" fill="{base}" opacity="{opacity}"/>
      <rect x="30" y="{}" width="12" height="5" rx="2.5" fill="{accent}"/><circle cx="51" cy="{}" r="2.5" fill="{muted}"/>
      <path d="M595 {}h10m10 0h10m10 0h10" stroke="{muted}" stroke-width="3" stroke-linecap="round"/>
      <rect x="44" y="60" width="{left_width}" height="132" rx="{radius}" fill="{card}" stroke="{accent}" stroke-width="1.2"/>
      <rect x="{right}" y="60" width="{left_width}" height="132" rx="{radius}" fill="{card}"/>
      <path d="M64 80h65M64 100h170M64 115h120M64 130h152" stroke="{muted}" opacity=".45" stroke-width="4" stroke-linecap="round"/>
      <rect x="{}" y="78" width="50" height="94" rx="5" fill="{base}"/>
      <path d="M420 82h85M420 103h175M420 121h150M420 139h160" stroke="{muted}" opacity=".32" stroke-width="4" stroke-linecap="round"/>
    </svg>"##,
        680. - 2. * bar_margin,
        bar_y + bar_height / 2. - 2.5,
        bar_y + bar_height / 2.,
        bar_y + bar_height / 2.,
        right + 16.
    );

    svg_icon::from_svg_bytes(svg.into_bytes())
        .symbolic(false)
        .icon()
        .width(Length::Fill)
        .height(Length::Fixed(132.))
        .content_fit(cosmic::iced::ContentFit::Contain)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_preset(index: usize) -> Snapshot {
        let mut snapshot = Snapshot::parse(String::new()).unwrap();
        for edit in preset(index) {
            snapshot.edit(&edit).unwrap();
        }
        snapshot
    }

    #[test]
    fn shared_style_controls_replace_scoped_styles_without_deleting_endpoints() {
        let mut snapshot = Snapshot::parse(
            r##"theme {
            family catppuccin
            light { focus-ring { style solid; gradient { from "#123456"; to "#654321"; }; }; }
            dark { focus-ring { style solid; gradient { from "#ABCDEF"; to "#FEDCBA"; }; }; }
        }"##
            .into(),
        )
        .unwrap();
        for (style, enabled) in [("auto", true), ("solid", false), ("auto", true)] {
            let edit = set("theme.focus_ring.style", style);
            for unset in paint_style_overrides(&snapshot, &edit) {
                snapshot.edit(&unset).unwrap();
            }

            snapshot.edit(&edit).unwrap();
            for (mode, from) in [("light", "#123456"), ("dark", "#ABCDEF")] {
                snapshot.edit(&set("theme.mode", mode)).unwrap();
                let theme = ferese_config::theme::resolve(
                    &snapshot.doc,
                    std::path::Path::new("/config"),
                    "2026-09-30T12:00:00Z".parse().unwrap(),
                    |_| unreachable!(),
                )
                .unwrap()
                .theme;
                assert_eq!(theme.tokens.focus_ring.gradient.is_some(), enabled);
                if enabled {
                    assert_eq!(theme.tokens.focus_ring.gradient.unwrap().from, from);
                }

                assert!(snapshot.item(&format!("theme.{mode}.focus_ring.gradient")).is_some());
            }
        }
    }

    fn contrast(a: Color, b: Color) -> f32 {
        ferese_theme::contrast(a, b)
    }

    #[test]
    fn galleries_follow_mode_and_keep_selection_independent() {
        let mut snapshot = Snapshot::parse(String::from(r#"theme { mode "auto"; }"#)).unwrap();
        for index in 0..PRESETS.len() {
            for edit in preset(index) {
                snapshot.edit(&edit).unwrap();
            }
            assert_eq!(snapshot.string("theme.mode", ""), "auto");
        }
    }

    #[test]
    fn split_toggle_restores_the_pair_and_uses_the_active_family_when_linked() {
        use ferese_config::theme::Appearance::{Dark, Light};
        let mut snapshot = Snapshot::parse(r#"theme { family "ferese-blue"; split #true; light { family "gruvbox"; }; dark { family "catppuccin"; }; }"#.into()).unwrap();
        for edit in toggle_split(&snapshot, false, Light) {
            snapshot.edit(&edit).unwrap();
        }
        assert!(!split(&snapshot));
        assert_eq!(family_selection(&snapshot, None), "gruvbox");
        for edit in choose_family("everforest", None) {
            snapshot.edit(&edit).unwrap();
        }
        for edit in toggle_split(&snapshot, true, Dark) {
            snapshot.edit(&edit).unwrap();
        }
        assert!(split(&snapshot));
        assert_eq!(family_selection(&snapshot, Some(Light)), "gruvbox");
        assert_eq!(family_selection(&snapshot, Some(Dark)), "catppuccin");
    }

    #[test]
    fn shared_family_selection_clears_stale_scoped_gradients() {
        let source = r##"theme {
            family "ferese-blue"
            border { style solid; }
            light { family "gruvbox"; focus-ring { gradient { from "#3D7BE6"; to "#3D7BE6"; }; }; }
            dark {
                family "catppuccin"
                colors { accent "#3D7BE6"; }
                focus-ring { gradient { from "#3D7BE6"; to "#3D7BE6"; }; }
                background { path "/wallpaper.png"; }
            }
        }"##;
        for (id, _, _, _) in ferese_config::families::BUILTINS {
            let mut snapshot = Snapshot::parse(source.into()).unwrap();
            for edit in choose_family(id, None) {
                snapshot.edit(&edit).unwrap();
            }

            assert_eq!(
                family_selection(&snapshot, Some(ferese_config::theme::Appearance::Light)),
                "gruvbox"
            );
            assert_eq!(
                family_selection(&snapshot, Some(ferese_config::theme::Appearance::Dark)),
                "catppuccin"
            );
            assert_eq!(snapshot.string("theme.dark.background.path", ""), "/wallpaper.png");
            assert_eq!(snapshot.string("theme.border.style", ""), "solid");
            for mode in ["light", "dark"] {
                assert!(snapshot.item(&format!("theme.{mode}.focus_ring")).is_none());
                snapshot.edit(&set("theme.mode", mode)).unwrap();
                let reference = Snapshot::parse(format!(
                    "theme {{ family \"{id}\"; mode \"{mode}\"; border {{ style solid; }}; }}"
                ))
                .unwrap();
                let resolve = |snapshot: &Snapshot| {
                    ferese_config::theme::resolve(
                        &snapshot.doc,
                        std::path::Path::new("/config"),
                        "2026-10-04T12:00:00Z".parse().unwrap(),
                        |_| unreachable!(),
                    )
                    .unwrap()
                    .theme
                };
                let actual = resolve(&snapshot);
                let expected = resolve(&reference);
                assert_eq!(actual.tokens.focus_ring, expected.tokens.focus_ring, "{id}/{mode}");
                assert_eq!(
                    Palette::from_resolved(&actual).accent_gradient,
                    Palette::from_resolved(&expected).accent_gradient
                );
            }
        }
    }

    #[test]
    fn the_first_split_starts_with_both_variants_of_the_single_family() {
        use ferese_config::theme::Appearance::{Dark, Light};
        let mut snapshot = Snapshot::parse(r#"theme { family "tokyo-night"; }"#.into()).unwrap();
        for edit in toggle_split(&snapshot, true, Dark) {
            snapshot.edit(&edit).unwrap();
        }
        assert_eq!(family_selection(&snapshot, Some(Light)), "tokyo-night");
        assert_eq!(family_selection(&snapshot, Some(Dark)), "tokyo-night");
    }

    #[test]
    fn presets_are_distinct_and_readable_on_settings_surfaces() {
        for (index, preset) in PRESETS.iter().enumerate() {
            let tokens = ferese_config::theme::preset(preset.id, preset.appearance).unwrap();
            let theme = ferese_config::theme::ResolvedTheme {
                appearance: preset.appearance,
                tokens,
                ..ferese_config::theme::default_theme()
            };
            let palette = Palette::from_resolved(&theme);
            for background in [palette.background, palette.sidebar, palette.card] {
                assert!(contrast(palette.text, background) >= 4.5, "{} text", preset.name);
                assert!(contrast(palette.muted, background) >= 4.5, "{} muted text", preset.name);
            }
            for other in PRESETS.iter().skip(index + 1) {
                if preset.appearance == other.appearance {
                    assert_ne!(preset.accent, other.accent);
                }
                assert_ne!(preset.base, other.base);
            }
        }
    }

    #[test]
    fn selection_tracks_all_preset_colors_and_gradient_changes() {
        for (index, preset) in PRESETS.iter().enumerate() {
            let mut snapshot = configured_preset(index);
            assert!(preset_selected(&snapshot, index));
            let kind = if preset.appearance == ferese_config::theme::Appearance::Light {
                "light"
            } else {
                "dark"
            };
            snapshot
                .edit(&set(&format!("theme.{kind}.surface.bar.text_primary"), "#123456"))
                .unwrap();
            assert!(!preset_selected(&snapshot, index));
            let mut snapshot = configured_preset(index);
            snapshot
                .edit(&set(&format!("theme.{kind}.focus_ring.gradient.to"), "#123456"))
                .unwrap();
            assert!(!preset_selected(&snapshot, index));
        }
        assert!(preset_selected(&Snapshot::parse(String::new()).unwrap(), 0));
        assert!(preset(usize::MAX).is_empty());
        assert!(!preset_selected(&Snapshot::parse(String::new()).unwrap(), usize::MAX));
    }

    #[test]
    fn native_controls_follow_lightness() {
        for (index, item) in PRESETS.iter().enumerate() {
            let _ = index;
            let resolved = ferese_config::theme::ResolvedTheme {
                appearance: item.appearance,
                tokens: ferese_config::theme::preset(item.id, item.appearance).unwrap(),
                ..ferese_config::theme::default_theme()
            };
            assert_eq!(
                Palette::from_resolved(&resolved).native_theme().cosmic().is_dark,
                item.appearance == ferese_config::theme::Appearance::Dark
            );
        }
    }
}
