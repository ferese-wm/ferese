use crate::{CornerRadii, Edge};
use ferese_layout::Rect;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Style {
    Floating,
    Capsules,
    Notch,
    #[default]
    Flat,
    Frame,
    CornerWrap,
    Fused,
    Morph,
    Adaptive,
    WindowHug,
}

impl Style {
    /// Styles with a complete shell surface contract.
    pub const AVAILABLE: [Self; 7] = [
        Self::Floating,
        Self::Capsules,
        Self::Notch,
        Self::Flat,
        Self::Fused,
        Self::Adaptive,
        Self::Morph,
    ];

    pub const ALL: [Self; 10] = [
        Self::Floating,
        Self::Capsules,
        Self::Notch,
        Self::Flat,
        Self::Adaptive,
        Self::Morph,
        Self::Fused,
        Self::Frame,
        Self::CornerWrap,
        Self::WindowHug,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Floating => "floating",
            Self::Capsules => "capsules",
            Self::Notch => "notch",
            Self::Flat => "flat",
            Self::Frame => "frame",
            Self::CornerWrap => "corner-wrap",
            Self::Fused => "fused",
            Self::Morph => "morph",
            Self::Adaptive => "adaptive",
            Self::WindowHug => "window-hug",
        }
    }
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Floating => "Floating",
            Self::Capsules => "Capsules",
            Self::Notch => "Notch",
            Self::Flat => "Edge to edge",
            Self::Frame => "Frame",
            Self::CornerWrap => "Corner wrap",
            Self::Fused => "Fused islands",
            Self::Morph => "Morph",
            Self::Adaptive => "Adaptive",
            Self::WindowHug => "Window hug",
        }
    }
    #[must_use]
    pub const fn connected(self) -> bool {
        matches!(self, Self::Capsules | Self::Notch)
    }
    #[must_use]
    pub const fn detached(self) -> bool {
        matches!(
            self,
            Self::Floating
                | Self::Capsules
                | Self::Fused
                | Self::Morph
                | Self::Adaptive
                | Self::WindowHug
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZonePosition {
    Start,
    Center,
    End,
}

/// Clockwise from the left corner on the anchored edge. The same values are
/// used for top and bottom panels; only conversion to screen space differs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchoredRadii(pub [f32; 4]);

impl AnchoredRadii {
    #[must_use]
    pub fn screen_space(self, edge: Edge) -> CornerRadii {
        let [flat_left, flat_right, free_right, free_left] = self.0;
        CornerRadii(match edge {
            Edge::Top => [flat_left, flat_right, free_right, free_left],
            Edge::Bottom => [free_left, free_right, flat_right, flat_left],
        })
    }

    /// Membership refers to the occupied groups after availability and
    /// overflow have been resolved, never to the stored group count.
    pub fn joined(mut self, index: usize, count: usize) -> Result<Self, &'static str> {
        if index >= count {
            return Err("a joined group must belong to a nonempty zone");
        }
        if index > 0 {
            self.0[0] = 0.;
            self.0[3] = 0.;
        }
        if index + 1 < count {
            self.0[1] = 0.;
            self.0[2] = 0.;
        }
        Ok(self)
    }
}

pub fn effective_radii(
    style: Style,
    zone: ZonePosition,
    edge: Edge,
    size: [f32; 2],
    index: usize,
    count: usize,
) -> Result<CornerRadii, &'static str> {
    if !size
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.)
    {
        return Err("panel dimensions must be finite and nonnegative");
    }
    if index >= count {
        return Err("a group must belong to a nonempty zone");
    }
    let [width, height] = size;
    let base = AnchoredRadii(match style {
        Style::Floating
        | Style::Capsules
        | Style::Fused
        | Style::Morph
        | Style::Adaptive
        | Style::WindowHug => [height / 2.; 4],
        Style::Notch => match zone {
            ZonePosition::Start => [0., 0., 14., 0.],
            ZonePosition::Center => [0., 0., 20., 20.],
            ZonePosition::End => [0., 0., 0., 14.],
        },
        Style::Flat | Style::Frame | Style::CornerWrap => [0.; 4],
    });
    let radii = if matches!(style, Style::Capsules | Style::Notch) {
        base.joined(index, count)?
    } else {
        base
    };
    Ok(radii.screen_space(edge).normalized(width, height))
}

/// Snap rendered boundaries, without quantizing the stored animation state.
#[derive(Clone, Copy, Debug)]
pub struct PixelGrid {
    scale: f64,
}

