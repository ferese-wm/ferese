use serde::{Deserialize, Serialize};

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
