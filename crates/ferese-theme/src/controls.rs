use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Background, Border, Color, Vector};
use cosmic::widget::{button, container};
use cosmic::{theme, widget};

use crate::{Palette, mix};

pub fn surface(background: Color, radius: f32) -> theme::Container<'static> {
    theme::Container::custom(move |_| surface_appearance(background, radius))
}

pub fn button_style(p: Palette, selected: bool) -> theme::Button {
    button_style_with_focus(p, selected, true)
}

pub fn button_style_with_focus(p: Palette, selected: bool, focus_visible: bool) -> theme::Button {
    styled_button(p, selected, false, 1.0, focus_visible)
}

/// Selection cards share native button states while revealing the modal material.
pub fn material_button_style(p: Palette, selected: bool, material_opacity: f32) -> theme::Button {
    material_button_style_with_focus(p, selected, material_opacity, true)
}

pub fn material_button_style_with_focus(
    p: Palette,
    selected: bool,
    material_opacity: f32,
    focus_visible: bool,
) -> theme::Button {
    let opacity = if material_opacity.is_finite() {
        material_opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };

    let fill = if opacity < 1.0 {
        opacity * if selected { 0.65 } else { 0.25 }
    } else {
        1.0
    };

    styled_button(p, selected, false, fill, focus_visible)
}

pub fn text_button<'a, M: Clone + 'a>(
    label: impl Into<std::borrow::Cow<'a, str>> + 'a,
    font: cosmic::font::Font,
    palette: Palette,
    selected: bool,
) -> button::Button<'a, M> {
    let theme = cosmic::theme::active();
    let native = theme.cosmic();
    let content = widget::row![
        crate::text(label, font)
            .size(14)
            .line_height(cosmic::iced::widget::text::LineHeight::Absolute(20.into())),
    ]
    .height(native.space_l())
    .padding([0, native.space_s()])
    .align_y(cosmic::iced::Alignment::Center);

    button::custom(content)
        .padding(0)
        .class(button_style(palette, selected))
}

pub fn switch<'a, M: Clone + 'a>(enabled: bool, palette: Palette) -> button::Button<'a, M> {
    let paint = move |focused| button::Style {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: None,
        text_color: Some(palette.text),
        icon_color: Some(palette.text),
        border_radius: 12.into(),
        border_width: if focused { 1. } else { 0. },
        border_color: palette.accent,
        ..Default::default()
    };
    button::custom(crate::menus::switch(enabled, palette, 1.))
        .padding(2)
        .class(theme::Button::Custom {
            active: Box::new(move |focused, _| paint(focused)),
            hovered: Box::new(move |focused, _| paint(focused)),
            pressed: Box::new(move |focused, _| paint(focused)),
            disabled: Box::new(move |_| paint(false)),
        })
}

pub(crate) fn switch_colors(palette: Palette, enabled: bool, hovered: bool) -> (Color, Color) {
    let track = if enabled {
        palette.accent
    } else {
        mix(palette.card, palette.text, if hovered { 0.18 } else { 0.12 })
    };
    let thumb = crate::foreground(track, palette.text);
    (track, thumb)
}

pub fn navigation_style(p: Palette, selected: bool) -> theme::Button {
    styled_button(p, selected, true, 1.0, true)
}

pub fn settings_input(p: Palette) -> theme::TextInput {
    let appearance = move |focused: bool, hovered: bool| cosmic::widget::text_input::Appearance {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: mix(p.card, p.text, if hovered { 0.06 } else { 0.035 }).into(),
        border_radius: p.radius.min(7.).into(),
        border_width: 1.,
        border_offset: None,
        border_color: if focused {
            p.accent
        } else {
            mix(p.card, p.text, if hovered { 0.26 } else { 0.18 })
        },
        icon_color: Some(p.muted),
        text_color: Some(p.text),
        placeholder_color: p.muted,
        selected_text_color: p.on_accent,
        selected_fill: p.accent,
        label_color: p.muted,
    };
    theme::TextInput::Custom {
        active: Box::new(move |_| appearance(false, false)),
        hovered: Box::new(move |_| appearance(false, true)),
        focused: Box::new(move |_| appearance(true, true)),
        error: Box::new(move |_| appearance(true, false)),
        disabled: Box::new(move |_| appearance(false, false)),
    }
}

