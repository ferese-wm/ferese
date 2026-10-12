use super::*;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct TabIndicatorConfig {
    width: f64,
    segment_height: f64,
    segment_gap: f64,
    hit_width: f64,
    gap_threshold: f64,
    list_width: f64,
    row_height: f64,
    padding: f64,
    font_size: f64,
    flash_duration_ms: u64,
    active_color: Option<String>,
    inactive_color: Option<String>,
    background_color: Option<String>,
    text_color: Option<String>,
}

impl Default for TabIndicatorConfig {
    fn default() -> Self {
        Self {
            width: 4.0,
            segment_height: 18.0,
            segment_gap: 4.0,
            hit_width: 12.0,
            gap_threshold: 8.0,
            list_width: 220.0,
            row_height: 28.0,
            padding: 4.0,
            font_size: 12.0,
            flash_duration_ms: 1000,
            active_color: None,
            inactive_color: None,
            background_color: None,
            text_color: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TabIndicatorSettings {
    pub width: f64,
    pub segment_height: f64,
    pub segment_gap: f64,
    pub hit_width: f64,
    pub gap_threshold: f64,
    pub list_width: f64,
    pub row_height: f64,
    pub padding: f64,
    pub font_size: f64,
    pub flash_duration: std::time::Duration,
    pub active_color: Option<[f32; 4]>,
    pub inactive_color: Option<[f32; 4]>,
    pub background_color: Option<[f32; 4]>,
    pub text_color: Option<[f32; 4]>,
}

impl Config {
    pub(crate) fn tab_indicator_settings(&self) -> Result<TabIndicatorSettings, ConfigError> {
        let c = &self.scrolling.tab_indicator;
        let range = |value: f64, min: f64, max: f64, field: &'static str| {
            if value.is_finite() && (min..=max).contains(&value) {
                Ok(value)
            } else {
                Err(ConfigError::LayoutValue { field, value })
            }
        };
        let color =
            |value: &Option<String>, field| value.as_ref().map(|s| parse_color(s, field).map(|c| c.0)).transpose();
        let width = range(c.width, 1.0, 16.0, "tab_indicator.width")?;
        let hit_width = range(c.hit_width, width, 64.0, "tab_indicator.hit_width")?;
        let font_size = range(c.font_size, 8.0, 32.0, "tab_indicator.font_size")?;
        Ok(TabIndicatorSettings {
            width,
            hit_width,
            font_size,
            segment_height: range(c.segment_height, width, 128.0, "tab_indicator.segment_height")?,
            segment_gap: range(c.segment_gap, 0.0, 64.0, "tab_indicator.segment_gap")?,
            gap_threshold: range(c.gap_threshold, width, 128.0, "tab_indicator.gap_threshold")?,
            list_width: range(c.list_width, 80.0, 640.0, "tab_indicator.list_width")?,
            row_height: range(c.row_height, font_size + 6.0, 80.0, "tab_indicator.row_height")?,
            padding: range(c.padding, 0.0, 24.0, "tab_indicator.padding")?,
            flash_duration: std::time::Duration::from_millis(range(
                c.flash_duration_ms as f64,
                0.0,
                60_000.0,
                "tab_indicator.flash_duration_ms",
            )? as u64),
            active_color: color(&c.active_color, "tab_indicator.active_color")?,
            inactive_color: color(&c.inactive_color, "tab_indicator.inactive_color")?,
            background_color: color(&c.background_color, "tab_indicator.background_color")?,
            text_color: color(&c.text_color, "tab_indicator.text_color")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_defaults_overrides_and_validation() {
        let c = Config::parse_source("").unwrap().tab_indicator_settings().unwrap();
        assert_eq!(
            (c.width, c.segment_height, c.segment_gap, c.hit_width, c.gap_threshold),
            (4., 18., 4., 12., 8.)
        );
        assert_eq!(c.flash_duration.as_millis(), 1000);
        assert!(c.active_color.is_none());
        let config = Config::parse_source(
            "scrolling { tab-indicator { width 3; hit-width 14; flash-duration-ms 750; active-color \"#ff0000\"; }; }",
        )
        .unwrap();
        let c = config.tab_indicator_settings().unwrap();
        assert_eq!((c.width, c.hit_width, c.flash_duration.as_millis()), (3., 14., 750));
        assert_eq!(c.active_color, Some([1., 0., 0., 1.]));
        for source in [
            "width 0",
            "segment-gap -1",
            "hit-width 2",
            "row-height 8",
            "flash-duration-ms 60001",
            "active-color \"invalid\"",
        ] {
            let config = Config::parse_source(&format!("scrolling {{ tab-indicator {{ {source}; }}; }}")).unwrap();
            assert!(config.tab_indicator_settings().is_err(), "{source}");
        }
        assert!(Config::parse_source("scrolling { tab-indicator { typo 1; }; }").is_err());
    }
}
