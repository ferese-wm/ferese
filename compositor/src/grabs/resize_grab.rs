use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Size};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResizeEdge(xdg_toplevel::ResizeEdge);

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    fn from(edge: xdg_toplevel::ResizeEdge) -> Self {
        Self(edge)
    }
}

impl ResizeEdge {
    pub(crate) fn cursor(self) -> smithay::input::pointer::CursorIcon {
        use smithay::input::pointer::CursorIcon;
        match (self.left(), self.right(), self.top(), self.bottom()) {
            (true, _, true, _) | (_, true, _, true) => CursorIcon::NwseResize,
            (true, _, _, true) | (_, true, true, _) => CursorIcon::NeswResize,
            (true, _, _, _) | (_, true, _, _) => CursorIcon::EwResize,
            _ => CursorIcon::NsResize,
        }
    }
    pub fn at(point: Point<f64, Logical>, rect: Rectangle<i32, Logical>) -> Self {
        use xdg_toplevel::ResizeEdge::*;
        let x = (point.x - rect.loc.x as f64) / rect.size.w.max(1) as f64;
        let y = (point.y - rect.loc.y as f64) / rect.size.h.max(1) as f64;
        let (mut left, mut right) = (x < 1. / 3., x > 2. / 3.);
        let (mut top, mut bottom) = (y < 1. / 3., y > 2. / 3.);
        if !(left || right || top || bottom) {
            left = x < 0.5;
            right = !left;
            top = y < 0.5;
            bottom = !top;
        }
        Self(match (left, right, top, bottom) {
            (true, _, true, _) => TopLeft,
            (_, true, true, _) => TopRight,
            (true, _, _, true) => BottomLeft,
            (_, true, _, true) => BottomRight,
            (true, _, _, _) => Left,
            (_, true, _, _) => Right,
            (_, _, true, _) => Top,
            _ => Bottom,
        })
    }
    pub(crate) fn left(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Left
                | xdg_toplevel::ResizeEdge::TopLeft
                | xdg_toplevel::ResizeEdge::BottomLeft
        )
    }

    pub(crate) fn right(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Right
                | xdg_toplevel::ResizeEdge::TopRight
                | xdg_toplevel::ResizeEdge::BottomRight
        )
    }

    pub(crate) fn top(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Top
                | xdg_toplevel::ResizeEdge::TopLeft
                | xdg_toplevel::ResizeEdge::TopRight
        )
    }

    pub(crate) fn bottom(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Bottom
                | xdg_toplevel::ResizeEdge::BottomLeft
                | xdg_toplevel::ResizeEdge::BottomRight
        )
    }
}

pub(crate) fn constrained_size(
    initial: Size<i32, Logical>,
    delta: Point<f64, Logical>,
    edges: ResizeEdge,
    minimum: (i32, i32),
    maximum: (i32, i32),
) -> Size<i32, Logical> {
    let mut width = initial.w;
    let mut height = initial.h;
    if edges.left() {
        width = width.saturating_sub(delta.x as i32);
    } else if edges.right() {
        width = width.saturating_add(delta.x as i32);
    }

    if edges.top() {
        height = height.saturating_sub(delta.y as i32);
    } else if edges.bottom() {
        height = height.saturating_add(delta.y as i32);
    }

    // Protocol validation rejects these ranges, but a grab may still be alive
    // when its client is disconnected. Keep this path safe for any cached state.
    let minimum_width = minimum.0.max(1);
    let minimum_height = minimum.1.max(1);
    let maximum_width = if maximum.0 <= 0 {
        i32::MAX
    } else {
        maximum.0.max(minimum_width)
    };
    let maximum_height = if maximum.1 <= 0 {
        i32::MAX
    } else {
        maximum.1.max(minimum_height)
    };
    (
        width.clamp(minimum_width, maximum_width),
        height.clamp(minimum_height, maximum_height),
    )
        .into()
}

