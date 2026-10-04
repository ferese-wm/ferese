use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_BACKGROUND: [u8; 3] = [11, 15, 20];

#[derive(Clone, Debug, Default)]
pub(crate) struct ShellConfig {
    pub(crate) notifications: ferese_config::notifications::NotificationConfig,
    pub(crate) desktop_widgets: ferese_config::desktop::DesktopWidgets,
    pub(crate) animations: crate::motion::Settings,
    pub(crate) font_family: Option<String>,
    pub(crate) wallpaper: WallpaperConfig,
    pub(crate) theme: ShellTheme,
    pub(crate) theme_mode: ferese_config::theme::Mode,
    pub(crate) status: StatusConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub(crate) struct StatusConfig {
    pub(crate) keybinding_guide: bool,
    pub(crate) battery_percentage: bool,
    pub(crate) window_title: bool,
    pub(crate) low_battery_threshold: u8,
    pub(crate) settings_command: Option<Vec<String>>,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            keybinding_guide: true,
            battery_percentage: true,
            window_title: true,
            low_battery_threshold: 20,
            settings_command: Some(vec!["ferese-settings".into()]),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ShellTheme {
    pub(crate) appearance: ferese_config::theme::Appearance,
    pub(crate) high_contrast: bool,
    pub(crate) bar_background: [u8; 4],
    pub(crate) bar_text_primary: [u8; 4],
    pub(crate) bar_text_muted: [u8; 4],
    pub(crate) surface_base: [u8; 4],
    pub(crate) surface_popover: [u8; 4],
    pub(crate) text_primary: [u8; 4],
    pub(crate) text_muted: [u8; 4],
    pub(crate) accent: [u8; 4],
    pub(crate) accent_gradient: Option<cosmic::iced::gradient::Linear>,
    pub(crate) on_accent: [u8; 4],
    pub(crate) border: [u8; 4],
    pub(crate) shadow: [u8; 4],
    pub(crate) bar_height: f32,
    pub(crate) bar_margin_top: i32,
    pub(crate) bar_window_gap: i32,
    pub(crate) bar_margin_horizontal: i32,
    pub(crate) bar_radius: f32,
    pub(crate) material_radius: f32,
    pub(crate) panel_padding: f32,
    pub(crate) control_gap: f32,
    pub(crate) shadow_offset_y: f32,
    pub(crate) shadow_blur: f32,
    pub(crate) shadow_opacity: f32,
}

impl Default for ShellTheme {
    fn default() -> Self {
        Self {
            appearance: Default::default(),
            high_contrast: false,
            bar_background: [28, 32, 46, 255],
            bar_text_primary: [240, 243, 250, 255],
            bar_text_muted: [170, 180, 199, 255],
            surface_base: [17, 24, 33, 255],
            surface_popover: [17, 24, 33, 255],
            text_primary: [244, 247, 251, 255],
            text_muted: [135, 147, 162, 255],
            accent: [61, 123, 230, 255],
            accent_gradient: None,
            on_accent: [244, 247, 251, 255],
            border: [255, 255, 255, 24],
            shadow: [0, 0, 0, 85],
            bar_height: 28.0,
            bar_margin_top: 0,
            bar_window_gap: 0,
            bar_margin_horizontal: 0,
            bar_radius: 14.0,
            material_radius: 14.0,
            panel_padding: 12.0,
            control_gap: 12.0,
            shadow_offset_y: 4.0,
            shadow_blur: 18.0,
            shadow_opacity: 0.20,
        }
    }
}

impl ShellTheme {
    pub(crate) fn palette(self) -> ferese_theme::Palette {
        let color = |[r, g, b, a]: [u8; 4]| cosmic::iced::Color::from_rgba8(r, g, b, f32::from(a) / 255.);
        let surface = color(self.surface_base);
        ferese_theme::Palette {
            appearance: self.appearance,
            high_contrast: self.high_contrast,
            background: surface,
            sidebar: surface,
            card: surface,
            text: color(self.text_primary),
            muted: color(self.text_muted),
            accent: color(self.accent),
            accent_gradient: self.accent_gradient,
            on_accent: color(self.on_accent),
            radius: self.material_radius,
            error: cosmic::iced::Color::from_rgb8(235, 98, 98),
        }
    }

    // The bar has its own foreground tokens, matched to its surface.
    pub(crate) fn for_bar(mut self) -> Self {
        self.text_primary = self.bar_text_primary;
        self.text_muted = self.bar_text_muted;
        self
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WallpaperConfig {
    #[serde(default = "default_wallpaper_path")]
    pub(crate) path: Option<PathBuf>,
    #[serde(default)]
    pub(crate) mode: WallpaperMode,
}

fn default_wallpaper_path() -> Option<PathBuf> {
    Some(PathBuf::from(ferese_config::default_wallpaper()))
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            path: default_wallpaper_path(),
            mode: WallpaperMode::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WallpaperMode {
    #[default]
    Fill,
    Fit,
}

#[derive(Debug, Default, Deserialize)]
struct FereseConfig {
    #[serde(default)]
    notifications: ferese_config::notifications::NotificationConfig,
    #[serde(default)]
    desktop_widgets: ferese_config::desktop::DesktopWidgets,
    #[serde(default)]
    animations: crate::motion::Settings,
    #[serde(default)]
    appearance: AppearanceConfig,
    #[serde(default)]
    theme: ThemeConfig,
    #[serde(default)]
    status: StatusConfig,
}

#[derive(Debug, Default, Deserialize)]
struct AppearanceConfig {
    corner_radius: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
struct ThemeConfig {
    #[serde(default)]
    material: MaterialConfig,
    #[serde(default)]
    surface: SurfaceConfig,
    #[serde(default)]
    colors: ThemeColorsConfig,
    #[serde(default)]
    geometry: ThemeGeometryConfig,
    #[serde(default)]
    shadow: ThemeShadowConfig,
    #[serde(default)]
    typography: TypographyConfig,
    #[serde(default)]
    background: WallpaperConfig,
}

#[derive(Debug, Default, Deserialize)]
struct SurfaceConfig {
    #[serde(default)]
    bar: BarConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct BarConfig {
    background: String,
    text_primary: String,
    text_muted: String,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            background: "#1C202EF2".to_owned(),
            text_primary: "#F0F3FA".to_owned(),
            text_muted: "#AAB4C7".to_owned(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct MaterialConfig {
    #[serde(default)]
    style: String,
    opacity: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
struct TypographyConfig {
    font_family: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ThemeColorsConfig {
    #[serde(default = "default_surface_base")]
    surface_base: String,
    #[serde(default = "default_text_primary")]
    text_primary: String,
    #[serde(default = "default_text_muted")]
    text_muted: String,
    #[serde(default = "default_accent")]
    accent: String,
    #[serde(default = "default_text_primary")]
    on_accent: String,
    #[serde(default = "default_border")]
    border: String,
    #[serde(default = "default_shadow")]
    shadow: String,
}

impl Default for ThemeColorsConfig {
    fn default() -> Self {
        Self {
            surface_base: default_surface_base(),
            text_primary: default_text_primary(),
            text_muted: default_text_muted(),
            accent: default_accent(),
            on_accent: default_text_primary(),
            border: default_border(),
            shadow: default_shadow(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ThemeGeometryConfig {
    #[serde(default = "default_bar_height")]
    top_bar_height: f32,
    #[serde(default = "default_bar_margin_top")]
    top_bar_margin_top: i32,
    #[serde(default = "default_bar_window_gap")]
    top_bar_window_gap: i32,
    #[serde(default = "default_bar_margin_horizontal")]
    top_bar_margin_horizontal: i32,
    #[serde(default)]
    shell_radius: Option<f32>,
    #[serde(default)]
    top_bar_radius: Option<f32>,
    #[serde(default = "default_panel_padding")]
    panel_padding: f32,
    #[serde(default = "default_control_gap")]
    control_gap: f32,
}

impl Default for ThemeGeometryConfig {
    fn default() -> Self {
        Self {
            top_bar_height: default_bar_height(),
            top_bar_margin_top: default_bar_margin_top(),
            top_bar_window_gap: default_bar_window_gap(),
            top_bar_margin_horizontal: default_bar_margin_horizontal(),
            shell_radius: None,
            top_bar_radius: None,
            panel_padding: default_panel_padding(),
            control_gap: default_control_gap(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ThemeShadowConfig {
    #[serde(default)]
    soft: SoftShadowConfig,
}

#[derive(Debug, Deserialize)]
struct SoftShadowConfig {
    #[serde(default = "default_shadow_offset_y")]
    offset_y: f32,
    #[serde(default = "default_shadow_blur")]
    blur: f32,
    #[serde(default = "default_shadow_opacity")]
    opacity: f32,
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

pub(crate) fn load() -> ShellConfig {
    let Some(path) = config_path() else {
        return ShellConfig::default();
    };
    let Ok(source) = fs::read_to_string(&path) else {
        return ShellConfig::default();
    };

    match parse_source(&source) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("ferese-shell: failed to read {}: {error}", path.display());
            ShellConfig::default()
        }
    }
}

pub(crate) fn parse_source(source: &str) -> Result<ShellConfig, ferese_config::Error> {
    let document = ferese_config::Document::parse(source)?;
    let snapshot = ferese_theme_client::service::current();
    let mut config = parse_document(
        &document.with_theme(&snapshot.presented),
        snapshot.presented.appearance,
        snapshot.mode,
    )?;
    config.apply_theme(&snapshot.presented);
    Ok(config)
}

fn parse_document(
    document: &ferese_config::Document,
    appearance: ferese_config::theme::Appearance,
    mode: ferese_config::theme::Mode,
) -> Result<ShellConfig, ferese_config::Error> {
    match serde_json::from_value::<FereseConfig>(document.value().clone())
        .map_err(|error| ferese_config::Error::from(error.to_string()))
    {
        Ok(config) => {
            config.animations.validate().map_err(ferese_config::Error::from)?;
            config.notifications.validate().map_err(ferese_config::Error::from)?;
            config.desktop_widgets.validate().map_err(ferese_config::Error::from)?;
            let mut theme = shell_theme(&config.theme);
            theme.appearance = appearance;
            theme.material_radius = nonnegative_or(
                config
                    .theme
                    .geometry
                    .shell_radius
                    .or(config.appearance.corner_radius)
                    .or(config.theme.geometry.top_bar_radius)
                    .unwrap_or(14.0),
                14.0,
            );
            theme.bar_radius = theme.material_radius;

            Ok(ShellConfig {
                notifications: config.notifications,
                desktop_widgets: config.desktop_widgets,
                animations: config.animations,
                font_family: config.theme.typography.font_family,
                wallpaper: config.theme.background,
                theme,
                theme_mode: mode,
                status: StatusConfig {
                    low_battery_threshold: config.status.low_battery_threshold.min(100),
                    ..config.status
                },
            })
        }
        Err(error) => Err(error),
    }
}

impl ShellConfig {
    pub(crate) fn apply_theme(&mut self, theme: &ferese_config::theme::ResolvedTheme) {
        let config: ThemeConfig = serde_json::from_value(serde_json::to_value(&theme.tokens).unwrap()).unwrap();
        self.theme = shell_theme(&config);
        self.theme.appearance = theme.appearance;
        self.theme.high_contrast = theme.accessibility.increase_contrast;
        self.theme.accent_gradient = ferese_theme::Palette::from_resolved(theme).accent_gradient;
        self.theme.material_radius = theme.tokens.geometry.shell_radius as f32;
        self.theme.bar_radius = self.theme.material_radius;
        self.font_family = Some(theme.tokens.typography.font_family.clone());
        self.wallpaper = config.background;
        if std::env::var_os("FERESE_COMPOSITOR_WALLPAPER").is_some() {
            self.wallpaper.path = None;
        }
    }
}

fn shell_theme(theme: &ThemeConfig) -> ShellTheme {
    let defaults = ShellTheme::default();
    let opacity = if theme.material.style == "translucent" {
        finite_or(
            theme
                .material
                .opacity
                .unwrap_or(ferese_config::DEFAULT_MATERIAL_OPACITY as f32),
            ferese_config::DEFAULT_MATERIAL_OPACITY as f32,
        )
        .clamp(0.0, 1.0)
    } else {
        1.0
    };
    let alpha = (opacity * 255.0).round() as u8;

    ShellTheme {
        appearance: defaults.appearance,
        high_contrast: defaults.high_contrast,
        material_radius: defaults.material_radius,
        bar_background: {
            let mut background = parse_color(&theme.surface.bar.background).unwrap_or(defaults.bar_background);
            background[3] = alpha;
            background
        },
        bar_text_primary: parse_color(&theme.surface.bar.text_primary).unwrap_or(defaults.bar_text_primary),
        bar_text_muted: parse_color(&theme.surface.bar.text_muted).unwrap_or(defaults.bar_text_muted),
        surface_base: parse_color(&theme.colors.surface_base).unwrap_or(defaults.surface_base),
        surface_popover: {
            let mut color = parse_color(&theme.colors.surface_base).unwrap_or(defaults.surface_base);
            color[3] = alpha;
            color
        },
        text_primary: parse_color(&theme.colors.text_primary).unwrap_or(defaults.text_primary),
        text_muted: parse_color(&theme.colors.text_muted).unwrap_or(defaults.text_muted),
        accent: parse_color(&theme.colors.accent).unwrap_or(defaults.accent),
        accent_gradient: defaults.accent_gradient,
        on_accent: parse_color(&theme.colors.on_accent).unwrap_or(defaults.on_accent),
        border: parse_color(&theme.colors.border).unwrap_or(defaults.border),
        shadow: parse_color(&theme.colors.shadow).unwrap_or(defaults.shadow),
        bar_height: positive_or(theme.geometry.top_bar_height, defaults.bar_height),
        bar_margin_top: theme.geometry.top_bar_margin_top.max(0),
        bar_window_gap: theme.geometry.top_bar_window_gap.max(0),
        bar_margin_horizontal: theme.geometry.top_bar_margin_horizontal.max(0),
        bar_radius: nonnegative_or(
            theme
                .geometry
                .shell_radius
                .or(theme.geometry.top_bar_radius)
                .unwrap_or(14.0),
            defaults.bar_radius,
        ),
        panel_padding: nonnegative_or(theme.geometry.panel_padding, defaults.panel_padding),
        control_gap: nonnegative_or(theme.geometry.control_gap, defaults.control_gap),
        shadow_offset_y: finite_or(theme.shadow.soft.offset_y, defaults.shadow_offset_y),
        shadow_blur: nonnegative_or(theme.shadow.soft.blur, defaults.shadow_blur),
        shadow_opacity: finite_or(theme.shadow.soft.opacity, defaults.shadow_opacity).clamp(0.0, 1.0),
    }
}

pub(crate) fn parse_color(value: &str) -> Option<[u8; 4]> {
    let value = value.strip_prefix('#')?;
    if value.len() != 6 && value.len() != 8 {
        return None;
    }

    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    let alpha = if value.len() == 8 {
        u8::from_str_radix(&value[6..8], 16).ok()?
    } else {
        255
    };

    Some([red, green, blue, alpha])
}

fn positive_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn nonnegative_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn default_surface_base() -> String {
    "#111821".to_owned()
}

fn default_text_primary() -> String {
    "#F4F7FB".to_owned()
}

fn default_text_muted() -> String {
    "#8793A2".to_owned()
}

fn default_accent() -> String {
    "#3D7BE6".to_owned()
}

fn default_border() -> String {
    "#FFFFFF18".to_owned()
}

fn default_shadow() -> String {
    "#00000055".to_owned()
}

const fn default_bar_height() -> f32 {
    28.0
}

const fn default_bar_window_gap() -> i32 {
    0
}

const fn default_bar_margin_top() -> i32 {
    0
}

const fn default_bar_margin_horizontal() -> i32 {
    0
}

const fn default_panel_padding() -> f32 {
    12.0
}

const fn default_control_gap() -> f32 {
    12.0
}

const fn default_shadow_offset_y() -> f32 {
    4.0
}

const fn default_shadow_blur() -> f32 {
    18.0
}

const fn default_shadow_opacity() -> f32 {
    0.20
}

pub(crate) const fn default_background() -> [u8; 3] {
    DEFAULT_BACKGROUND
}

pub(crate) fn config_path() -> Option<PathBuf> {
    ferese_config::config_path()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_removed_physics_keys_before_publication() {
        for property in ["spring", "viewport-spring"] {
            for coefficient in ["mass", "stiffness", "damping", "damping-ratio"] {
                let source = format!("animations {{ {property} {{ {coefficient} 0; overshoot #true; }}; }}");
                let error = parse_test_source(&source).unwrap_err().to_string();
                assert!(error.contains("unknown field"), "{error}");
            }
        }
    }

    #[test]
    fn desktop_clock_parses_and_rejects_invalid_reload_values() {
        let clock = parse_test_source(
            r#"desktop-widgets {
    clock {
        enabled #true
        anchor "bottom_right"
        font-family ""
        color ""
        time-zone ""
    }
}
"#,
        )
        .unwrap()
        .desktop_widgets
        .clock;
        assert!(clock.enabled);
        assert_eq!(clock.anchor, ferese_config::desktop::Anchor::BottomRight);
        assert!(parse_test_source("desktop-widgets {\n    clock {\n        opacity 1.1\n    }\n}\n").is_err());
        assert!(parse_test_source("desktop-widgets {\n    clock {\n        time-format \"%\"\n    }\n}\n").is_err());
    }
    use super::*;

    fn parse_test_source(source: &str) -> Result<ShellConfig, ferese_config::Error> {
        let document = ferese_config::Document::parse(source)?;
        parse_document(&document, Default::default(), Default::default())
    }

    #[test]
    fn keybinding_guide_is_enabled_until_explicitly_disabled() {
        assert!(parse_test_source("").unwrap().status.keybinding_guide);
        assert!(
            !parse_test_source("status { keybinding-guide #false; }")
                .unwrap()
                .status
                .keybinding_guide
        );
    }

    #[test]
    fn parses_theme_without_rejecting_compositor_sections() {
        let config: FereseConfig = ferese_config::from_str(
            r#"layout {
    mode "scrolling"
}
theme {
    typography {
        font-family "JetBrainsMono Nerd Font"
    }
    background {
        path "/tmp/wallpaper.png"
        mode "fit"
    }
}
"#,
        )
        .unwrap();

        assert_eq!(
            config.theme.typography.font_family.as_deref(),
            Some("JetBrainsMono Nerd Font")
        );
        assert_eq!(config.theme.background.path, Some(PathBuf::from("/tmp/wallpaper.png")));
        assert_eq!(config.theme.background.mode, WallpaperMode::Fit);
    }

    #[test]
    fn shell_radius_unifies_surfaces_and_preserves_legacy_fallbacks() {
        for radius in [0.0, 18.0] {
            let config = parse_test_source(&format!("appearance {{\n corner-radius 9\n}}\ntheme {{\n geometry {{\n shell-radius {radius}\n top-bar-radius 5\n window-radius 23\n }}\n}}")).unwrap();
            assert_eq!(config.theme.material_radius, radius);
            assert_eq!(config.theme.bar_radius, radius);
        }
        let legacy = parse_test_source("theme {\n geometry {\n top-bar-radius 7\n }\n}").unwrap();
        assert_eq!(legacy.theme.material_radius, 7.0);
        assert_eq!(legacy.theme.bar_radius, 7.0);
        let legacy = parse_test_source("appearance {\n corner-radius 6\n}").unwrap();
        assert_eq!(legacy.theme.material_radius, 6.0);
        assert_eq!(legacy.theme.bar_radius, 6.0);
    }

    #[test]
    fn defaults_to_the_ferese_visual_profile() {
        let config: FereseConfig = ferese_config::from_str("").unwrap();

        assert_eq!(config.theme.typography.font_family, None);
        assert_eq!(
            config.theme.background.path,
            Some(PathBuf::from(ferese_config::default_wallpaper()))
        );
        assert_eq!(config.theme.background.mode, WallpaperMode::Fill);
        let theme = shell_theme(&config.theme);
        assert_eq!(theme.bar_margin_top, 0);
        assert_eq!(theme.bar_height, 28.0);
        assert_eq!(theme.bar_margin_horizontal, 0);
        assert_eq!(theme.bar_radius, 14.0);
        assert_eq!(theme.bar_background[3], 255);
        assert_eq!(theme.for_bar().text_primary, theme.bar_text_primary);
        assert_ne!(theme.for_bar().text_primary, theme.text_primary);
    }

    #[test]
    fn bar_palette_can_be_changed_without_changing_popovers() {
        let config: FereseConfig = ferese_config::from_str(
            r##"theme {
    surface {
        bar {
            background "#EAECEEDD"
            text-primary "#222222"
            text-muted "#666666"
        }
    }
}
"##,
        )
        .unwrap();
        let theme = shell_theme(&config.theme);
        assert_eq!(theme.bar_background, [234, 236, 238, 255]);
        assert_eq!(theme.for_bar().text_primary, [34, 34, 34, 255]);
        assert_eq!(theme.for_bar().text_muted, [102, 102, 102, 255]);
        assert_eq!(theme.text_primary, ShellTheme::default().text_primary);
    }

    #[test]
    fn shell_opacity_is_shared_and_solid_remains_opaque() {
        let config: FereseConfig = ferese_config::from_str(
            r#"theme {
    material { style "translucent"; opacity 0.6; }
    shadow {
        soft {
            opacity 0.07
        }
    }
}
"#,
        )
        .unwrap();
        let theme = shell_theme(&config.theme);
        assert_eq!(theme.bar_background[3], 153);
        assert_eq!(theme.surface_popover[3], 153);
        assert_eq!(theme.shadow_opacity, 0.07);
        let mut config = config;
        config.theme.material.style = "solid".into();
        let solid = shell_theme(&config.theme);
        assert_eq!(solid.bar_background[3], 255);
        assert_eq!(solid.surface_popover[3], 255);
    }
}