/// Give transparent native selectors the same resting surface as other controls.
pub fn select<'a, M: 'a>(
    content: impl Into<cosmic::Element<'a, M>>,
    p: Palette,
) -> container::Container<'a, M, cosmic::Theme> {
    widget::container(content).class(theme::Container::custom(move |_| container::Style {
        background: Some(mix(p.card, p.text, 0.035).into()),
        border: Border {
            shape: BorderShape::Continuous,
            width: 1.,
            color: mix(p.card, p.text, 0.18),
            radius: p.radius.min(7.).into(),
            ..Default::default()
        },
        ..Default::default()
    }))
}

fn styled_button(p: Palette, selected: bool, navigation: bool, opacity: f32, focus_visible: bool) -> theme::Button {
    let style = move |hover: bool, focused: bool| {
        let background = if selected {
            mix(p.sidebar, p.accent, if hover { 0.24 } else { 0.17 })
        } else if hover {
            if navigation {
                mix(p.card, crate::surface_shade(p.sidebar), 0.06)
            } else {
                mix(p.card, p.text, 0.08)
            }
        } else if navigation {
            p.sidebar
        } else {
            mix(p.card, p.text, 0.035)
        };
        let (background, on) = if selected {
            crate::accent_pair(background, p.text)
        } else {
            (background, p.text)
        };

        let mut background = background;
        background.a *= if hover && opacity < 1.0 {
            (opacity + 0.12).min(1.0)
        } else {
            opacity
        };

        button::Style {
            shape: Some(BorderShape::Continuous),
            outline: None,
            background: Some(Background::Color(background)),
            text_color: Some(on),
            icon_color: Some(on),
            border_radius: p.radius.min(9.).into(),
            border_width: if selected || !navigation { 1. } else { 0. },
            border_color: if selected {
                p.accent
            } else if !navigation {
                mix(p.card, p.text, 0.18)
            } else {
                Color::TRANSPARENT
            },
            outline_width: if focused && focus_visible { 2. } else { 0. },
            outline_color: p.accent,
            overlay: None,
            shadow_offset: Vector::ZERO,
        }
    };

    theme::Button::Custom {
        active: Box::new(move |focused, _| style(false, focused)),
        hovered: Box::new(move |focused, _| style(true, focused)),
        pressed: Box::new(move |focused, _| style(true, focused)),
        disabled: Box::new(move |_| style(false, false)),
    }
}

/// Authentication actions use normal text while enabled and muted text while disabled.
/// Keep the disabled fill neutral so contrast correction cannot make it look active.
pub fn authentication_button(palette: Palette) -> theme::Button {
    let paint = move |enabled: bool, interaction: f32, focused: bool| {
        let (fill, foreground) = if enabled {
            let candidate = mix(palette.accent, palette.text, interaction);
            crate::accent_pair(crate::composite(candidate, palette.card), palette.text)
        } else {
            (crate::composite(palette.card, palette.background), palette.muted)
        };
        button::Style {
            shape: Some(BorderShape::Continuous),
            outline: None,
            background: Some(Background::Color(fill)),
            text_color: Some(foreground),
            icon_color: Some(foreground),
            border_radius: palette.radius.into(),
            outline_width: if focused { 1. } else { 0. },
            outline_color: foreground,
            ..Default::default()
        }
    };
    theme::Button::Custom {
        active: Box::new(move |focused, _| paint(true, 0., focused)),
        hovered: Box::new(move |focused, _| paint(true, 0.06, focused)),
        pressed: Box::new(move |focused, _| paint(true, 0.12, focused)),
        disabled: Box::new(move |_| paint(false, 0., false)),
    }
}

