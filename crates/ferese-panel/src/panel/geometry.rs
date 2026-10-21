use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeometryIntent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_margin: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_margins: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_clearance: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_padding: Option<f32>,
}

impl GeometryIntent {
    #[must_use]
    pub fn resolve(self, style: crate::PanelStyle) -> PanelGeometry {
        use crate::PanelStyle::*;
        let flush = matches!(style, Notch | Flat | Frame | CornerWrap);
        PanelGeometry {
            height: self.height.unwrap_or(match style {
                Frame => 40.,
                Morph => 34.,
                _ => 36.,
            }),
            edge_margin: if flush {
                0
            } else {
                self.edge_margin.unwrap_or(8)
            },
            side_margins: if flush {
                0
            } else {
                self.side_margins.unwrap_or(8)
            },
            window_clearance: self.window_clearance.unwrap_or(0),
            inner_padding: self.inner_padding.unwrap_or(12.),
        }
    }

    pub fn validate(self) -> Result<(), String> {
        // Validate pins even when a flush style temporarily overrides margins.
        let mut geometry = self.resolve(crate::PanelStyle::Floating);
        geometry.edge_margin = self.edge_margin.unwrap_or(0);
        geometry.side_margins = self.side_margins.unwrap_or(0);
        geometry.validate()
    }
}

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
            height: 36.,
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
            return Err(
                "panel height must be 24..128, inner padding 0..64, and margins/clearance 0..4096"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_defaults_and_user_pins_have_separate_ownership() {
        use crate::PanelStyle::*;
        let intent = GeometryIntent::default();
        assert_eq!(intent.resolve(Floating).edge_margin, 8);
        assert_eq!(intent.resolve(Flat).edge_margin, 0);
        assert_eq!(intent.resolve(Morph).height, 34.);
        let pins = GeometryIntent {
            height: Some(32.),
            edge_margin: Some(6),
            side_margins: Some(18),
            ..intent
        };
        assert_eq!(pins.resolve(Flat).height, 32.);
        assert_eq!(pins.resolve(Flat).side_margins, 0);
        assert_eq!(pins.resolve(Floating).side_margins, 18);
        assert_eq!(pins.resolve(Notch).edge_margin, 0);
        assert!(
            serde_json::to_value(intent)
                .unwrap()
                .as_object()
                .unwrap()
                .is_empty()
        );
        assert!(
            GeometryIntent {
                edge_margin: Some(-1),
                ..intent
            }
            .validate()
            .is_err()
        );
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
