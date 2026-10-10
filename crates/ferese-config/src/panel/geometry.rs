use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Logical dimensions owned by a panel, independent of its color theme or edge.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PanelGeometry {
    pub height: f32,
    pub edge_margin: i32,
    pub side_margins: i32,
    pub window_clearance: i32,
    pub inner_padding: f32,
}

impl Default for PanelGeometry {
    fn default() -> Self {
        Self {
            height: 28.,
            edge_margin: 0,
            side_margins: 0,
            window_clearance: 0,
            inner_padding: 12.,
        }
    }
}

impl PanelGeometry {
    pub fn validate(self) -> Result<(), String> {
        if !self.height.is_finite()
            || !(24. ..=128.).contains(&self.height)
            || !self.inner_padding.is_finite()
            || !(0. ..=64.).contains(&self.inner_padding)
            || [self.edge_margin, self.side_margins, self.window_clearance]
                .into_iter()
                .any(|value| !(0..=4096).contains(&value))
        {
            return Err("panel height must be 24..128, inner padding 0..64, and margins/clearance 0..4096".into());
        }
        Ok(())
    }

    /// Read old theme keys only when importing an earlier panel configuration.
    pub fn from_legacy(document: &crate::Document) -> Result<Self, crate::Error> {
        let geometry = legacy(document.value())?;
        geometry.validate().map_err(crate::Error::from)?;
        Ok(geometry)
    }
}

fn legacy(value: &Value) -> Result<PanelGeometry, crate::Error> {
    let mut geometry = serde_json::to_value(PanelGeometry::default()).unwrap();
    if let Some(theme) = value.pointer("/theme/geometry") {
        for (old, new) in [
            ("top_bar_height", "height"),
            ("top_bar_margin_top", "edge_margin"),
            ("top_bar_margin_horizontal", "side_margins"),
            ("top_bar_window_gap", "window_clearance"),
            ("panel_padding", "inner_padding"),
        ] {
            if let Some(value) = theme.get(old) {
                geometry[new] = value.clone();
            }
        }
    }
    serde_json::from_value(geometry).map_err(|error| crate::Error::from(error.to_string()))
}

pub(crate) fn migrate(value: &mut Value) -> Result<(), crate::Error> {
    // Preserve KDL text and comments. New edits serialize the normalized values.
    let needs_import = value.get("panels").and_then(Value::as_array).is_some_and(|panels| {
        panels
            .iter()
            .any(|panel| panel.is_object() && panel.get("geometry").is_none())
    });
    if !needs_import {
        return Ok(());
    }
    let geometry = serde_json::to_value(legacy(value)?).unwrap();
    if let Some(panels) = value.get_mut("panels").and_then(Value::as_array_mut) {
        for panel in panels {
            if let Some(panel) = panel.as_object_mut() {
                panel.entry("geometry").or_insert_with(|| geometry.clone());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_is_imported_once_and_explicit_panel_values_win() {
        let source = "// keep\ntheme { geometry { top-bar-height 36; top-bar-margin-top 6; top-bar-margin-horizontal 18; top-bar-window-gap 4; }; }\npanel main { edge bottom; }";
        let mut document = crate::Document::parse(source).unwrap();
        let panel = |document: &crate::Document| {
            serde_json::from_value::<Vec<super::super::Panel>>(document.get("panels").unwrap().clone())
                .unwrap()
                .remove(0)
        };
        let imported = panel(&document);
        assert_eq!(
            imported.geometry,
            PanelGeometry {
                height: 36.,
                edge_margin: 6,
                side_margins: 18,
                window_clearance: 4,
                ..Default::default()
            }
        );
        assert_eq!(document.to_string(), source);
        document
            .set("panels.0.geometry", serde_json::to_value(imported.geometry).unwrap())
            .unwrap();
        document.set("theme.geometry.top_bar_height", 48.into()).unwrap();
        assert_eq!(panel(&document).geometry.height, 36.);
        assert!(document.to_string().contains("// keep"));
        let mut theme = crate::theme::default_theme();
        theme.tokens.geometry.top_bar_height = 64.;
        assert_eq!(panel(&document.with_theme(&theme)).geometry, imported.geometry);
    }

    #[test]
    fn invalid_geometry_rejects_nonfinite_dimensions_and_negative_clearances() {
        for geometry in [
            PanelGeometry {
                height: f32::NAN,
                ..Default::default()
            },
            PanelGeometry {
                edge_margin: -1,
                ..Default::default()
            },
            PanelGeometry {
                height: 12.,
                ..Default::default()
            },
            PanelGeometry {
                inner_padding: f32::INFINITY,
                ..Default::default()
            },
        ] {
            assert!(geometry.validate().is_err());
        }
    }
}