pub fn authentication_input(palette: Palette) -> theme::TextInput {
    let text = palette.text;
    let muted = palette.muted;
    let accent = palette.accent;
    let radius = palette.radius.min(10.);
    let appearance = move |focused: bool| widget::text_input::Appearance {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: Color::from_rgba(text.r, text.g, text.b, 0.045).into(),
        border_radius: radius.into(),
        border_width: 1.,
        border_offset: None,
        border_color: if focused {
            accent.scale_alpha(0.82)
        } else {
            muted.scale_alpha(0.26)
        },
        icon_color: Some(muted),
        text_color: Some(text),
        placeholder_color: muted,
        selected_text_color: text,
        selected_fill: accent.scale_alpha(0.35),
        label_color: text,
    };
    theme::TextInput::Custom {
        active: Box::new(move |_| appearance(false)),
        hovered: Box::new(move |_| appearance(true)),
        focused: Box::new(move |_| appearance(true)),
        error: Box::new(move |_| appearance(false)),
        disabled: Box::new(move |_| appearance(false)),
    }
}

pub fn lock_input(radius: f32, accent: cosmic::iced::Color, surface: cosmic::iced::Color) -> theme::TextInput {
    let appearance = move |focused: bool| widget::text_input::Appearance {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: cosmic::iced::Color::from_rgba(
            (surface.r + 1.) * 0.5,
            (surface.g + 1.) * 0.5,
            (surface.b + 1.) * 0.5,
            0.24,
        )
        .into(),
        border_radius: (radius * 2.).min(26.).into(),
        border_width: 1.,
        border_offset: None,
        border_color: if focused {
            accent.scale_alpha(0.7)
        } else {
            cosmic::iced::Color::WHITE.scale_alpha(0.15)
        },
        icon_color: Some(cosmic::iced::Color::WHITE.scale_alpha(0.8)),
        text_color: Some(cosmic::iced::Color::WHITE),
        placeholder_color: cosmic::iced::Color::WHITE.scale_alpha(0.55),
        selected_text_color: cosmic::iced::Color::WHITE,
        selected_fill: accent.scale_alpha(0.5),
        label_color: cosmic::iced::Color::WHITE,
    };

    theme::TextInput::Custom {
        active: Box::new(move |_| appearance(false)),
        hovered: Box::new(move |_| appearance(true)),
        focused: Box::new(move |_| appearance(true)),
        error: Box::new(move |_| appearance(false)),
        disabled: Box::new(move |_| appearance(false)),
    }
}

pub fn shell_button(
    foreground: Color,
    selected: bool,
    opacity: f32,
    radius: f32,
    progress: f32,
    pressed: bool,
) -> button::Style {
    button::Style {
        shape: Some(BorderShape::Continuous),
        outline: None,
        text_color: Some(foreground),
        icon_color: Some(foreground),
        border_radius: radius.into(),
        background: Some(Background::Color(Color {
            a: if pressed {
                0.20 * opacity
            } else {
                ((if selected { 0.14 } else { 0.0 }) + progress * if selected { 0.02 } else { 0.08 }) * opacity
            },
            ..foreground
        })),
        ..Default::default()
    }
}