impl PixelGrid {
    pub fn try_new(scale: f64) -> Result<Self, &'static str> {
        if !scale.is_finite() || scale <= 0. {
            return Err("output scale must be finite and positive");
        }
        Ok(Self { scale })
    }

    #[must_use]
    pub fn snap(self, position: f64) -> f64 {
        (position * self.scale).round() / self.scale
    }

    pub fn rect(self, bounds: Rect) -> Result<Rect, &'static str> {
        if ![bounds.x, bounds.y, bounds.width, bounds.height]
            .into_iter()
            .all(f64::is_finite)
            || bounds.width < 0.
            || bounds.height < 0.
        {
            return Err("surface bounds must be finite with nonnegative dimensions");
        }
        let left = self.snap(bounds.x);
        let top = self.snap(bounds.y);
        let right = self.snap(bounds.x + bounds.width);
        let bottom = self.snap(bounds.y + bounds.height);
        if ![left, top, right, bottom].into_iter().all(f64::is_finite) {
            return Err("surface bounds exceed the physical pixel grid");
        }
        Ok(Rect::new(
            left,
            top,
            (right - left).max(0.),
            (bottom - top).max(0.),
        ))
    }
}

/// Exact circular-corner hit testing, including asymmetric radii larger than
/// half the surface height. Decorative fillets and shadows are separate shapes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundedSurface {
    bounds: Rect,
    radii: CornerRadii,
}

