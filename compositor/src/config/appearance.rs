use super::*;

impl Config {
    pub(crate) fn focus_effect_settings(&self) -> Result<FocusEffectSettings, ConfigError> {
        let effect = &self.appearance.focus_effect;
        Ok(FocusEffectSettings {
            enabled: effect.enabled,
            active_opacity: unit_theme_value(effect.active_opacity, "appearance.focus_effect.active_opacity")?,
            inactive_opacity: unit_theme_value(effect.inactive_opacity, "appearance.focus_effect.inactive_opacity")?,
            inactive_dim: unit_theme_value(effect.inactive_dim, "appearance.focus_effect.inactive_dim")?,
            duration_ms: nonnegative_theme_value(effect.duration_ms, "appearance.focus_effect.duration_ms")?,
        })
    }

    pub fn theme_settings(&self) -> Result<ThemeSettings, ConfigError> {
        let border_width = nonnegative_theme_value(self.theme.geometry.border_width, "geometry.border_width")?;
        let focus_ring_width =
            nonnegative_theme_value(self.theme.geometry.focus_ring_width, "geometry.focus_ring_width")?;
        let window_radius = nonnegative_theme_value(self.theme.geometry.window_radius, "geometry.window_radius")?;
        let shadow_offset_y = finite_theme_value(self.theme.shadow.soft.offset_y, "shadow.soft.offset_y")?;
        let shadow_blur = nonnegative_theme_value(self.theme.shadow.soft.blur, "shadow.soft.blur")?;
        let shadow_opacity = unit_theme_value(self.theme.shadow.soft.opacity, "shadow.soft.opacity")?;

        let material_radius = nonnegative_theme_value(
            self.theme.geometry.shell_radius.unwrap_or(14.0),
            "geometry.shell_radius",
        )?;
        Ok(ThemeSettings {
            border_width,
            focus_ring_width,
            border_color: parse_color(&self.theme.colors.border, "colors.border")?,
            text_primary_color: parse_color(&self.theme.colors.text_primary, "colors.text_primary")?,
            accent_color: parse_color(&self.theme.colors.accent, "colors.accent")?,
            border_gradient: self.theme.border.settings("border")?,
            focus_ring_gradient: self.theme.focus_ring.settings("focus_ring")?,
            shadow_color: parse_color(&self.theme.colors.shadow, "colors.shadow")?,
            surface_base_color: parse_color(&self.theme.colors.surface_base, "colors.surface_base")?,
            bar_background_color: parse_color(
                self.theme
                    .surface
                    .bar
                    .background
                    .as_deref()
                    .unwrap_or(&self.theme.colors.surface_base),
                "surface.bar.background",
            )?,
            shell_opacity: unit_theme_value(self.theme.material.opacity, "material.opacity")?,
            window_radius,
            shadow_offset_y,
            shadow_blur,
            shadow_opacity,
            material_style: self.theme.material.style,
            backdrop_blur: nonnegative_theme_value(self.theme.material.blur_radius, "material.blur_radius")?.min(32.0),
            material_tint_strength: unit_theme_value(self.theme.material.tint_strength, "material.tint_strength")?,
            material_radius,
            panel_radius: material_radius,
        })
    }
}

pub(super) fn nonnegative_theme_value(value: f64, field: &'static str) -> Result<f64, ConfigError> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(ConfigError::ThemeValue {
            field,
            value: value.to_string(),
        })
    }
}

pub(super) fn finite_theme_value(value: f64, field: &'static str) -> Result<f64, ConfigError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ConfigError::ThemeValue {
            field,
            value: value.to_string(),
        })
    }
}

pub(super) fn unit_theme_value(value: f64, field: &'static str) -> Result<f64, ConfigError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(ConfigError::ThemeValue {
            field,
            value: value.to_string(),
        })
    }
}