pub(crate) fn resized_rect(
    initial: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    edges: ResizeEdge,
) -> Rectangle<i32, Logical> {
    let mut location = initial.loc;

    if edges.left() {
        location.x = location
            .x
            .saturating_add(initial.size.w.saturating_sub(size.w));
    }

    if edges.top() {
        location.y = location
            .y
            .saturating_add(initial.size.h.saturating_sub(size.h));
    }

    Rectangle::new(location, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_grid_selects_edges_corners_and_center_fallback_once() {
        use xdg_toplevel::ResizeEdge::*;
        let rect = Rectangle::new((100, 50).into(), (300, 300).into());
        for (x, y, edge) in [
            (0.1, 0.1, TopLeft),
            (0.5, 0.1, Top),
            (0.9, 0.1, TopRight),
            (0.1, 0.5, Left),
            (0.5, 0.5, BottomRight),
            (0.9, 0.5, Right),
            (0.1, 0.9, BottomLeft),
            (0.5, 0.9, Bottom),
            (0.9, 0.9, BottomRight),
            (0.4, 0.4, TopLeft),
            (0.6, 0.4, TopRight),
            (0.4, 0.6, BottomLeft),
        ] {
            assert_eq!(
                ResizeEdge::at((100. + 300. * x, 50. + 300. * y).into(), rect),
                ResizeEdge(edge)
            );
        }
    }

    #[test]
    fn left_and_top_edges_invert_pointer_delta() {
        let size = constrained_size(
            (800, 600).into(),
            (100.0, 50.0).into(),
            ResizeEdge(xdg_toplevel::ResizeEdge::TopLeft),
            (1, 1),
            (0, 0),
        );
        assert_eq!(size, (700, 550).into());
    }

    #[test]
    fn right_and_bottom_edges_follow_pointer_delta() {
        let size = constrained_size(
            (800, 600).into(),
            (100.0, 50.0).into(),
            ResizeEdge(xdg_toplevel::ResizeEdge::BottomRight),
            (1, 1),
            (0, 0),
        );
        assert_eq!(size, (900, 650).into());
    }

    #[test]
    fn client_constraints_clamp_interactive_size() {
        let size = constrained_size(
            (800, 600).into(),
            (1_000.0, 1_000.0).into(),
            ResizeEdge(xdg_toplevel::ResizeEdge::BottomRight),
            (640, 480),
            (1_024, 768),
        );
        assert_eq!(size, (1_024, 768).into());
    }

    #[test]
    fn invalid_and_extreme_client_limits_cannot_panic_or_overflow() {
        for (minimum, maximum) in [
            ((800, 700), (400, 300)),
            ((-1, -2), (-3, -4)),
            ((i32::MAX, i32::MAX), (1, 1)),
            ((0, 0), (0, 0)),
        ] {
            for delta in [(f64::MAX, f64::MAX), (-f64::MAX, -f64::MAX), (0.0, 0.0)] {
                let size = constrained_size(
                    (800, 600).into(),
                    delta.into(),
                    ResizeEdge(xdg_toplevel::ResizeEdge::BottomRight),
                    minimum,
                    maximum,
                );
                assert!(size.w >= 1 && size.h >= 1);
                let _ = resized_rect(
                    Rectangle::new((100, 80).into(), (800, 600).into()),
                    size,
                    ResizeEdge(xdg_toplevel::ResizeEdge::TopLeft),
                );
            }
        }
    }

    #[test]
    fn resized_rect_keeps_the_opposite_edge_fixed() {
        let initial = Rectangle::new((100, 80).into(), (800, 600).into());
        let resized = resized_rect(
            initial,
            (700, 550).into(),
            ResizeEdge(xdg_toplevel::ResizeEdge::TopLeft),
        );

        assert_eq!(resized.loc, (200, 130).into());
        assert_eq!(resized.size, (700, 550).into());
    }
}
