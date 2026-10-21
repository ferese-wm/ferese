use cosmic::iced::Color;

pub fn luminance(color: Color) -> f32 {
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

pub fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Resolve a control fill against its surface before selecting its foreground.
/// The returned fill is opaque, so wallpaper and translucent panels cannot change
/// the contrast of a small, filled control.
pub fn composite(fill: Color, surface: Color) -> Color {
    let a = fill.a.clamp(0., 1.);
    Color::from_rgb(
        fill.r * a + surface.r * (1. - a),
        fill.g * a + surface.g * (1. - a),
        fill.b * a + surface.b * (1. - a),
    )
}

/// Preserve the user's preferred color when it passes normal-text contrast.
/// Otherwise black or white always provides at least 4.5:1 on an opaque sRGB fill.
pub fn foreground(background: Color, preferred: Color) -> Color {
    let preferred = composite(preferred, background);
    if contrast(background, preferred) >= 4.5 {
        return preferred;
    }
    if contrast(background, Color::BLACK) >= contrast(background, Color::WHITE) {
        Color::BLACK
    } else {
        Color::WHITE
    }
}

/// Prefer the theme's foreground for filled controls by adjusting the accent's
/// brightness until the text has enough contrast.
pub fn accent_pair(background: Color, preferred: Color) -> (Color, Color) {
    let on = composite(preferred, background);
    let shade = if luminance(on) > luminance(background) {
        Color::BLACK
    } else {
        Color::WHITE
    };
    for step in 0..=65 {
        let fill = composite(
            Color {
                a: step as f32 / 100.,
                ..shade
            },
            background,
        );
        if contrast(fill, on) >= 4.5 {
            return (fill, on);
        }
    }
    (background, foreground(background, preferred))
}

/// Resolve configured accent transparency before passing an opaque color to a toolkit.
pub fn accent_color(accent: Color, surface: Color) -> cosmic::cosmic_theme::palette::Srgb {
    rgba(composite(accent, surface)).color
}

/// Supply the same semantic on-accent colors to native toolkit controls.
pub fn apply(native: &mut cosmic::cosmic_theme::Theme, preferred: Color) {
    let surface: Color = native.primary(false).base.into();
    for component in [&mut native.accent, &mut native.accent_button] {
        let (base, on) = accent_pair(composite(component.base.into(), surface), preferred);
        component.hover = rgba(readable_fill(
            composite(component.hover.into(), surface),
            base,
            on,
        ));
        component.pressed = rgba(readable_fill(
            composite(component.pressed.into(), surface),
            base,
            on,
        ));
        component.selected = rgba(readable_fill(
            composite(component.selected.into(), surface),
            base,
            on,
        ));
        component.base = rgba(base);
        component.on = rgba(on);
        component.selected_text = rgba(foreground(
            composite(component.selected.into(), surface),
            on,
        ));
        component.disabled = rgba(composite(Color { a: 0.5, ..base }, surface));
        component.on_disabled = rgba(foreground(composite(Color { a: 0.5, ..base }, surface), on));
    }
}

// Native controls reuse one foreground for normal/hover/pressed. Keep their
// hover feedback, but pull an unreadable fill toward the readable normal fill.
fn readable_fill(candidate: Color, base: Color, on: Color) -> Color {
    for step in 0..=20 {
        let t = step as f32 / 20.;
        let fill = Color::from_rgb(
            candidate.r + (base.r - candidate.r) * t,
            candidate.g + (base.g - candidate.g) * t,
            candidate.b + (base.b - candidate.b) * t,
        );
        if contrast(fill, on) >= 4.5 {
            return fill;
        }
    }
    base
}

fn rgba(c: Color) -> cosmic::cosmic_theme::palette::Srgba {
    cosmic::cosmic_theme::palette::Srgba::new(c.r, c.g, c.b, c.a)
}

#[cfg(test)]
mod tests {
    use cosmic::iced::Background;

    use super::*;
    use crate::button::{State, accent_button_style};
    #[test]
    fn reference_contrast_values_use_linear_srgb() {
        assert!((contrast(Color::BLACK, Color::WHITE) - 21.).abs() < 0.00001);
        assert!((contrast(Color::from_rgb8(128, 128, 128), Color::BLACK) - 5.31721).abs() < 0.0001);
    }

    #[test]
    fn foreground_preserves_readable_theme_text_and_rejects_low_contrast() {
        let preferred = Color::from_rgb8(240, 242, 246);
        assert_eq!(foreground(Color::BLACK, preferred), preferred);
        assert_eq!(foreground(Color::WHITE, preferred), Color::BLACK);
        assert_eq!(
            foreground(Color::from_rgb8(61, 123, 230), preferred),
            Color::BLACK
        );
    }

    #[test]
    fn all_accent_hues_and_opacities_have_readable_text() {
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(17) {
                    for surface in [Color::BLACK, Color::WHITE] {
                        for alpha in [0., 0.2, 0.5, 1.] {
                            let fill = composite(Color::from_rgba8(r, g, b, alpha), surface);
                            let (fill, on) = accent_pair(fill, Color::from_rgb8(120, 130, 140));
                            assert!(contrast(fill, on) >= 4.5, "{fill:?}: {on:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn blue_accent_preserves_light_theme_text_in_every_state() {
        let preferred = Color::from_rgb8(244, 247, 251);
        let mut native = cosmic::cosmic_theme::ThemeBuilder::dark()
            .accent(rgba(Color::from_rgb8(61, 123, 230)).color)
            .build();
        apply(&mut native, preferred);
        let theme = cosmic::Theme::custom(std::sync::Arc::new(native));
        for state in [
            State::Active,
            State::Hovered,
            State::Pressed,
            State::Disabled,
        ] {
            let actual = accent_button_style(&theme, state, false);
            assert_eq!(actual.text_color, Some(preferred));
            assert_eq!(actual.icon_color, Some(preferred));
        }
    }

    #[test]
    fn gruvbox_orange_button_keeps_its_light_theme_text_when_enabled() {
        let preferred = Color::from_rgb8(235, 219, 178);
        let accent = Color::from_rgb8(254, 128, 25);
        let mut native = cosmic::cosmic_theme::ThemeBuilder::dark()
            .accent(rgba(accent).color)
            .build();
        apply(&mut native, preferred);
        let theme = cosmic::Theme::custom(std::sync::Arc::new(native));

        for state in [State::Active, State::Hovered, State::Pressed] {
            let actual = accent_button_style(&theme, state, false);
            assert_eq!(actual.text_color, Some(preferred));
            let Some(Background::Color(fill)) = actual.background else {
                panic!("No button fill")
            };
            assert!(contrast(fill, preferred) >= 4.5);
        }
    }

    #[test]
    fn native_button_states_keep_text_and_icons_readable() {
        for accent in [
            Color::BLACK,
            Color::WHITE,
            Color::from_rgb8(61, 123, 230),
            Color::from_rgb8(255, 220, 0),
        ] {
            for mut native in [
                cosmic::cosmic_theme::ThemeBuilder::dark()
                    .accent(rgba(accent).color)
                    .build(),
                cosmic::cosmic_theme::ThemeBuilder::light()
                    .accent(rgba(accent).color)
                    .build(),
            ] {
                apply(&mut native, Color::from_rgb8(244, 247, 251));
                for component in [&native.accent, &native.accent_button] {
                    for fill in [component.base, component.hover, component.pressed] {
                        assert!(contrast(fill.into(), component.on.into()) >= 4.5);
                    }
                    assert!(
                        contrast(component.selected.into(), component.selected_text.into()) >= 4.5
                    );
                    assert!(
                        contrast(component.disabled.into(), component.on_disabled.into()) >= 4.5
                    );
                }
                let theme = cosmic::Theme::custom(std::sync::Arc::new(native));
                for state in [
                    State::Active,
                    State::Hovered,
                    State::Pressed,
                    State::Disabled,
                ] {
                    let style = accent_button_style(&theme, state, false);
                    let Some(Background::Color(fill)) = style.background else {
                        panic!("No fill")
                    };
                    assert_eq!(style.text_color, style.icon_color);
                    assert!(contrast(fill, style.text_color.unwrap()) >= 4.5);
                }
            }
        }
    }
}
