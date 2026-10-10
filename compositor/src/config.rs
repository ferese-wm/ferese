mod appearance;
mod bindings;
mod input_motion;
mod layout;
mod outputs;
mod xwayland;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

pub(crate) use self::xwayland::{XwaylandConfig, XwaylandStartup};
use appearance::*;
pub use appearance::{BorderGradient, FocusEffectSettings, MaterialStyle, ThemeSettings};
use bindings::*;
pub use bindings::{Binding, BindingAction};
pub(crate) use bindings::{BindingSet, physical_keymap};
use ferese_animation::SpringConfig;
use ferese_core::LayoutMode;
use ferese_layout::{ColumnWidth, Direction, GapConfig, ViewportFocusStrategy};
pub use input_motion::InputSettings;
use input_motion::*;
use outputs::*;
pub use outputs::{LidPolicy, OutputLayout, OutputModeRequest, OutputProfile, OutputSettings, OutputTransform};
use serde::Deserialize;
use smithay::input::keyboard::{Keycode, keysyms, xkb};

use crate::window_rules;
use crate::window_rules::{WindowRule, WindowRuleConfig};

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    panels: Option<Vec<ferese_config::panel::Panel>>,
    #[serde(default)]
    notifications: ferese_config::notifications::NotificationConfig,
    #[serde(default)]
    lock_screen: crate::session_lock::IdleSettings,
    #[serde(default)]
    idle_inhibit: crate::idle_inhibition::Settings,
    #[serde(default)]
    desktop_widgets: ferese_config::desktop::DesktopWidgets,
    #[serde(default)]
    pub(crate) autostart: Vec<DaemonConfig>,
    #[serde(default)]
    animations: AnimationsConfig,
    #[serde(default)]
    layout: LayoutConfig,
    #[serde(default)]
    workspaces: WorkspacesConfig,
    #[serde(default)]
    input: InputConfig,
    #[serde(default)]
    theme: ThemeConfig,
    #[serde(default)]
    appearance: AppearanceConfig,
    #[serde(default)]
    commands: HashMap<String, Vec<String>>,
    #[serde(default)]
    bindings: Vec<BindingConfig>,
    #[serde(default)]
    window_rules: Vec<WindowRuleConfig>,
    #[serde(default)]
    scrolling: ScrollingConfig,
    #[serde(default)]
    output_profiles: Vec<OutputProfileConfig>,
    #[serde(default)]
    pub(crate) xwayland: XwaylandConfig,
    #[serde(default, rename = "status")]
    _status: ShellStatusConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct DaemonConfig {
    pub command: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub restart: bool,
    #[serde(default)]
    pub nested: bool,
}

// Validate shell-only fields too, before publishing an accepted source.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields)]
struct ShellStatusConfig {
    keybinding_guide: Option<bool>,
    low_battery_threshold: Option<u8>,
    settings_command: Option<Vec<String>>,
}

impl Config {
    #[cfg(test)]
    pub(crate) fn parse_source(source: &str) -> Result<Self, ConfigError> {
        ferese_config::from_str(source).map_err(|source| ConfigError::Parse {
            path: config_path().unwrap_or_default(),
            source,
        })
    }

    pub(crate) fn resolved_theme_settings(
        theme: &ferese_config::theme::ResolvedTheme,
    ) -> Result<ThemeSettings, ConfigError> {
        let config = Self {
            theme: serde_json::from_value(serde_json::to_value(&theme.tokens).unwrap())
                .expect("resolved theme matches runtime schema"),
            ..Self::default()
        };
        config.theme_settings()
    }

    pub(crate) fn runtime_config(&self) -> Result<crate::RuntimeConfig, ConfigError> {
        if let Some(panels) = &self.panels {
            ferese_config::panel::validate(panels).map_err(ConfigError::Binding)?;
        }
        self.notifications.validate().map_err(ConfigError::Binding)?;
        self.desktop_widgets.validate().map_err(ConfigError::Binding)?;
        self.xwayland.validate().map_err(ConfigError::Binding)?;
        for daemon in &self.autostart {
            if daemon.command.first().is_none_or(|program| program.trim().is_empty()) {
                return Err(ConfigError::Binding("autostart command must contain a program".into()));
            }
        }
        let input_settings = self.input_settings()?;
        let bindings = self.bindings(&input_settings)?;
        Ok(crate::RuntimeConfig {
            lock_idle: self.lock_screen.validate()?,
            idle_inhibit: self.idle_inhibit,
            autostart: self.autostart.clone(),
            layout_mode: self.layout_mode(),
            workspace_auto_back_and_forth: self.workspaces.auto_back_and_forth,
            gap_config: self.gap_config()?,
            input_settings,
            bindings,
            window_rules: self.window_rules()?,
            theme_settings: self.theme_settings()?,
            panel_corner_radius: self
                .panels
                .as_ref()
                .and_then(|panels| panels.first())
                .and_then(|panel| panel.corner_radius),
            panel_background_opacity: self
                .panels
                .as_ref()
                .and_then(|panels| panels.first())
                .and_then(|panel| panel.background_opacity),
            focus_effect: self.focus_effect_settings()?,
            default_column_width: self.default_column_width()?,
            scrolling_focus_strategy: self.scrolling_focus_strategy(),
            column_width_presets: self.width_presets()?,
            animations_enabled: self.animations_enabled(),
            animation_speed: self.animation_speed()?,
            spring_config: self.spring_config()?,
            viewport_spring_config: self.viewport_spring_config()?,
            output_profiles: self.output_profiles()?,
            wallpaper: self.wallpaper_settings(),
            overview_font_family: self.overview_font_family(),
            xwayland: self.xwayland.clone(),
        })
    }

    pub(crate) fn overview_font_family(&self) -> String {
        self.theme
            .typography
            .font_family
            .clone()
            .unwrap_or_else(|| "sans-serif".into())
    }

    pub(crate) fn wallpaper_settings(&self) -> crate::wallpaper::WallpaperConfig {
        self.theme.background.clone()
    }
}

#[derive(Debug, Default, Deserialize)]
struct WorkspacesConfig {
    #[serde(default)]
    auto_back_and_forth: bool,
}

