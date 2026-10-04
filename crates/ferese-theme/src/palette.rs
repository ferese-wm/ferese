use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Background, Color, gradient::Linear};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub appearance: ferese_theme_model::Appearance,
    pub high_contrast: bool,
    pub background: Color,
    pub sidebar: Color,
    pub card: Color,
    pub accent: Color,
    pub accent_gradient: Option<Linear>,
    pub on_accent: Color,
    pub text: Color,
    pub muted: Color,
    pub error: Color,
    pub radius: f32,
}

impl Palette {
    pub fn from_resolved(theme: &ferese_theme_model::ResolvedTheme) -> Self {
        let color = |value: &str| parse_color(value).expect("validated resolved theme color");
        Self {
            appearance: theme.appearance,
            high_contrast: theme.accessibility.increase_contrast,
            background: color(&theme.tokens.colors.application_background),
            sidebar: color(&theme.tokens.colors.surface_base),
            card: color(&theme.tokens.colors.surface_raised),
            accent: color(&theme.tokens.colors.accent),
            accent_gradient: theme.tokens.focus_ring.effective_gradient().map(|gradient| {
                // Iced's zero angle points up; theme angles start left-to-right.
                Linear::new((gradient.angle.rem_euclid(360.) as f32 + 90.).to_radians())
                    .add_stop(0., color(&gradient.from))
                    .add_stop(1., color(&gradient.to))
            }),
            on_accent: color(&theme.tokens.colors.on_accent),
            text: color(&theme.tokens.colors.text_primary),
            muted: color(&theme.tokens.colors.text_muted),
            error: mix(
                color(&theme.tokens.colors.surface_base),
                Color::from_rgb8(210, 80, 80),
                0.2,
            ),
            radius: theme.tokens.geometry.shell_radius as f32,
        }
    }

    pub fn flat(mut self) -> Self {
        self.background = self.sidebar;
        self.card = self.sidebar;
        self
    }

    pub(crate) fn selected_background(
        self,
        strength: f32,
        opacity: f32,
        foreground: Color,
        fallback: Color,
    ) -> Background {
        let Some(mut gradient) = self.accent_gradient else {
            return Background::Color(fallback);
        };

        for stop in gradient.stops.iter_mut().flatten() {
            stop.color = mix(self.sidebar, crate::composite(stop.color, self.sidebar), strength);
        }

        // Bound every interpolated color, rather than sampling a few points.
        // Adjust both stops together so one foreground stays readable.
        let minimum = if self.high_contrast { 7. } else { 4.5 };
        let shade = if crate::contrast(Color::BLACK, foreground) > crate::contrast(Color::WHITE, foreground) {
            Color::BLACK
        } else {
            Color::WHITE
        };
        for step in 0..=20 {
            let mut candidate = gradient;
            for stop in candidate.stops.iter_mut().flatten() {
                stop.color = mix(stop.color, shade, step as f32 / 20.);
                stop.color.a = opacity;
            }

            let from = crate::composite(candidate.stops[0].unwrap().color, self.sidebar);
            let to = crate::composite(candidate.stops[1].unwrap().color, self.sidebar);
            let lower = Color::from_rgb(from.r.min(to.r), from.g.min(to.g), from.b.min(to.b));
            let upper = Color::from_rgb(from.r.max(to.r), from.g.max(to.g), from.b.max(to.b));
            let lightness = crate::luminance(foreground);
            if (lightness < crate::luminance(lower) || lightness > crate::luminance(upper))
                && crate::contrast(lower, foreground) >= minimum
                && crate::contrast(upper, foreground) >= minimum
            {
                return candidate.into();
            }
        }

        Background::Color(fallback)
    }

    pub fn native_theme(self) -> cosmic::Theme {
        let rgba = |c: Color| cosmic::cosmic_theme::palette::Srgba::new(c.r, c.g, c.b, 1.);
        use cosmic::cosmic_theme::ThemeBuilder;
        use ferese_theme_model::Appearance;
        let builder = match (self.appearance, self.high_contrast) {
            (Appearance::Light, false) => ThemeBuilder::light(),
            (Appearance::Dark, false) => ThemeBuilder::dark(),
            (Appearance::Light, true) => ThemeBuilder::light_high_contrast(),
            (Appearance::Dark, true) => ThemeBuilder::dark_high_contrast(),
        };
        let corners = cosmic::cosmic_theme::CornerRadii {
            radius_xs: [self.radius.min(4.); 4],
            radius_s: [self.radius.min(8.); 4],
            radius_m: [self.radius; 4],
            radius_l: [self.radius; 4],
            radius_xl: [self.radius; 4],
            radius_0: Default::default(),
        };
        let mut native = builder
            .corner_radii(corners)
            .bg_color(rgba(self.background))
            .primary_container_bg(rgba(self.card))
            .text_tint(rgba(self.text).color)
            .accent(rgba(self.accent).color)
            .build();

        crate::apply(&mut native, self.on_accent);
        cosmic::Theme::custom(std::sync::Arc::new(native)).corner_shape(BorderShape::Continuous)
    }

    pub fn application_style(self, background: Color) -> cosmic::iced::theme::Style {
        cosmic::iced::theme::Style {
            background_color: background,
            text_color: self.text,
            icon_color: self.text,
        }
    }
}

pub fn parse_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    let packed = u32::from_str_radix(hex, 16).ok()?;
    let rgba = match hex.len() {
        6 => (packed << 8) | 255,
        8 => packed,
        _ => return None,
    };
    Some(Color::from_rgba8(
        (rgba >> 24) as u8,
        (rgba >> 16) as u8,
        (rgba >> 8) as u8,
        (rgba & 255) as f32 / 255.,
    ))
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgb(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t)
}