pub(super) fn parse_color(value: &str, field: &'static str) -> Result<RgbaColor, ConfigError> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if !digits.is_ascii() || !matches!(digits.len(), 6 | 8) {
        return Err(ConfigError::ThemeValue {
            field,
            value: value.to_owned(),
        });
    }

    let parse_channel = |offset| u8::from_str_radix(&digits[offset..offset + 2], 16).ok();
    let Some((red, green, blue)) = parse_channel(0)
        .zip(parse_channel(2))
        .zip(parse_channel(4))
        .map(|((red, green), blue)| (red, green, blue))
    else {
        return Err(ConfigError::ThemeValue {
            field,
            value: value.to_owned(),
        });
    };
    let alpha = if digits.len() == 8 {
        parse_channel(6).ok_or_else(|| ConfigError::ThemeValue {
            field,
            value: value.to_owned(),
        })?
    } else {
        u8::MAX
    };

    Ok(RgbaColor([
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        f32::from(alpha) / 255.0,
    ]))
}

pub(super) fn default_border_color() -> String {
    "#FFFFFF18".to_owned()
}

pub(super) fn default_text_primary_color() -> String {
    "#F4F7FB".to_owned()
}

pub(super) fn default_surface_base_color() -> String {
    "#111821".to_owned()
}

pub(super) fn default_accent_color() -> String {
    "#3D7BE6".to_owned()
}