#[derive(Debug, Deserialize)]
struct LayoutConfig {
    mode: Option<LayoutModeValue>,
    #[serde(default = "default_inner_gap")]
    inner_gap: f64,
    #[serde(default = "default_outer_gap")]
    outer_gap: f64,
    #[serde(default)]
    smart_gaps: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            mode: None,
            inner_gap: default_inner_gap(),
            outer_gap: default_outer_gap(),
            smart_gaps: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LayoutModeValue {
    Scrolling,
    Tree,
}

#[derive(Debug, Default, Deserialize)]
struct ScrollingConfig {
    default_column_width: Option<ColumnWidthValue>,
    focus_strategy: Option<FocusStrategyValue>,
    #[serde(default)]
    width_presets: Vec<ColumnWidthValue>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FocusStrategyValue {
    Minimal,
    CenterOnFocus,
    Paged,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ColumnWidthValue {
    Proportion(f64),
    Named(String),
}

#[derive(Debug)]
pub enum ConfigError {
    #[cfg(test)]
    Parse {
        path: PathBuf,
        source: ferese_config::Error,
    },
    ColumnWidth {
        field: &'static str,
        value: String,
    },
    AnimationValue {
        field: &'static str,
        value: f64,
    },
    LayoutValue {
        field: &'static str,
        value: f64,
    },
    InputValue {
        field: &'static str,
        value: String,
    },
    Binding(String),
    WindowRule(String),
    ThemeValue {
        field: &'static str,
        value: String,
    },
    OutputProfile(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(test)]
            Self::Parse { path, source } => {
                write!(formatter, "failed to parse {}: {source}", path.display())
            }
            Self::ColumnWidth { field, value } => write!(
                formatter,
                "invalid scrolling.{field} {value}; expected a positive number or \"full\""
            ),
            Self::AnimationValue { field, value } => {
                write!(formatter, "invalid animations.{field} value {value}")
            }
            Self::LayoutValue { field, value } => {
                write!(formatter, "invalid layout.{field} value {value}")
            }
            Self::InputValue { field, value } => {
                write!(formatter, "invalid input.{field} value {value}")
            }
            Self::Binding(message) => write!(formatter, "invalid binding: {message}"),
            Self::WindowRule(message) => write!(formatter, "invalid window rule: {message}"),
            Self::ThemeValue { field, value } => {
                write!(formatter, "invalid theme.{field} value {value}")
            }
            Self::OutputProfile(message) => {
                write!(formatter, "invalid output profile: {message}")
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(test)]
            Self::Parse { source, .. } => Some(source),
            Self::ColumnWidth { .. }
            | Self::AnimationValue { .. }
            | Self::LayoutValue { .. }
            | Self::InputValue { .. }
            | Self::Binding(_)
            | Self::WindowRule(_)
            | Self::ThemeValue { .. }
            | Self::OutputProfile(_) => None,
        }
    }
}

impl Config {
    pub fn window_rules(&self) -> Result<Vec<WindowRule>, ConfigError> {
        window_rules::validate(&self.window_rules).map_err(ConfigError::WindowRule)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BindingIdentity {
    modifiers: BindingModifiers,
    match_mode: BindingMatch,
    key: u32,
}

const fn enabled_by_default() -> bool {
    true
}

const fn default_inner_gap() -> f64 {
    10.0
}

const fn default_outer_gap() -> f64 {
    4.0
}

const fn default_border_width() -> f64 {
    1.0
}

const fn default_focus_ring_width() -> f64 {
    2.0
}

const fn default_window_radius() -> f64 {
    14.0
}

const fn default_shadow_offset_y() -> f64 {
    4.0
}

const fn default_shadow_blur() -> f64 {
    18.0
}

const fn default_shadow_opacity() -> f64 {
    0.20
}

const fn default_repeat_rate() -> i32 {
    25
}

const fn default_repeat_delay() -> i32 {
    600
}

const fn default_true() -> bool {
    true
}

pub(crate) fn config_path() -> Option<PathBuf> {
    ferese_config::config_path()
}

#[cfg(test)]
mod tests {
    #[test]
    fn panel_background_opacity_override_leaves_the_shell_material_unchanged() {
        let config = Config::parse_source(
            "theme { material { style translucent; opacity 0.3; }; }; panel main { background-opacity 0.8; }",
        )
        .unwrap();
        let runtime = config.runtime_config().unwrap();
        assert_eq!(runtime.panel_background_opacity, Some(0.8));
        assert!((runtime.theme_settings.shell_opacity - 0.3).abs() < 0.001);
        let inherited = Config::parse_source("panel main;").unwrap().runtime_config().unwrap();
        assert!(inherited.panel_background_opacity.is_none());
    }

    #[test]
    fn panel_corner_override_does_not_replace_shell_radius() {
        let config = Config::parse_source(
            "theme { geometry { shell-radius 20; }; }; panel \"main\" { corner-radius \"4px 8px 12px 16px\"; }",
        )
        .unwrap();
        let runtime = config.runtime_config().unwrap();
        assert_eq!(runtime.panel_corner_radius.unwrap().0, [4., 8., 12., 16.]);
        assert_eq!(runtime.theme_settings.material_radius, 20.);
        let inherited = Config::parse_source("theme { geometry { shell-radius 20; }; }")
            .unwrap()
            .runtime_config()
            .unwrap();
        assert!(inherited.panel_corner_radius.is_none());
        assert_eq!(inherited.theme_settings.panel_radius, 20.);
    }

    #[test]
    fn validates_shell_composition_before_publishing_config() {
        let source = r#"panel "main" { end { group "status" { item "network" kind="network"; item "clock" kind="clock"; }; }; }"#;
        assert!(Config::parse_source(source).unwrap().runtime_config().is_ok());
        assert!(
            Config::parse_source(&source.replace("item \"clock\"", "item \"network\""))
                .unwrap()
                .runtime_config()
                .is_err()
        );
        assert!(
            Config::parse_source("panel \"one\"; panel \"two\";")
                .unwrap()
                .runtime_config()
                .is_err()
        );
        assert!(Config::parse_source("panel \"one\" { edge \"left\"; }").is_err());
    }

    #[test]
    fn panel_surfaces_are_validated_before_live_publication() {
        for surface in ["none", "inset", "island"] {
            let source = format!(
                "panel main {{ surface solid; group-surface {surface}; end {{ group status {{ surface {surface}; }}; }}; }}"
            );
            Config::parse_source(&source).unwrap().runtime_config().unwrap();
        }
        assert!(Config::parse_source("panel main { group-surface invalid; }").is_err());
        assert!(Config::parse_source("panel main { background islands; }").is_err());
        assert!(Config::parse_source("status { bar-layout islands; }").is_err());
    }

    #[test]
    fn group_padding_is_validated_before_live_publication() {
        for padding in ["0", "4.5", "12", "32"] {
            let source = format!("panel main {{ end {{ group status {{ island-padding {padding}; }}; }}; }}");
            Config::parse_source(&source).unwrap().runtime_config().unwrap();
        }
        for padding in ["-1", "32.5", "\"small\""] {
            let source = format!("panel main {{ end {{ group status {{ island-padding {padding}; }}; }}; }}");
            assert!(
                Config::parse_source(&source)
                    .map(|config| config.runtime_config().is_err())
                    .unwrap_or(true)
            );
        }
    }

    #[test]
    fn rejects_removed_physics_keys_before_publication() {
        for property in ["spring", "viewport-spring"] {
            for coefficient in ["mass", "stiffness", "damping", "damping-ratio"] {
                let source = format!("animations {{ {property} {{ {coefficient} 0; overshoot #true; }}; }}");
                let error = Config::parse_source(&source).unwrap_err().to_string();
                assert!(error.contains("unknown field"), "{error}");
            }
        }
    }

    use super::*;

    #[test]
    fn display_key_has_a_default_binding_and_can_be_rebound() {
        let bindings = parse("").runtime_config().unwrap().bindings;
        assert!(bindings.iter().any(
            |binding| binding.trigger == BindingTrigger::Keysym(keysyms::KEY_XF86Display)
                && binding.action == BindingAction::ToggleDisplayMode
        ));
        let bindings = parse(
            "binding keys=\"XF86Display\" disabled=#true\nbinding keys=\"Super+P\" action=\"toggle-display-mode\"\n",
        )
        .runtime_config()
        .unwrap()
        .bindings;
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.trigger == BindingTrigger::Keysym(keysyms::KEY_XF86Display))
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.modifiers.logo && binding.action == BindingAction::ToggleDisplayMode)
        );
    }

