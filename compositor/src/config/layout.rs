use super::*;

impl Config {
    pub fn default_column_display(&self) -> ferese_layout::ColumnDisplay {
        match self
            .scrolling
            .default_column_display
            .unwrap_or(ColumnDisplayValue::Stacked)
        {
            ColumnDisplayValue::Stacked => ferese_layout::ColumnDisplay::Stacked,
            ColumnDisplayValue::Tabbed => ferese_layout::ColumnDisplay::Tabbed,
        }
    }

    pub fn layout_mode(&self) -> LayoutMode {
        match self.layout.mode.unwrap_or(LayoutModeValue::Scrolling) {
            LayoutModeValue::Scrolling => LayoutMode::Scrolling,
            LayoutModeValue::Tree => LayoutMode::Tree,
        }
    }

    pub fn gap_config(&self) -> Result<GapConfig, ConfigError> {
        let inner = nonnegative_layout_value(self.layout.inner_gap, "inner_gap")?;
        let outer = nonnegative_layout_value(self.layout.outer_gap, "outer_gap")?;

        Ok(GapConfig {
            inner,
            outer,
            smart: self.layout.smart_gaps,
        })
    }

    pub fn default_column_width(&self) -> Result<ColumnWidth, ConfigError> {
        parse_column_width(
            self.scrolling
                .default_column_width
                .as_ref()
                .unwrap_or(&ColumnWidthValue::Proportion(0.5)),
            "default_column_width",
        )
    }

    pub fn width_presets(&self) -> Result<Vec<ColumnWidth>, ConfigError> {
        if self.scrolling.width_presets.is_empty() {
            return Ok(vec![
                ColumnWidth::Proportion(1.0 / 3.0),
                ColumnWidth::Proportion(0.5),
                ColumnWidth::Proportion(2.0 / 3.0),
                ColumnWidth::Full,
            ]);
        }

        self.scrolling
            .width_presets
            .iter()
            .map(|value| parse_column_width(value, "width_presets"))
            .collect()
    }

    pub fn scrolling_focus_strategy(&self) -> ViewportFocusStrategy {
        match self
            .scrolling
            .focus_strategy
            .unwrap_or(FocusStrategyValue::Minimal)
        {
            FocusStrategyValue::Minimal => ViewportFocusStrategy::Minimal,
            FocusStrategyValue::CenterOnFocus => ViewportFocusStrategy::Center,
            FocusStrategyValue::Paged => ViewportFocusStrategy::Paged,
        }
    }
}

pub(super) fn nonnegative_layout_value(
    value: f64,
    field: &'static str,
) -> Result<f64, ConfigError> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(ConfigError::LayoutValue { field, value })
    }
}

pub(super) fn parse_column_width(
    value: &ColumnWidthValue,
    field: &'static str,
) -> Result<ColumnWidth, ConfigError> {
    match value {
        ColumnWidthValue::Proportion(value) if value.is_finite() && *value > 0.0 => {
            Ok(ColumnWidth::Proportion(*value))
        }
        ColumnWidthValue::Named(value) if value.eq_ignore_ascii_case("full") => {
            Ok(ColumnWidth::Full)
        }
        value => Err(ConfigError::ColumnWidth {
            field,
            value: match value {
                ColumnWidthValue::Proportion(value) => value.to_string(),
                ColumnWidthValue::Named(value) => format!("{value:?}"),
            },
        }),
    }
}