pub(super) fn default_shadow_color() -> String {
    "#00000055".to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbaColor(pub [f32; 4]);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderGradient {
    pub from: RgbaColor,
    pub to: RgbaColor,
    pub angle: f64,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct BorderPaintConfig {
    #[serde(default)]
    pub(super) style: ferese_config::theme::PaintStyle,
    pub(super) gradient: Option<BorderGradientConfig>,
}

#[derive(Debug, Deserialize)]
pub(super) struct BorderGradientConfig {
    pub(super) from: String,
    pub(super) to: String,
    #[serde(default)]
    pub(super) angle: f64,
}

impl BorderPaintConfig {
    fn settings(&self, name: &str) -> Result<Option<BorderGradient>, ConfigError> {
        let (from_name, to_name, angle_name) = if name == "focus_ring" {
            (
                "focus_ring.gradient.from",
                "focus_ring.gradient.to",
                "focus_ring.gradient.angle",
            )
        } else {
            ("border.gradient.from", "border.gradient.to", "border.gradient.angle")
        };
        let gradient = self
            .gradient
            .as_ref()
            .map(|gradient| {
                Ok(BorderGradient {
                    from: parse_color(&gradient.from, from_name)?,
                    to: parse_color(&gradient.to, to_name)?,
                    angle: finite_theme_value(gradient.angle, angle_name)?.rem_euclid(360.0),
                })
            })
            .transpose()?;

        Ok(if self.style == ferese_config::theme::PaintStyle::Solid {
            None
        } else {
            gradient
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeSettings {
    pub border_width: f64,
    pub focus_ring_width: f64,
    pub border_color: RgbaColor,
    pub accent_color: RgbaColor,
    pub border_gradient: Option<BorderGradient>,
    pub focus_ring_gradient: Option<BorderGradient>,
    pub shadow_color: RgbaColor,
    pub surface_base_color: RgbaColor,
    pub bar_background_color: RgbaColor,
    pub text_primary_color: RgbaColor,
    pub shell_opacity: f64,
    pub window_radius: f64,
    pub shadow_offset_y: f64,
    pub shadow_blur: f64,
    pub shadow_opacity: f64,
    pub material_style: MaterialStyle,
    pub backdrop_blur: f64,
    pub material_tint_strength: f64,
    pub material_radius: f64,
    pub panel_radius: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MaterialStyle {
    Translucent,
    #[default]
    Solid,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AppearanceConfig {
    #[serde(default)]
    pub(super) focus_effect: FocusEffectConfig,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusEffectSettings {
    pub enabled: bool,
    pub active_opacity: f64,
    pub inactive_opacity: f64,
    pub inactive_dim: f64,
    pub duration_ms: f64,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct FocusEffectConfig {
    pub(super) enabled: bool,
    pub(super) active_opacity: f64,
    pub(super) inactive_opacity: f64,
    pub(super) inactive_dim: f64,
    pub(super) duration_ms: f64,
}

impl Default for FocusEffectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            active_opacity: 1.0,
            inactive_opacity: 1.0,
            inactive_dim: 0.0,
            duration_ms: 150.0,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ThemeConfig {
    #[serde(default)]
    pub(super) border: BorderPaintConfig,
    #[serde(default)]
    pub(super) focus_ring: BorderPaintConfig,
    #[serde(default)]
    pub(super) typography: OverviewTypographyConfig,
    #[serde(default)]
    pub(super) background: crate::wallpaper::WallpaperConfig,
    #[serde(default)]
    pub(super) colors: ThemeColorsConfig,
    #[serde(default)]
    pub(super) geometry: ThemeGeometryConfig,
    #[serde(default)]
    pub(super) shadow: ThemeShadowConfig,
    #[serde(default)]
    pub(super) material: ThemeMaterialConfig,
    #[serde(default)]
    pub(super) surface: ThemeSurfaceConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ThemeSurfaceConfig {
    #[serde(default)]
    pub(super) bar: ThemeBarConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ThemeBarConfig {
    pub(super) background: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct OverviewTypographyConfig {
    pub(super) font_family: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ThemeMaterialConfig {
    #[serde(default = "default_shell_opacity")]
    pub(super) opacity: f64,
    #[serde(default)]
    pub(super) style: MaterialStyle,
    #[serde(default = "default_backdrop_blur")]
    pub(super) blur_radius: f64,
    #[serde(default = "default_material_tint_strength")]
    pub(super) tint_strength: f64,
}

impl Default for ThemeMaterialConfig {
    fn default() -> Self {
        Self {
            style: MaterialStyle::Solid,
            opacity: default_shell_opacity(),
            blur_radius: default_backdrop_blur(),
            tint_strength: default_material_tint_strength(),
        }
    }
}

fn default_shell_opacity() -> f64 {
    ferese_config::DEFAULT_MATERIAL_OPACITY
}

fn default_material_tint_strength() -> f64 {
    0.5
}

fn default_backdrop_blur() -> f64 {
    12.0
}

#[derive(Debug, Deserialize)]
pub(super) struct ThemeColorsConfig {
    #[serde(default = "default_text_primary_color")]
    pub(super) text_primary: String,
    #[serde(default = "default_surface_base_color")]
    pub(super) surface_base: String,
    #[serde(default = "default_border_color")]
    pub(super) border: String,
    #[serde(default = "default_accent_color")]
    pub(super) accent: String,
    #[serde(default = "default_shadow_color")]
    pub(super) shadow: String,
}

impl Default for ThemeColorsConfig {
    fn default() -> Self {
        Self {
            surface_base: default_surface_base_color(),
            text_primary: default_text_primary_color(),
            border: default_border_color(),
            accent: default_accent_color(),
            shadow: default_shadow_color(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ThemeShadowConfig {
    #[serde(default)]
    pub(super) soft: SoftShadowConfig,
}

#[derive(Debug, Deserialize)]
pub(super) struct SoftShadowConfig {
    #[serde(default = "default_shadow_offset_y")]
    pub(super) offset_y: f64,
    #[serde(default = "default_shadow_blur")]
    pub(super) blur: f64,
    #[serde(default = "default_shadow_opacity")]
    pub(super) opacity: f64,
}

impl Default for SoftShadowConfig {
    fn default() -> Self {
        Self {
            offset_y: default_shadow_offset_y(),
            blur: default_shadow_blur(),
            opacity: default_shadow_opacity(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ThemeGeometryConfig {
    #[serde(default = "default_border_width")]
    pub(super) border_width: f64,
    #[serde(default = "default_focus_ring_width")]
    pub(super) focus_ring_width: f64,
    #[serde(default = "default_window_radius")]
    pub(super) window_radius: f64,
    #[serde(default)]
    pub(super) shell_radius: Option<f64>,
}

impl Default for ThemeGeometryConfig {
    fn default() -> Self {
        Self {
            border_width: default_border_width(),
            focus_ring_width: default_focus_ring_width(),
            window_radius: default_window_radius(),
            shell_radius: None,
        }
    }
}