    #[test]
    fn shortcut_hint_has_a_default_binding_and_can_be_rebound() {
        let bindings = parse("").runtime_config().unwrap().bindings;
        assert!(bindings.iter().any(|binding| binding.modifiers.logo
            && binding.trigger == BindingTrigger::Keysym(keysyms::KEY_F1)
            && binding.action == BindingAction::ToggleKeybindingGuide));
        let bindings = parse(
            "binding keys=\"Super+F1\" disabled=#true\nbinding keys=\"Super+F2\" action=\"toggle-keybinding-guide\"\n",
        )
        .runtime_config()
        .unwrap()
        .bindings;
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.trigger == BindingTrigger::Keysym(keysyms::KEY_F1))
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.trigger == BindingTrigger::Keysym(keysyms::KEY_F2)
                    && binding.action == BindingAction::ToggleKeybindingGuide)
        );
    }
    #[test]
    fn workspace_back_and_forth_config_is_optional_and_bindable() {
        let defaults = Config::default().runtime_config().unwrap();
        assert!(!defaults.workspace_auto_back_and_forth);
        assert!(
            defaults
                .bindings
                .iter()
                .any(|binding| binding.action == BindingAction::WorkspaceBackAndForth)
        );
        let configured = parse(
            "workspaces {\n auto-back-and-forth #true\n}\nbinding \"Super+BackSpace\" \"workspace-back-and-forth\"\n",
        )
        .runtime_config()
        .unwrap();
        assert!(configured.workspace_auto_back_and_forth);
        assert_eq!(
            configured
                .bindings
                .iter()
                .filter(|binding| binding.action == BindingAction::WorkspaceBackAndForth)
                .count(),
            2
        );
        assert!(parse_action("workspace-back-and-forth", Some("2"), &HashMap::new()).is_err());
        assert!(Config::parse_source("workspaces {\n auto-back-and-forth \"yes\"\n}\n").is_err());
    }

    #[test]
    fn focus_floating_is_bound_to_super_v_and_accepts_a_custom_binding() {
        for source in ["", "binding \"Super+V\" \"focus-floating\"\n"] {
            let configured = parse(source).runtime_config().unwrap();
            assert!(configured.bindings.iter().any(|binding| {
                binding.action == BindingAction::FocusFloating
                    && binding.matches(55_u32.into(), &[keysyms::KEY_v], true, false, false, false)
            }));
        }
    }

    #[test]
    fn focus_history_actions_are_bindable_and_have_defaults() {
        let defaults = Config::default().runtime_config().unwrap();
        for action in [
            BindingAction::FocusLastWindow,
            BindingAction::FocusFloating,
            BindingAction::FocusMru(false),
            BindingAction::FocusMru(true),
        ] {
            assert!(defaults.bindings.iter().any(|binding| binding.action == action));
        }
        for (name, action) in [
            ("focus-last-window", BindingAction::FocusLastWindow),
            ("focus-floating", BindingAction::FocusFloating),
            ("focus-mru-next", BindingAction::FocusMru(false)),
            ("focus-mru-previous", BindingAction::FocusMru(true)),
        ] {
            assert_eq!(parse_action(name, None, &HashMap::new()).unwrap(), action);
            assert!(parse_action(name, Some("left"), &HashMap::new()).is_err());
        }
        let bindings = &defaults.bindings;
        assert!(bindings.iter().any(|binding| binding.matches(
            23_u32.into(),
            &[keysyms::KEY_Tab],
            false,
            false,
            true,
            false
        ) && binding.action == BindingAction::FocusMru(false)));
        assert!(bindings.iter().any(|binding| binding.matches(
            23_u32.into(),
            &[keysyms::KEY_Tab],
            false,
            false,
            true,
            true
        ) && binding.action == BindingAction::FocusMru(true)));
    }

    #[test]
    fn packaged_and_custom_kdl_pass_runtime_validation() {
        let packaged = Config::parse_source(include_str!("../../packaging/config.kdl")).unwrap();
        packaged.runtime_config().unwrap();

        assert!(
            packaged.xwayland.enabled,
            "packaging/config.kdl must ship X11 support enabled, matching the default"
        );
        let source = "input {\n    touchpad {\n        swipe-threshold 96\n    }\n}\nbinding keys=\"Swipe3Up\" action=\"toggle-overview\"\noutput-profile name=\"desk\" {\n    output match=\"DP-1\" scale=1.5 {\n        position 0 0\n    }\n}\ndesktop-widgets {\n    clock {\n        enabled #true\n        outputs \"DP-1\"\n    }\n}\nautostart {\n    command \"program\" \"argument with space\"\n}\n";
        let config = Config::parse_source(source).unwrap();
        config.runtime_config().unwrap();
        assert_eq!(config.input_settings().unwrap().touchpad.swipe_threshold, 96);
        assert_eq!(config.desktop_widgets.clock.outputs, ["DP-1"]);
        assert_eq!(config.output_profiles().unwrap()[0].outputs[0].position, Some([0, 0]));
    }

    #[test]
    fn desktop_clock_settings_are_validated_before_live_publication() {
        assert!(
            parse(
                "desktop-widgets {\n    clock {\n        enabled #true\n        anchor \"center\"\n        time-format \"%H:%M\"\n    }\n}\n"
            )
            .runtime_config()
            .is_ok()
        );
        for source in [
            "desktop-widgets {\n    clock {\n        width 8192\n    }\n}\n",
            "desktop-widgets {\n    clock {\n        opacity #nan\n    }\n}\n",
            "desktop-widgets {\n    clock {\n        time-format \"%\"\n    }\n}\n",
            "desktop-widgets {\n    clock {\n        time-zone \"invalid/zone\"\n    }\n}\n",
        ] {
            assert!(
                Config::parse_source(source)
                    .and_then(|config| config.runtime_config())
                    .is_err()
            );
        }
        assert!(Config::parse_source("desktop-widgets { clock { anchor \"wrong\"; }; }").is_err());
    }

    fn parse(source: &str) -> Config {
        ferese_config::from_str(source).unwrap()
    }

    #[test]
    fn an_empty_configuration_enables_the_managed_x11_service() {
        let config = parse("");

        assert_eq!(config.xwayland, XwaylandConfig::default());
        assert!(config.xwayland.enabled, "an empty config keeps the default");
        assert_eq!(config.xwayland.startup, XwaylandStartup::OnDemand);
        let runtime = config.runtime_config().expect("an empty config is valid");
        assert!(runtime.xwayland.enabled);
    }

    #[test]
    fn a_configuration_without_an_xwayland_section_keeps_the_defaults() {
        let source = r#"
            layout-mode "scrolling";
            desktop-widgets {
                clock {
                    time-format "%H:%M";
                };
            }
            scroll-factor 1.0;
        "#;
        let config = parse(source);

        assert!(
            source.find("xwayland").is_none(),
            "the sample must have no xwayland node"
        );
        assert_eq!(config.xwayland, XwaylandConfig::default());
        assert!(config.xwayland.enabled);
        let runtime = config.runtime_config().expect("the existing config stays valid");
        assert!(runtime.xwayland.enabled);
    }

    #[test]
    fn defaults_to_half_width_scrolling_columns() {
        let config = parse("");

        assert_eq!(config.layout_mode(), LayoutMode::Scrolling);
        assert_eq!(config.default_column_width().unwrap(), ColumnWidth::Proportion(0.5));
        assert_eq!(config.gap_config().unwrap(), GapConfig::default());
    }

    #[test]
    fn accepts_full_and_numeric_column_widths() {
        let full = parse("scrolling {\n    default-column-width \"full\"\n}\n");
        let numeric = parse("scrolling {\n    default-column-width 1.0\n}\n");

        assert_eq!(full.default_column_width().unwrap(), ColumnWidth::Full);
        assert_eq!(numeric.default_column_width().unwrap(), ColumnWidth::Proportion(1.0));
    }

    #[test]
    fn parses_width_presets_and_supplies_defaults() {
        let defaults = parse("");
        let configured = parse("scrolling {\n    width-presets 0.5 1.0 \"full\"\n}\n");

        assert_eq!(defaults.width_presets().unwrap().len(), 4);
        assert_eq!(
            configured.width_presets().unwrap(),
            vec![
                ColumnWidth::Proportion(0.5),
                ColumnWidth::Proportion(1.0),
                ColumnWidth::Full
            ]
        );
    }

    #[test]
    fn parses_scrolling_focus_strategy() {
        let minimal = parse("");
        let centered = parse("scrolling {\n    focus-strategy \"center_on_focus\"\n}\n");
        let paged = parse("scrolling {\n    focus-strategy \"paged\"\n}\n");
        assert_eq!(paged.scrolling_focus_strategy(), ViewportFocusStrategy::Paged);

        assert_eq!(minimal.scrolling_focus_strategy(), ViewportFocusStrategy::Minimal);
        assert_eq!(centered.scrolling_focus_strategy(), ViewportFocusStrategy::Center);
    }

    #[test]
    fn rejects_non_positive_or_unknown_column_widths() {
        let zero = parse("scrolling {\n    default-column-width 0.0\n}\n");
        let unknown = parse("scrolling {\n    default-column-width \"wide\"\n}\n");

        assert!(zero.default_column_width().is_err());
        assert!(unknown.default_column_width().is_err());
    }

    #[test]
    fn parses_animation_policy_and_reduced_motion() {
        let config = parse(
            "animations {\n    speed 1.5\n    reduced-motion #true\n    spring {\n        duration-ms 300.0\n        bounce 0.0\n    }\n}\n",
        );

        assert!(!config.animations_enabled());
        assert_eq!(config.animation_speed().unwrap(), 1.5);
        assert_eq!(
            config.spring_config().unwrap(),
            SpringConfig {
                mass: 1.0,
                stiffness: (std::f64::consts::TAU / 0.3).powi(2),
                damping: 2.0 * std::f64::consts::TAU / 0.3,
                ..SpringConfig::default()
            }
        );
    }

    #[test]
    fn rejects_invalid_animation_numbers() {
        let speed = parse("animations {\n    speed 0.0\n}\n");
        let damping = parse("animations {\n    spring {\n        duration-ms -1.0\n    }\n}\n");

        assert!(speed.animation_speed().is_err());
        assert!(damping.spring_config().is_err());
    }

    #[test]
    fn parses_and_validates_layout_gaps() {
        let configured = parse("layout {\n    inner-gap 6.0\n    outer-gap 14.0\n    smart-gaps #true\n}\n");
        let invalid = parse("layout {\n    outer-gap -1.0\n}\n");

        assert_eq!(
            configured.gap_config().unwrap(),
            GapConfig {
                inner: 6.0,
                outer: 14.0,
                smart: true,
            }
        );
        assert!(invalid.gap_config().is_err());
    }

    #[test]
    fn shell_radius_is_independent_of_windows() {
        for radius in [0.0, 18.0] {
            let config = parse(&format!(
                "theme {{\n geometry {{\n shell-radius {radius}\n window-radius 23\n }}\n}}"
            ));
            let theme = config.theme_settings().unwrap();
            assert_eq!(theme.material_radius, radius);
            assert_eq!(theme.panel_radius, radius);
            assert_eq!(theme.window_radius, 23.0);
        }
        assert!(
            parse("theme {\n geometry {\n shell-radius -1\n }\n}")
                .theme_settings()
                .is_err()
        );
    }

    #[test]
    fn parses_and_validates_theme_window_tokens() {
        let configured = parse(
            "theme {\n    colors {\n        border \"#11223344\"\n        accent \"#AABBCC\"\n        shadow \"#01020380\"\n    }\n    geometry {\n        border-width 1.5\n        focus-ring-width 3.0\n        window-radius 12.0\n    }\n    shadow {\n        soft {\n            offset-y -2.0\n            blur 24.0\n            opacity 0.4\n        }\n    }\n    material {\n        style \"translucent\"\n    }\n}\n",
        );
        let invalid_color = parse("theme {\n    colors {\n        accent \"blue\"\n    }\n}\n");
        let invalid_radius = parse("theme {\n    geometry {\n        window-radius -1.0\n    }\n}\n");
        let invalid_opacity =
            parse("theme {\n    shadow {\n        soft {\n            opacity 1.1\n        }\n    }\n}\n");

        assert_eq!(
            configured.theme_settings().unwrap(),
            ThemeSettings {
                border_width: 1.5,
                focus_ring_width: 3.0,
                border_color: RgbaColor([17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 68.0 / 255.0,]),
                accent_color: RgbaColor([170.0 / 255.0, 187.0 / 255.0, 0.8, 1.0]),
                border_gradient: None,
                focus_ring_gradient: None,
                shadow_color: RgbaColor([1.0 / 255.0, 2.0 / 255.0, 3.0 / 255.0, 128.0 / 255.0]),
                surface_base_color: RgbaColor([17.0 / 255.0, 24.0 / 255.0, 33.0 / 255.0, 1.0,]),
                bar_background_color: RgbaColor([17.0 / 255.0, 24.0 / 255.0, 33.0 / 255.0, 1.0,]),
                text_primary_color: RgbaColor([244.0 / 255.0, 247.0 / 255.0, 251.0 / 255.0, 1.0]),
                shell_opacity: 0.78,
                window_radius: 12.0,
                shadow_offset_y: -2.0,
                shadow_blur: 24.0,
                shadow_opacity: 0.4,
                material_style: MaterialStyle::Translucent,
                backdrop_blur: 12.0,
                material_tint_strength: 0.5,
                material_radius: 14.0,
                panel_radius: 14.0,
            }
        );
        assert!(invalid_color.theme_settings().is_err());
        assert!(invalid_radius.theme_settings().is_err());
        assert!(invalid_opacity.theme_settings().is_err());
        assert!(
            ferese_config::from_str::<Config>("theme {\n    material {\n        style \"mist\"\n    }\n}\n").is_err()
        );
    }

    #[test]
    fn materials_default_to_solid_and_reject_removed_style() {
        assert_eq!(parse("").theme_settings().unwrap().material_style, MaterialStyle::Solid);
        assert!(
            ferese_config::from_str::<Config>("theme {\n    material {\n        style \"glass\"\n    }\n}\n").is_err()
        );
    }

    #[test]
    fn border_gradients_are_optional_and_independent() {
        let defaults = parse("").theme_settings().unwrap();
        assert_eq!(defaults.border_gradient, None);
        assert_eq!(defaults.focus_ring_gradient, None);
        let configured =
            parse("theme {\n    focus-ring {\n        gradient {\n            from \"#e5c890\"\n            to \"#b98d5880\"\n            angle -45\n        }\n    }\n}\n")
                .theme_settings()
                .unwrap();
        let gradient = configured.focus_ring_gradient.unwrap();
        assert_eq!(configured.border_gradient, None);
        assert_eq!(gradient.angle, 315.0);
        assert_eq!(gradient.to.0[3], 128.0 / 255.0);
        let border = parse("theme {\n    border {\n        gradient {\n            from \"#112233\"\n            to \"#445566\"\n        }\n    }\n}\n")
            .theme_settings()
            .unwrap();
        assert_eq!(border.border_gradient.unwrap().angle, 0.0);
        assert_eq!(border.focus_ring_gradient, None);
        assert_eq!(configured.border_color, defaults.border_color);
        assert_eq!(configured.accent_color, defaults.accent_color);
    }

    #[test]
    fn resolved_paint_styles_reach_window_decorations() {
        let mut theme = ferese_config::theme::default_theme();
        let settings = Config::resolved_theme_settings(&theme).unwrap();
        assert!(settings.border_gradient.is_some());
        assert!(settings.focus_ring_gradient.is_some());
        theme.tokens.focus_ring.style = ferese_config::theme::PaintStyle::Solid;
        let settings = Config::resolved_theme_settings(&theme).unwrap();
        assert!(settings.focus_ring_gradient.is_none());
        assert!(settings.border_gradient.is_some());
    }

    #[test]
    fn border_gradients_reject_invalid_colors_and_nonfinite_angles() {
        for settings in [
            "from \"invalid\"\nto \"#445566\"\n",
            "from \"#112233\"\nto \"invalid\"\n",
            "from \"#112233\"\nto \"#445566\"\nangle #nan\n",
            "from \"#112233\"\nto \"#445566\"\nangle #inf\n",
        ] {
            assert!(
                Config::parse_source(&format!("theme {{ focus-ring {{ gradient {{\n{settings}\n}} }} }}"))
                    .and_then(|config| config.theme_settings())
                    .is_err()
            );
        }
        assert!(
            ferese_config::from_str::<Config>(
                "theme {\n    border {\n        gradient {\n            from \"#112233\"\n        }\n    }\n}\n"
            )
            .is_err()
        );
    }

    #[test]
    fn material_color_comes_from_surface_base() {
        let theme = parse("theme {\n    colors {\n        surface-base \"#000000\"\n    }\n    surface {\n        bar {\n            background \"#FFFFFF\"\n        }\n    }\n}\n")
            .theme_settings()
            .unwrap();
        assert_eq!(theme.surface_base_color, RgbaColor([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(theme.bar_background_color, RgbaColor([1.0, 1.0, 1.0, 1.0]));
    }

    #[test]
    fn shell_opacity_uses_config_and_rejects_invalid_values() {
        assert_eq!(parse("").theme_settings().unwrap().shell_opacity, 0.78);
        for opacity in [0.0, 0.65, 1.0] {
            assert_eq!(
                parse(&format!("theme {{ material {{ opacity {opacity}; }} }}"))
                    .theme_settings()
                    .unwrap()
                    .shell_opacity,
                opacity
            );
        }
        for opacity in ["-0.1", "1.1", "#nan", "#inf"] {
            assert!(
                Config::parse_source(&format!("theme {{ material {{ opacity {opacity}; }} }}"))
                    .and_then(|config| config.theme_settings())
                    .is_err()
            );
        }
    }

    #[test]
    fn backdrop_blur_is_configurable_without_glass_settings() {
        assert_eq!(parse("").theme_settings().unwrap().backdrop_blur, 12.0);
        assert_eq!(
            parse("theme {\n    material {\n        style \"translucent\"\n        blur-radius 100\n    }\n}\n")
                .theme_settings()
                .unwrap()
                .backdrop_blur,
            32.0
        );
        assert_eq!(
            parse("theme {\n    material {\n        blur-radius 0\n    }\n}\n")
                .theme_settings()
                .unwrap()
                .backdrop_blur,
            0.0
        );
        for value in ["-1", "#nan", "#inf"] {
            assert!(
                Config::parse_source(&format!("theme {{ material {{ blur-radius {value}; }} }}"))
                    .and_then(|config| config.theme_settings())
                    .is_err()
            );
        }
    }

    #[test]
    fn focus_effect_defaults_and_validation() {
        let defaults = parse("").focus_effect_settings().unwrap();
        assert!(defaults.enabled);
        assert_eq!(defaults.active_opacity, 1.0);
        assert_eq!(defaults.inactive_opacity, 1.0);
        assert_eq!(defaults.inactive_dim, 0.0);
        assert_eq!(defaults.duration_ms, 150.0);
        let settings = parse("appearance { focus-effect { active-opacity 0.9; inactive-opacity 0.8; inactive-dim 0.25; duration-ms 100; }; }")
            .focus_effect_settings().unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.active_opacity, 0.9);
        assert_eq!(settings.inactive_opacity, 0.8);
        assert_eq!(settings.inactive_dim, 0.25);
        assert_eq!(settings.duration_ms, 100.0);
        for key in ["active-opacity", "inactive-opacity", "inactive-dim"] {
            for value in ["-0.1", "1.1", "#nan", "#inf"] {
                assert!(
                    Config::parse_source(&format!("appearance {{ focus-effect {{ {key} {value}; }} }}"))
                        .and_then(|config| config.runtime_config())
                        .is_err()
                );
            }
        }
        for value in ["-1", "#nan", "#inf"] {
            assert!(
                Config::parse_source(&format!("appearance {{ focus-effect {{ duration-ms {value}; }} }}"))
                    .and_then(|config| config.runtime_config())
                    .is_err()
            );
        }
        let disabled = parse("appearance { focus-effect { enabled #false; active-opacity 0.9; inactive-opacity 0.8; inactive-dim 0.25; }; }")
            .focus_effect_settings().unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.active_opacity, 0.9);
        assert_eq!(disabled.inactive_opacity, 0.8);
        assert_eq!(disabled.inactive_dim, 0.25);
        assert!(Config::parse_source("appearance { inactive-dim { enabled #true; }; }").is_err());
    }

    #[test]
    fn parses_input_and_touchpad_settings() {
        let config = parse(
            "input {\n    focus-follows-mouse #true\n    xkb-layout \"us,de\"\n    xkb-variant \",nodeadkeys\"\n    xkb-options \"grp:alt_shift_toggle\"\n    repeat-rate 30\n    repeat-delay-ms 450\n    touchpad {\n        tap #false\n        natural-scroll #false\n        disable-while-typing #true\n    }\n}\n",
        );

        assert_eq!(
            config.input_settings().unwrap(),
            InputSettings {
                focus_follows_mouse: true,
                xkb_layout: "us,de".to_owned(),
                xkb_variant: ",nodeadkeys".to_owned(),
                xkb_options: vec!["grp:alt_shift_toggle".to_owned()],
                repeat_rate: 30,
                repeat_delay_ms: 450,
                touchpad: TouchpadSettings {
                    tap: false,
                    natural_scroll: false,
                    disable_while_typing: true,
                    swipe_threshold: 80,
                },
            }
        );
    }

    #[test]
    fn rejects_invalid_input_settings() {
        let layout = parse("input {\n    xkb-layout \"\"\n}\n");
        let rate = parse("input {\n    repeat-rate 0\n}\n");
        let delay = parse("input {\n    repeat-delay-ms -1\n}\n");

        assert!(layout.input_settings().is_err());
        assert!(rate.input_settings().is_err());
        assert!(delay.input_settings().is_err());
    }

    #[test]
    fn swipe_bindings_can_override_actions_disable_defaults_and_set_distance() {
        use crate::gestures::SwipeDirection;
        let config = parse(
            "input {\n    touchpad {\n        swipe-threshold 120\n    }\n}\nbinding keys=\"Swipe3Up\" action=\"toggle-overview\"\nbinding keys=\"Swipe3Left\" action=\"move\" argument=\"left\"\nbinding keys=\"Swipe3Down\" disabled=#true\n",
        );
        let input = config.input_settings().unwrap();
        assert_eq!(input.touchpad.swipe_threshold, 120);
        let bindings = config.bindings(&input).unwrap();
        assert!(
            bindings
                .iter()
                .any(|binding| binding.matches_swipe(3, SwipeDirection::Up)
                    && binding.action == BindingAction::ToggleOverview)
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.matches_swipe(3, SwipeDirection::Left)
                    && binding.action == BindingAction::Move(ferese_layout::Direction::Left))
        );
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.matches_swipe(3, SwipeDirection::Down))
        );
        for threshold in [0, 15, 1001] {
            assert!(
                parse(&format!("input {{ touchpad {{ swipe-threshold {threshold}; }} }}"))
                    .input_settings()
                    .is_err()
            );
        }
    }

    #[test]
    fn gesture_bindings_validate_fingers_directions_and_actions() {
        for keys in ["Swipe2Up", "Swipe3Diagonal", "Super+Swipe3Up"] {
            let config = parse(&format!("binding \"{keys}\" \"toggle-overview\""));
            assert!(config.bindings(&config.input_settings().unwrap()).is_err());
        }
        let config = parse("binding keys=\"Swipe4Up\" action=\"toggle-overview\"\n");
        let bindings = config.bindings(&config.input_settings().unwrap()).unwrap();
        assert!(
            bindings
                .iter()
                .any(|binding| binding.matches_swipe(4, crate::gestures::SwipeDirection::Up))
        );
    }

    #[test]
    fn guide_uses_effective_bindings_including_overrides_and_unbindings() {
        let config =
            parse("binding keys=\"Super+Q\" action=\"none\"\nbinding keys=\"Super+Tab\" action=\"toggle-floating\"\n");
        let input = config.input_settings().unwrap();
        let map = physical_keymap(&input).unwrap();
        let entries = config
            .bindings(&input)
            .unwrap()
            .iter()
            .filter_map(|binding| binding.guide_entry(Some(&map)))
            .collect::<Vec<_>>();
        assert!(!entries.iter().any(|entry| entry["description"] == "Close window"));
        assert!(
            entries
                .iter()
                .any(|entry| entry["keys"] == "Super + Tab" && entry["description"] == "Toggle floating window")
        );
        assert!(entries.iter().any(|entry| entry["keys"] == "Super + Enter"));
    }

    #[test]
    fn native_media_keys_share_the_selected_player_and_reject_arguments() {
        let defaults = Config::default().runtime_config().unwrap();
        for (key, name, action) in [
            (keysyms::KEY_XF86AudioPlay, "media-play-pause", "play-pause"),
            (keysyms::KEY_XF86AudioPause, "media-play-pause", "play-pause"),
            (keysyms::KEY_XF86AudioNext, "media-next", "next"),
            (keysyms::KEY_XF86AudioPrev, "media-previous", "previous"),
        ] {
            assert!(
                defaults
                    .bindings
                    .iter()
                    .any(|binding| binding.trigger == BindingTrigger::Keysym(key)
                        && binding.action == BindingAction::Media(action))
            );
            assert_eq!(
                parse_action(name, None, &HashMap::new()).unwrap(),
                BindingAction::Media(action)
            );
            assert!(parse_action(name, Some("unexpected"), &HashMap::new()).is_err());
        }
    }

    #[test]
    fn supplies_complete_v0_bindings_and_terminal_command() {
        let config = parse("");
        let input = config.input_settings().unwrap();
        let bindings = config.bindings(&input).unwrap();

        assert_eq!(bindings.len(), 59);
        for (shift, action) in [
            (false, BindingAction::ToggleMaximized),
            (true, BindingAction::ToggleFullscreen),
        ] {
            assert!(bindings.iter().any(|binding| binding.modifiers.logo
                && binding.modifiers.shift == shift
                && binding.trigger == BindingTrigger::Keysym(keysyms::KEY_f)
                && binding.action == action));
        }
        assert!(bindings.iter().any(|binding| binding.action == BindingAction::Exit));
        assert!(bindings.iter().any(|binding| {
            binding.modifiers.logo
                && binding.trigger == BindingTrigger::Keysym(keysyms::KEY_Return)
                && binding.action == BindingAction::Spawn(vec!["foot".to_owned()].into())
        }));
        assert!(bindings.iter().any(|binding| {
            binding.modifiers.logo
                && binding.modifiers.shift
                && binding.trigger == BindingTrigger::Keysym(keysyms::KEY_s)
                && binding.action == BindingAction::Spawn(vec!["ferese-screenshot".to_owned()].into())
        }));
        assert!(bindings.iter().any(|binding| {
            binding.trigger == BindingTrigger::Keysym(keysyms::KEY_Print)
                && binding.modifiers == BindingModifiers::default()
                && binding.action
                    == BindingAction::Spawn(vec!["ferese-screenshot".to_owned(), "--full".to_owned()].into())
        }));
        assert!(bindings.iter().any(|binding| {
            binding.modifiers.logo
                && binding.trigger == BindingTrigger::Keysym(keysyms::KEY_Tab)
                && binding.action == BindingAction::ToggleOverview
        }));
    }

    #[test]
    fn replaces_and_unbinds_default_bindings() {
        let replaced = parse(
            "commands {\n    term \"foot\" \"--app-id\" \"work\"\n}\nbinding keys=\"Super+Enter\" action=\"spawn\" argument=\"term\"\n",
        );
        let unbound = parse("binding keys=\"Super+Q\" disabled=#true\n");

        let input = replaced.input_settings().unwrap();
        let bindings = replaced.bindings(&input).unwrap();
        assert_eq!(bindings.len(), 59);
        assert!(bindings.iter().any(|binding| {
            binding.action
                == BindingAction::Spawn(vec!["foot".to_owned(), "--app-id".to_owned(), "work".to_owned()].into())
        }));

        let input = unbound.input_settings().unwrap();
        let bindings = unbound.bindings(&input).unwrap();
        assert_eq!(bindings.len(), 58);
        assert!(!bindings.iter().any(|binding| binding.action == BindingAction::Close));
    }

    #[test]
    fn accepts_physical_xkb_key_names() {
        let config = parse("binding keys=\"Super+AD06\" match=\"physical\" action=\"focus\" argument=\"left\"\n");
        let input = config.input_settings().unwrap();
        let bindings = config.bindings(&input).unwrap();

        assert!(bindings.iter().any(|binding| {
            matches!(binding.trigger, BindingTrigger::Physical(_))
                && binding.action == BindingAction::Focus(Direction::Left)
        }));
    }

    #[test]
    fn rejects_duplicate_or_invalid_user_bindings() {
        let duplicate = parse("binding keys=\"Super+Q\" action=\"close\"\nbinding keys=\"logo+q\" action=\"close\"\n");
        let missing_command = parse("binding keys=\"Super+Enter\" action=\"spawn\" argument=\"missing\"\n");
        let invalid_argument = parse("binding keys=\"Super+Q\" action=\"close\" argument=\"left\"\n");

        for config in [duplicate, missing_command, invalid_argument] {
            let input = config.input_settings().unwrap();
            assert!(config.bindings(&input).is_err());
        }
    }

    #[test]
    fn parses_and_validates_window_rules() {
        let config = parse(
            "window-rule app-id=\"org.example.Editor\" workspace=3 floating=#true width=900.0 height=600.0 min-width=500 min-height=400 fullscreen=#false block-out-from-screencasts=#true\n",
        );
        let rules = config.window_rules().unwrap();
        let result = window_rules::resolve(&rules, Some("org.example.editor.desktop"), Some("Document"), false);

        assert_eq!(result.workspace, Some(3));
        assert_eq!(result.floating, Some(true));
        assert_eq!(result.width, Some(900.0));
        assert_eq!(result.height, Some(600.0));
        assert_eq!(result.min_width, Some(500.0));
        assert_eq!(result.min_height, Some(400.0));
        assert_eq!(result.fullscreen, Some(false));
        assert_eq!(result.block_out_from_screencasts, Some(true));
    }

    #[test]
    fn rejects_invalid_window_rule_configuration() {
        let catch_all = parse("window-rule floating=#true\n");
        let zero_workspace = parse("window-rule app-id=\"editor\" workspace=0\n");
        let zero_minimum = parse("window-rule app-id=\"editor\" min-width=0\n");

        assert!(catch_all.window_rules().is_err());
        assert!(zero_workspace.window_rules().is_err());
        assert!(zero_minimum.window_rules().is_err());
    }

    #[test]
    fn derives_critical_viewport_damping() {
        let config = parse("");
        let spring = config.viewport_spring_config().unwrap();

        assert_eq!(spring.mass, 1.0);
        assert!((spring.stiffness - (std::f64::consts::TAU / 0.350).powi(2)).abs() < 1e-9);
        assert!((spring.damping - 2.0 * spring.stiffness.sqrt()).abs() < 1e-9);
    }

    #[test]
    fn parses_output_profiles() {
        let config = parse(
            "output-profile name=\"docked\" {\n    output match=\"HDMI-A-1\" mode=\"3840x2160@119.998\" scale=1.6 {\n        position 0 0\n    }\n    output match=\"eDP-1\" enabled=#false transform=\"rotate_90\"\n}\n",
        );

        assert_eq!(
            config.output_profiles().unwrap(),
            vec![OutputProfile {
                name: "docked".to_owned(),
                layout: OutputLayout::Extend,
                confirm_timeout: 15,
                lid_policy: LidPolicy::DockOrSuspend,
                lid_closed: None,
                mirror_source: None,
                outputs: vec![
                    OutputSettings {
                        auto_refresh: false,
                        required: true,
                        matcher: "HDMI-A-1".to_owned(),
                        enabled: true,
                        mode: Some(OutputModeRequest {
                            width: 3840,
                            height: 2160,
                            refresh_millihertz: Some(119_998),
                        }),
                        scale: 1.6,
                        transform: OutputTransform::Normal,
                        position: Some([0, 0]),
                    },
                    OutputSettings {
                        auto_refresh: false,
                        required: true,
                        matcher: "eDP-1".to_owned(),
                        enabled: false,
                        mode: None,
                        scale: 1.0,
                        transform: OutputTransform::Rotate90,
                        position: None,
                    },
                ],
            }]
        );
    }

    #[test]
    fn output_profile_metadata_and_invalid_mirrors() {
        let config = parse(
            "output-profile desk layout=\"mirror\" mirror-source=\"eDP-1\" lid-closed=#false confirm-timeout=0 lid-policy=\"ignore\" { output eDP-1; output DP-1 required=#false; }",
        );
        let profile = &config.output_profiles().unwrap()[0];
        assert_eq!(profile.layout, OutputLayout::Mirror);
        assert_eq!(profile.lid_closed, Some(false));
        assert_eq!(profile.confirm_timeout, 0);
        assert_eq!(profile.lid_policy, LidPolicy::Ignore);
        assert!(!profile.outputs[1].required);
        for source in [
            "output-profile a layout=\"mirror\" { output eDP-1; }",
            "output-profile a layout=\"extend\" mirror-source=\"eDP-1\" { output eDP-1; }",
            "output-profile a layout=\"mirror\" mirror-source=\"DP-2\" { output eDP-1; output DP-1; }",
            "output-profile a { output eDP-1 enabled=#false; }",
            "output-profile a { output eDP-1; }\noutput-profile a { output DP-1; }",
        ] {
            assert!(parse(source).output_profiles().is_err(), "{source}");
        }
        assert!(Config::parse_source("output-profile a layout=\"docked\" { output eDP-1; }").is_err());
    }

    #[test]
    fn automatic_refresh_is_opt_in_and_rejects_wrong_types() {
        let config = parse("output-profile laptop { output eDP-1 mode=\"2880x1800@120\" auto-refresh=#true; }");
        assert!(config.output_profiles().unwrap()[0].outputs[0].auto_refresh);
        let manual = parse("output-profile laptop { output eDP-1 mode=\"2880x1800@60\"; }");
        assert!(!manual.output_profiles().unwrap()[0].outputs[0].auto_refresh);
        assert!(Config::parse_source("output-profile laptop { output eDP-1 auto-refresh=\"yes\"; }").is_err());
    }

    #[test]
    fn rejects_invalid_output_profiles() {
        let invalid_scale = parse("output-profile name=\"bad\" {\n    output match=\"eDP-1\" scale=0.0\n}\n");
        let invalid_mode = parse("output-profile name=\"bad\" {\n    output match=\"eDP-1\" mode=\"native\"\n}\n");
        let duplicate =
            parse("output-profile name=\"bad\" {\n    output match=\"eDP-1\"\n    output match=\"eDP-1\"\n}\n");

        assert!(invalid_scale.output_profiles().is_err());
        assert!(invalid_mode.output_profiles().is_err());
        assert!(duplicate.output_profiles().is_err());
    }
}
