use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WindowRuleConfig {
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub transient: Option<bool>,
    pub workspace: Option<u32>,
    pub floating: Option<bool>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub min_width: Option<f64>,
    pub min_height: Option<f64>,
    pub fullscreen: Option<bool>,
    pub block_out_from_screencasts: Option<bool>,
    pub idle_inhibit: Option<crate::idle_inhibition::Mode>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowRule {
    app_id: Option<String>,
    title: Option<String>,
    transient: Option<bool>,
    workspace: Option<u32>,
    floating: Option<bool>,
    width: Option<f64>,
    height: Option<f64>,
    min_width: Option<f64>,
    min_height: Option<f64>,
    fullscreen: Option<bool>,
    block_out_from_screencasts: Option<bool>,
    idle_inhibit: Option<crate::idle_inhibition::Mode>,
}

impl WindowRule {
    pub(crate) fn idle_policy(&self) -> Option<crate::idle_inhibition::Mode> {
        self.idle_inhibit
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowRuleResult {
    pub workspace: Option<u32>,
    pub floating: Option<bool>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub min_width: Option<f64>,
    pub min_height: Option<f64>,
    pub fullscreen: Option<bool>,
    pub block_out_from_screencasts: Option<bool>,
    pub idle_inhibit: Option<crate::idle_inhibition::Mode>,
}

/// Only changed matches should override an existing window's manual state.
pub(crate) fn live_result(
    mut old: WindowRuleResult,
    mut new: WindowRuleResult,
    transient: bool,
) -> Option<WindowRuleResult> {
    // Capture and idle policies are reevaluated without changing placement.
    old.block_out_from_screencasts = None;
    new.block_out_from_screencasts = None;
    old.idle_inhibit = None;
    new.idle_inhibit = None;
    if old == new {
        return None;
    }
    if new.floating.is_none()
        && new.width.is_none()
        && new.height.is_none()
        && (old.floating.is_some() || old.width.is_some() || old.height.is_some())
    {
        new.floating = Some(transient);
    }
    if new.fullscreen.is_none() && old.fullscreen.is_some() {
        new.fullscreen = Some(false);
    }
    Some(new)
}

pub fn validate(configured: &[WindowRuleConfig]) -> Result<Vec<WindowRule>, String> {
    configured
        .iter()
        .enumerate()
        .map(|(index, rule)| validate_rule(index, rule))
        .collect()
}

pub fn resolve(rules: &[WindowRule], app_id: Option<&str>, title: Option<&str>, transient: bool) -> WindowRuleResult {
    let app_id = app_id.map(normalize_app_id);
    let mut result = WindowRuleResult::default();
    if is_native_dialog(app_id.as_deref()) {
        result.floating = Some(true);
    }

    if app_id.as_deref() == Some("dev.ferese.authentication") {
        result.block_out_from_screencasts = Some(true);
    }

    for rule in rules {
        let app_id_matches = rule
            .app_id
            .as_deref()
            .is_none_or(|expected| app_id.as_deref() == Some(expected));
        let title_matches = rule.title.as_deref().is_none_or(|expected| title == Some(expected));
        let transient_matches = rule.transient.is_none_or(|expected| expected == transient);
        if !app_id_matches || !title_matches || !transient_matches {
            continue;
        }

        result.workspace = rule.workspace.or(result.workspace);
        result.floating = rule.floating.or(result.floating);
        result.width = rule.width.or(result.width);
        result.height = rule.height.or(result.height);
        result.min_width = rule.min_width.or(result.min_width);
        result.min_height = rule.min_height.or(result.min_height);
        result.fullscreen = rule.fullscreen.or(result.fullscreen);
        result.block_out_from_screencasts = rule.block_out_from_screencasts.or(result.block_out_from_screencasts);
        result.idle_inhibit = rule.idle_inhibit.or(result.idle_inhibit);
    }

    result
}

fn validate_rule(index: usize, rule: &WindowRuleConfig) -> Result<WindowRule, String> {
    if rule.app_id.is_none() && rule.title.is_none() && rule.transient.is_none() {
        return Err(format!("window_rules[{index}] must match app_id, title, or transient"));
    }
    if rule.workspace == Some(0) {
        return Err(format!("window_rules[{index}].workspace must be greater than zero"));
    }

    let app_id = rule
        .app_id
        .as_deref()
        .map(nonempty_matcher)
        .transpose()
        .map_err(|message| format!("window_rules[{index}].app_id {message}"))?
        .map(normalize_app_id);
    let title = rule
        .title
        .as_deref()
        .map(nonempty_matcher)
        .transpose()
        .map_err(|message| format!("window_rules[{index}].title {message}"))?
        .map(str::to_owned);
    let width = positive_dimension(rule.width, index, "width")?;
    let height = positive_dimension(rule.height, index, "height")?;
    let min_width = positive_dimension(rule.min_width, index, "min-width")?;
    let min_height = positive_dimension(rule.min_height, index, "min-height")?;

    Ok(WindowRule {
        app_id,
        title,
        transient: rule.transient,
        workspace: rule.workspace,
        floating: rule.floating,
        width,
        height,
        min_width,
        min_height,
        fullscreen: rule.fullscreen,
        block_out_from_screencasts: rule.block_out_from_screencasts,
        idle_inhibit: rule.idle_inhibit,
    })
}

fn nonempty_matcher(value: &str) -> Result<&str, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        Err("cannot be empty")
    } else {
        Ok(value)
    }
}

fn positive_dimension(value: Option<f64>, index: usize, field: &'static str) -> Result<Option<f64>, String> {
    if value.is_none_or(|value| value.is_finite() && value > 0.0) {
        Ok(value)
    } else {
        Err(format!(
            "window_rules[{index}].{field} must be a positive finite number"
        ))
    }
}

pub(crate) fn is_native_dialog(app_id: Option<&str>) -> bool {
    matches!(
        app_id.map(normalize_app_id).as_deref(),
        Some(
            "dev.ferese.authentication"
                | "dev.ferese.screenshare"
                | "dev.ferese.screenshot"
                | "dev.ferese.portaldialog"
        )
    )
}

pub(crate) fn normalize_app_id(value: &str) -> String {
    value
        .trim()
        .strip_suffix(".desktop")
        .unwrap_or(value.trim())
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_rule_removal_reverts_placement_but_unchanged_rules_preserve_manual_state() {
        let old = super::WindowRuleResult {
            floating: Some(true),
            fullscreen: Some(true),
            ..Default::default()
        };
        assert_eq!(super::live_result(old, old, false), None);
        let removed = super::live_result(old, Default::default(), false).unwrap();
        assert_eq!(removed.floating, Some(false));
        assert_eq!(removed.fullscreen, Some(false));
        assert_eq!(
            super::live_result(old, Default::default(), true).unwrap().floating,
            Some(true)
        );
        let new = super::WindowRuleResult {
            width: Some(800.),
            ..Default::default()
        };
        assert_eq!(super::live_result(old, new, false).unwrap().floating, None);
    }
    use super::*;

    fn config(app_id: &str) -> WindowRuleConfig {
        WindowRuleConfig {
            app_id: Some(app_id.to_owned()),
            title: None,
            transient: None,
            workspace: None,
            floating: None,
            width: None,
            height: None,
            min_width: None,
            min_height: None,
            fullscreen: None,
            block_out_from_screencasts: None,
            idle_inhibit: None,
        }
    }

    #[test]
    fn capture_privacy_defaults_overrides_and_placement_are_independent() {
        for app_id in ["dev.ferese.Authentication", "dev.ferese.Authentication.desktop"] {
            assert_eq!(
                resolve(&[], Some(app_id), None, false).block_out_from_screencasts,
                Some(true)
            );
            let rules = validate(&[WindowRuleConfig {
                block_out_from_screencasts: Some(false),
                ..config(app_id)
            }])
            .unwrap();
            assert_eq!(
                resolve(&rules, Some(app_id), None, false).block_out_from_screencasts,
                Some(false)
            );
        }
        assert_eq!(
            resolve(&[], Some("editor"), None, false).block_out_from_screencasts,
            None
        );
        let rules = validate(&[
            WindowRuleConfig {
                block_out_from_screencasts: Some(true),
                ..config("editor")
            },
            WindowRuleConfig {
                title: Some("Public".into()),
                block_out_from_screencasts: Some(false),
                ..config("editor")
            },
        ])
        .unwrap();
        assert_eq!(
            resolve(&rules, Some("editor"), Some("Secret"), false).block_out_from_screencasts,
            Some(true)
        );
        assert_eq!(
            resolve(&rules, Some("editor"), Some("Public"), false).block_out_from_screencasts,
            Some(false)
        );
        let old = WindowRuleResult {
            width: Some(800.),
            floating: Some(true),
            ..Default::default()
        };
        let new = WindowRuleResult {
            block_out_from_screencasts: Some(true),
            ..old
        };
        assert_eq!(live_result(old, new, false), None);
    }

    #[test]
    fn native_dialogs_float_by_default_and_allow_user_overrides() {
        for app_id in [
            "dev.ferese.PortalDialog",
            "dev.ferese.ScreenShare",
            "dev.ferese.ScreenShare.desktop",
            "dev.ferese.Authentication",
            "dev.ferese.Screenshot",
        ] {
            assert_eq!(resolve(&[], Some(app_id), None, false).floating, Some(true));
            let rules = validate(&[WindowRuleConfig {
                floating: Some(false),
                ..config(app_id)
            }])
            .unwrap();
            assert_eq!(resolve(&rules, Some(app_id), None, false).floating, Some(false));
        }
        assert_eq!(resolve(&[], Some("org.example.Editor"), None, false).floating, None);
    }

    #[test]
    fn app_ids_are_normalized_for_matching() {
        let rules = validate(&[WindowRuleConfig {
            floating: Some(true),
            ..config("Org.Example.Editor.desktop")
        }])
        .unwrap();

        assert_eq!(
            resolve(&rules, Some("org.example.editor"), None, false).floating,
            Some(true)
        );
    }

    #[test]
    fn later_matching_rules_override_only_their_fields() {
        let rules = validate(&[
            WindowRuleConfig {
                workspace: Some(3),
                floating: Some(true),
                width: Some(800.0),
                ..config("editor")
            },
            WindowRuleConfig {
                floating: Some(false),
                fullscreen: Some(true),
                ..config("editor")
            },
        ])
        .unwrap();

        assert_eq!(
            resolve(&rules, Some("editor"), None, false),
            WindowRuleResult {
                workspace: Some(3),
                floating: Some(false),
                width: Some(800.0),
                fullscreen: Some(true),
                ..WindowRuleResult::default()
            }
        );
    }

    #[test]
    fn rejects_catch_all_and_invalid_dimensions() {
        let catch_all = WindowRuleConfig {
            app_id: None,
            title: None,
            transient: None,
            workspace: None,
            floating: Some(true),
            width: None,
            height: None,
            min_width: None,
            min_height: None,
            fullscreen: None,
            block_out_from_screencasts: None,
            idle_inhibit: None,
        };
        let invalid_size = WindowRuleConfig {
            width: Some(f64::NAN),
            ..config("editor")
        };
        let invalid_minimum = WindowRuleConfig {
            min_width: Some(0.0),
            ..config("editor")
        };

        assert!(validate(&[catch_all]).is_err());
        assert!(validate(&[invalid_size]).is_err());
        assert!(validate(&[invalid_minimum]).is_err());
    }

    #[test]
    fn minimum_size_overrides_are_per_axis_and_later_rules_win() {
        let rules = validate(&[
            WindowRuleConfig {
                min_width: Some(800.0),
                min_height: Some(600.0),
                ..config("spotify")
            },
            WindowRuleConfig {
                min_width: Some(500.0),
                ..config("spotify")
            },
        ])
        .unwrap();

        let result = resolve(&rules, Some("spotify"), None, false);
        assert_eq!(result.min_width, Some(500.0));
        assert_eq!(result.min_height, Some(600.0));

        let single = validate(&[WindowRuleConfig {
            min_height: Some(400.0),
            ..config("spotify")
        }])
        .unwrap();
        let result = resolve(&single, Some("spotify"), None, false);
        assert_eq!(result.min_width, None);
        assert_eq!(result.min_height, Some(400.0));
        assert_eq!(resolve(&[], Some("spotify"), None, false).min_width, None);
    }
}