impl RoundedSurface {
    pub fn try_new(
        bounds: Rect,
        radii: CornerRadii,
        grid: PixelGrid,
    ) -> Result<Self, &'static str> {
        if radii.validate().is_err() {
            return Err("surface radii must be finite and nonnegative");
        }
        let bounds = grid.rect(bounds)?;
        if bounds.width > f64::from(f32::MAX) || bounds.height > f64::from(f32::MAX) {
            return Err("surface dimensions exceed the radius coordinate range");
        }
        let radii = radii.normalized(bounds.width as f32, bounds.height as f32);
        Ok(Self { bounds, radii })
    }

    #[must_use]
    pub fn bounds(self) -> Rect {
        self.bounds
    }

    #[must_use]
    pub fn radii(self) -> CornerRadii {
        self.radii
    }

    /// Right and bottom boundaries are excluded so a shared seam belongs to
    /// exactly one surface, rather than both neighbors.
    #[must_use]
    pub fn contains(self, point: [f64; 2]) -> bool {
        let [x, y] = [point[0] - self.bounds.x, point[1] - self.bounds.y];
        let (width, height) = (self.bounds.width, self.bounds.height);
        if !(x >= 0. && y >= 0. && x < width && y < height) {
            return false;
        }
        for (radius, dx, dy) in [
            (self.radii.0[0], x, y),
            (self.radii.0[1], width - x, y),
            (self.radii.0[2], width - x, height - y),
            (self.radii.0[3], x, height - y),
        ] {
            let radius = f64::from(radius);
            if dx < radius && dy < radius && (dx - radius).hypot(dy - radius) > radius {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_the_last_group_rounds_the_new_end() {
        let radius = |index, count| {
            effective_radii(
                Style::Capsules,
                ZonePosition::Start,
                Edge::Top,
                [100., 36.],
                index,
                count,
            )
            .unwrap()
        };
        assert_eq!(radius(1, 3).0, [0.; 4]);
        assert_eq!(radius(1, 2).0, [0., 18., 18., 0.]);
        assert_eq!(radius(0, 1).0, [18.; 4]);
        assert_eq!(radius(0, 2).0, [18., 0., 0., 18.]);
        assert!(
            effective_radii(
                Style::Capsules,
                ZonePosition::Start,
                Edge::Top,
                [100., 36.],
                0,
                0
            )
            .is_err()
        );
    }

    #[test]
    fn anchor_flip_mirrors_all_styles_and_zone_positions() {
        for style in [Style::Floating, Style::Capsules, Style::Notch, Style::Flat] {
            for zone in [ZonePosition::Start, ZonePosition::Center, ZonePosition::End] {
                for index in 0..3 {
                    let top = effective_radii(style, zone, Edge::Top, [100., 28.], index, 3)
                        .unwrap()
                        .0;
                    let bottom = effective_radii(style, zone, Edge::Bottom, [100., 28.], index, 3)
                        .unwrap()
                        .0;
                    assert_eq!(bottom, [top[3], top[2], top[1], top[0]]);
                }
            }
        }
    }

    #[test]
    fn asymmetric_corners_preserve_notch_shape_and_scale_proportionally() {
        let notch = effective_radii(
            Style::Notch,
            ZonePosition::Center,
            Edge::Top,
            [100., 28.],
            0,
            1,
        )
        .unwrap();
        assert_eq!(notch.0, [0., 0., 20., 20.]);
        assert_eq!(notch.normalized(30., 28.).0, [0., 0., 15., 15.]);
        assert_eq!(
            CornerRadii([10., 20., 40., 30.]).normalized(35., 25.).0,
            [25. / 6., 25. / 3., 50. / 3., 12.5]
        );
        assert_eq!(
            CornerRadii::uniform(f32::MAX).normalized(40., 28.).0,
            [14.; 4]
        );
        assert_eq!(notch.normalized(0., 28.).0, [0.; 4]);
    }

    #[test]
    fn shared_boundaries_snap_without_seams_at_fractional_scales() {
        for scale in [1., 1.25, 1.5, 2.] {
            let grid = PixelGrid::try_new(scale).unwrap();
            for origin in [-1800.3, -0.4, 0., 23.3] {
                let left = grid.rect(Rect::new(origin, 0., 33.3, 36.)).unwrap();
                let right = grid.rect(Rect::new(origin + 33.3, 0., 42.7, 36.)).unwrap();
                assert!((left.x + left.width - right.x).abs() < 1e-9);
                for boundary in [left.x, left.x + left.width, right.x + right.width] {
                    assert!((boundary * scale - (boundary * scale).round()).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn floating_gaps_and_round_corners_do_not_capture_input() {
        let grid = PixelGrid::try_new(1.25).unwrap();
        let left = RoundedSurface::try_new(
            Rect::new(8., 8., 100., 36.),
            CornerRadii::uniform(18.),
            grid,
        )
        .unwrap();
        let right = RoundedSurface::try_new(
            Rect::new(116., 8., 100., 36.),
            CornerRadii::uniform(18.),
            grid,
        )
        .unwrap();
        for point in [
            [0., 0.],
            [8., 8.],
            [112., 26.],
            [216., 26.],
            [f64::NAN, 26.],
        ] {
            assert!(!left.contains(point) && !right.contains(point), "{point:?}");
        }
        assert!(left.contains([30., 26.]));
        assert!(right.contains([150., 26.]));
    }

    #[test]
    fn asymmetric_hit_testing_matches_a_vertically_mirrored_surface() {
        let grid = PixelGrid::try_new(1.).unwrap();
        let bounds = Rect::new(0., 0., 100., 28.);
        let top = RoundedSurface::try_new(
            bounds,
            AnchoredRadii([0., 0., 20., 20.]).screen_space(Edge::Top),
            grid,
        )
        .unwrap();
        let bottom = RoundedSurface::try_new(
            bounds,
            AnchoredRadii([0., 0., 20., 20.]).screen_space(Edge::Bottom),
            grid,
        )
        .unwrap();
        assert!(top.contains([1., 1.]));
        assert!(!top.contains([1., 27.]));
        for x in 0..100 {
            for y in 0..28 {
                assert_eq!(
                    top.contains([x as f64 + 0.5, y as f64 + 0.5]),
                    bottom.contains([x as f64 + 0.5, 27.5 - y as f64])
                );
            }
        }
    }

    #[test]
    fn a_connected_seam_has_exactly_one_input_owner() {
        let grid = PixelGrid::try_new(1.5).unwrap();
        let left = RoundedSurface::try_new(
            Rect::new(0., 0., 30., 36.),
            CornerRadii([18., 0., 0., 18.]),
            grid,
        )
        .unwrap();
        let right = RoundedSurface::try_new(
            Rect::new(30., 0., 40., 36.),
            CornerRadii([0., 18., 18., 0.]),
            grid,
        )
        .unwrap();
        for y in [0., 18., 35.9] {
            assert!(!left.contains([30., y]));
            assert!(right.contains([30., y]));
        }
    }

    #[test]
    fn invalid_runtime_geometry_is_rejected() {
        for scale in [0., -1., f64::NAN, f64::INFINITY] {
            assert!(PixelGrid::try_new(scale).is_err());
        }
        let grid = PixelGrid::try_new(1.).unwrap();
        for bounds in [
            Rect::new(0., 0., -1., 28.),
            Rect::new(f64::NAN, 0., 10., 28.),
            Rect::new(f64::MAX, 0., f64::MAX, 28.),
        ] {
            assert!(grid.rect(bounds).is_err());
        }
        assert!(
            RoundedSurface::try_new(Rect::new(0., 0., 10., 28.), CornerRadii::uniform(-1.), grid)
                .is_err()
        );
        assert!(
            effective_radii(
                Style::Floating,
                ZonePosition::Start,
                Edge::Top,
                [100., f32::NAN],
                0,
                1
            )
            .is_err()
        );
    }
}