pub fn surface_appearance(background: Color, radius: f32) -> container::Style {
    container::Style {
        background: Some(Background::Color(background)),
        border: Border {
            shape: BorderShape::Continuous,
            radius: radius.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn notification_button(foreground: Color, hover: Color, radius: f32, filled: bool) -> theme::Button {
    let style = move |active: bool| button::Style {
        shape: Some(BorderShape::Continuous),
        outline: None,
        text_color: Some(foreground),
        icon_color: Some(foreground),
        background: (active || filled).then_some(Background::Color(hover)),
        border_radius: radius.into(),
        ..Default::default()
    };
    theme::Button::Custom {
        active: Box::new(move |_, _| style(false)),
        hovered: Box::new(move |_, _| style(true)),
        pressed: Box::new(move |_, _| style(true)),
        disabled: Box::new(move |_| style(false)),
    }
}

pub fn filled_button(fill: Color, foreground: Color, radius: f32, opacity: f32) -> theme::Button {
    let paint = move |outline| button::Style {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: Some(Color { a: opacity, ..fill }.into()),
        text_color: Some(foreground),
        icon_color: Some(foreground),
        border_radius: radius.into(),
        outline_width: outline,
        outline_color: foreground,
        ..Default::default()
    };
    theme::Button::Custom {
        active: Box::new(move |focused, _| paint(if focused { 1. } else { 0. })),
        hovered: Box::new(move |_, _| paint(1.)),
        pressed: Box::new(move |_, _| paint(2.)),
        disabled: Box::new(move |_| paint(0.)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_modal_focus_preserves_button_borders_and_materials() {
        use cosmic::widget::button::Catalog;

        let palette = Palette::from_resolved(&ferese_config::theme::default_theme());
        let theme = palette.native_theme();

        for selected in [false, true] {
            for opacity in [0.65, 1.0] {
                let hidden = material_button_style_with_focus(palette, selected, opacity, false);
                let visible = material_button_style_with_focus(palette, selected, opacity, true);

                for (hidden, visible, resting) in [
                    (
                        theme.active(true, selected, &hidden),
                        theme.active(true, selected, &visible),
                        theme.active(false, selected, &visible),
                    ),
                    (
                        theme.hovered(true, selected, &hidden),
                        theme.hovered(true, selected, &visible),
                        theme.hovered(false, selected, &visible),
                    ),
                    (
                        theme.pressed(true, selected, &hidden),
                        theme.pressed(true, selected, &visible),
                        theme.pressed(false, selected, &visible),
                    ),
                ] {
                    assert_eq!(hidden.outline_width, 0.0);
                    assert_eq!(visible.outline_width, 2.0);
                    assert_eq!(resting.outline_width, 0.0);
                    assert_eq!(hidden.border_width, 1.0);
                    assert_eq!(hidden.border_width, visible.border_width);
                    assert_eq!(hidden.border_color, visible.border_color);
                    assert_eq!(hidden.border_radius, visible.border_radius);
                    assert_eq!(hidden.background, visible.background);
                    assert_eq!(hidden.text_color, visible.text_color);
                }
            }

            let hidden = button_style_with_focus(palette, selected, false);
            let visible = button_style_with_focus(palette, selected, true);
            assert_eq!(theme.active(true, selected, &hidden).outline_width, 0.0);
            assert_eq!(theme.active(true, selected, &visible).outline_width, 2.0);
            assert_eq!(theme.active(true, selected, &hidden).border_width, 1.0);
        }
    }

    #[test]
    fn authentication_button_does_not_reverse_enabled_and_disabled_text() {
        use cosmic::widget::button::Catalog;

        let palette = Palette {
            appearance: ferese_theme_model::Appearance::Dark,
            high_contrast: false,
            background: Color::from_rgb8(17, 24, 33),
            sidebar: Color::from_rgb8(17, 24, 33),
            card: Color::from_rgb8(17, 24, 33),
            accent: Color::from_rgb8(61, 123, 230),
            on_accent: Color::from_rgb8(17, 24, 33),
            text: Color::from_rgb8(244, 247, 251),
            muted: Color::from_rgb8(135, 147, 162),
            error: Color::from_rgb8(235, 98, 98),
            radius: 10.,
        };
        let theme = palette.native_theme();
        let class = authentication_button(palette);
        for style in [
            theme.active(false, false, &class),
            theme.hovered(false, false, &class),
            theme.pressed(false, false, &class),
        ] {
            assert_eq!(style.text_color, Some(palette.text));
            let Some(Background::Color(fill)) = style.background else {
                panic!("No fill")
            };
            assert!(crate::contrast(fill, palette.text) >= 4.5);
        }
        let disabled = theme.disabled(&class);
        assert_eq!(disabled.text_color, Some(palette.muted));
        assert_eq!(disabled.background, Some(Background::Color(palette.card)));
    }

    #[test]
    fn switch_thumb_remains_readable_in_every_preset_and_state() {
        for preset in ferese_config::presets::PRESETS {
            let resolved = ferese_theme_model::ResolvedTheme {
                appearance: preset.appearance,
                tokens: ferese_config::theme::preset(preset.id, preset.appearance).unwrap(),
                ..ferese_config::theme::default_theme()
            };
            let palette = Palette::from_resolved(&resolved);
            for enabled in [false, true] {
                for hovered in [false, true] {
                    let (track, thumb) = switch_colors(palette, enabled, hovered);
                    assert!(
                        crate::contrast(track, thumb) >= 4.5,
                        "{}: enabled={enabled}, hovered={hovered}",
                        preset.name,
                    );
                }
            }
        }
    }
}