pub fn surface_shade(base: Color) -> Color {
    if crate::luminance(base) > 0.5 {
        Color::BLACK
    } else {
        Color::WHITE
    }
}

pub fn material_opacity(theme: &ferese_theme_model::ResolvedTheme) -> f32 {
    if theme.tokens.material.style == "translucent" {
        theme.tokens.material.opacity as f32
    } else {
        1.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_theme_preserves_the_resolved_accessibility_mode() {
        for appearance in [
            ferese_theme_model::Appearance::Light,
            ferese_theme_model::Appearance::Dark,
        ] {
            for high_contrast in [false, true] {
                let mut resolved = ferese_theme_model::ResolvedTheme {
                    appearance,
                    ..ferese_config::theme::default_theme()
                };
                resolved.accessibility.increase_contrast = high_contrast;
                let palette = Palette::from_resolved(&resolved);
                assert_eq!(palette.high_contrast, high_contrast);
                let native = palette.native_theme();
                assert_eq!(native.cosmic().is_high_contrast, high_contrast);
                assert_eq!(native.corner_shape, BorderShape::Continuous);
            }
        }
    }

    #[test]
    fn selected_gradients_preserve_material_alpha_and_text_contrast() {
        use cosmic::widget::button::Catalog;
        for preset in ferese_config::presets::PRESETS {
            let mode = if preset.appearance == ferese_theme_model::Appearance::Light {
                "light"
            } else {
                "dark"
            };
            let document = ferese_config::Document::parse(&format!(
                "theme {{ mode {mode}; family {}; }}",
                ferese_config::families::family_id(preset.id)
            ))
            .unwrap();
            let resolved = ferese_config::theme::resolve(
                &document,
                std::path::Path::new("/config"),
                "2026-09-30T12:00:00Z".parse().unwrap(),
                |_| unreachable!(),
            )
            .unwrap()
            .theme;
            let palette = Palette::from_resolved(&resolved);
            let native = palette.native_theme();
            for opacity in [1., 0.5] {
                let class = crate::controls::material_button_style(palette, true, opacity);
                for style in [native.active(false, true, &class), native.hovered(false, true, &class)] {
                    let Some(Background::Gradient(cosmic::iced::Gradient::Linear(gradient))) = style.background else {
                        panic!("{} selected control lost its gradient", preset.id);
                    };
                    let from = gradient.stops[0].unwrap().color;
                    let to = gradient.stops[1].unwrap().color;
                    assert_eq!(from.a, to.a);
                    assert!(if opacity == 1. { from.a == 1. } else { from.a < 1. });
                    for step in 0..=32 {
                        let fill = crate::composite(
                            Color {
                                a: from.a,
                                ..mix(from, to, step as f32 / 32.)
                            },
                            palette.sidebar,
                        );
                        assert!(crate::contrast(fill, style.text_color.unwrap()) >= 4.5, "{}", preset.id);
                    }
                }
            }

            let class = crate::controls::button_style(palette, false);
            assert!(matches!(
                native.active(false, false, &class).background,
                Some(Background::Color(_))
            ));
        }
    }

    #[test]
    fn gradient_angles_and_solid_paints_reach_the_control_palette() {
        let mut resolved = ferese_config::theme::default_theme();
        resolved.tokens.focus_ring.gradient.as_mut().unwrap().angle = 0.;
        let palette = Palette::from_resolved(&resolved);
        assert!((palette.accent_gradient.unwrap().angle.0 - std::f32::consts::FRAC_PI_2).abs() < 0.001);
        resolved.tokens.focus_ring.style = ferese_theme_model::PaintStyle::Solid;
        assert!(Palette::from_resolved(&resolved).accent_gradient.is_none());
        resolved.tokens.focus_ring.style = ferese_theme_model::PaintStyle::Auto;
        resolved.tokens.focus_ring.gradient = None;
        assert!(Palette::from_resolved(&resolved).accent_gradient.is_none());
    }

    #[test]
    fn every_preset_builds_an_opaque_control_palette_over_translucent_materials() {
        for preset in ferese_config::presets::PRESETS {
            let mut resolved = ferese_theme_model::ResolvedTheme {
                appearance: preset.appearance,
                tokens: ferese_config::theme::preset(preset.id, preset.appearance).unwrap(),
                ..ferese_config::theme::default_theme()
            };
            resolved.tokens.material.style = "translucent".into();
            resolved.tokens.material.opacity = 0.25;
            resolved.tokens.geometry.shell_radius = 0.;
            let palette = Palette::from_resolved(&resolved);
            let native = palette.native_theme();
            assert_eq!(crate::material_opacity(&resolved), 0.25);
            assert_eq!(native.cosmic().primary(false).base.alpha, 1.);
            assert_eq!(native.cosmic().corner_radii.radius_m, [0.; 4]);
            for component in [&native.cosmic().accent, &native.cosmic().accent_button] {
                for fill in [component.base, component.hover, component.pressed] {
                    assert!(
                        crate::contrast(fill.into(), component.on.into()) >= 4.5,
                        "{}",
                        preset.name
                    );
                }
            }
        }
    }

    #[test]
    fn solid_materials_ignore_opacity_and_translucent_materials_bound_it() {
        for (style, opacity, expected) in [
            ("solid", 0.4, 1.),
            ("translucent", 0.4, 0.4),
            ("translucent", 1., 1.),
            ("translucent", 0., 0.),
        ] {
            let mut theme = ferese_config::theme::default_theme();
            theme.tokens.material.style = style.into();
            theme.tokens.material.opacity = opacity;
            assert_eq!(crate::material_opacity(&theme), expected);
        }
    }
}
