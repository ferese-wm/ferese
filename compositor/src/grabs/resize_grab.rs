use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData,
    MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;

use crate::Ferese;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResizeEdge(xdg_toplevel::ResizeEdge);

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    fn from(edge: xdg_toplevel::ResizeEdge) -> Self {
        Self(edge)
    }
}

impl ResizeEdge {
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
    fn left(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Left | xdg_toplevel::ResizeEdge::TopLeft | xdg_toplevel::ResizeEdge::BottomLeft
        )
    }

    fn right(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Right
                | xdg_toplevel::ResizeEdge::TopRight
                | xdg_toplevel::ResizeEdge::BottomRight
        )
    }

    fn top(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Top | xdg_toplevel::ResizeEdge::TopLeft | xdg_toplevel::ResizeEdge::TopRight
        )
    }

    fn bottom(self) -> bool {
        matches!(
            self.0,
            xdg_toplevel::ResizeEdge::Bottom
                | xdg_toplevel::ResizeEdge::BottomLeft
                | xdg_toplevel::ResizeEdge::BottomRight
        )
    }
}

pub struct ResizeSurfaceGrab {
    start_data: GrabStartData<Ferese>,
    window: Window,
    edges: ResizeEdge,
    initial_rect: Rectangle<i32, Logical>,
    last_size: Size<i32, Logical>,
    finished: bool,
    snap_x: crate::floating::AxisSnap,
    snap_y: crate::floating::AxisSnap,
}

impl ResizeSurfaceGrab {
    pub fn new(
        start_data: GrabStartData<Ferese>,
        window: Window,
        edges: ResizeEdge,
        initial_rect: Rectangle<i32, Logical>,
    ) -> Self {
        Self {
            start_data,
            window,
            edges,
            initial_rect,
            last_size: initial_rect.size,
            finished: false,
            snap_x: Default::default(),
            snap_y: Default::default(),
        }
    }
}

impl PointerGrab<Ferese> for ResizeSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        let mut delta = event.location - self.start_data.location;
        let initial = self.initial_rect;
        let raw_left = initial.loc.x as f64 + if self.edges.left() { delta.x } else { 0. };
        let raw_top = initial.loc.y as f64 + if self.edges.top() { delta.y } else { 0. };
        let raw_right = initial.loc.x as f64 + initial.size.w as f64 + if self.edges.right() { delta.x } else { 0. };
        let raw_bottom = initial.loc.y as f64 + initial.size.h as f64 + if self.edges.bottom() { delta.y } else { 0. };
        let raw = ferese_layout::Rect::new(
            raw_left,
            raw_top,
            (raw_right - raw_left).max(1.),
            (raw_bottom - raw_top).max(1.),
        );
        let (xs, ys) = data.floating_snap_lines(&self.window, raw);
        if data.floating_snap_bypassed() {
            self.snap_x.clear();
            self.snap_y.clear();
        } else {
            if self.edges.left() || self.edges.right() {
                let moving = if self.edges.left() { raw_left } else { raw_right };
                delta.x += self.snap_x.apply(moving, 0., &xs) - moving;
            }
            if self.edges.top() || self.edges.bottom() {
                let moving = if self.edges.top() { raw_top } else { raw_bottom };
                delta.y += self.snap_y.apply(moving, 0., &ys) - moving;
            }
        }
        let surface = self.window.toplevel().expect("managed window has a toplevel");
        let (minimum, maximum) = with_states(surface.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let state = cached.current();
            (state.min_size, state.max_size)
        });
        let (minimum, maximum) = data.effective_size_constraints(
            &self.window,
            (minimum.w, minimum.h),
            (maximum.w, maximum.h),
        );
        self.last_size = constrained_size(
            self.initial_rect.size,
            delta,
            self.edges,
            minimum,
            (maximum.0, maximum.1),
        );
        let rect = resized_rect(self.initial_rect, self.last_size, self.edges);
        data.set_floating_window_geometry(&self.window, rect.loc, rect.size);
        if let Some(id) = data.windows.ids().get(&self.window).copied() {
            data.windows.update(id, |w| {
                w.resize_anchor = Some((
                    self.edges.left(),
                    self.edges.top(),
                    ferese_layout::Rect::new(
                        initial.loc.x as f64,
                        initial.loc.y as f64,
                        initial.size.w as f64,
                        initial.size.h as f64,
                    ),
                ))
            });
        }

        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(self.last_size);
        });
        surface.send_pending_configure();
    }

    fn relative_motion(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(&mut self, data: &mut Ferese, handle: &mut PointerInnerHandle<'_, Ferese>, event: &ButtonEvent) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            self.finished = true;
            data.remember_floating(&self.window);
            handle.unset_grab(self, data, event.serial, event.time, true);
            let surface = self.window.toplevel().expect("managed window has a toplevel");
            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
                state.size = Some(self.last_size);
            });
            surface.send_pending_configure();
        }
    }

    fn axis(&mut self, data: &mut Ferese, handle: &mut PointerInnerHandle<'_, Ferese>, frame: AxisFrame) {
        handle.axis(data, frame);
    }

    fn frame(&mut self, data: &mut Ferese, handle: &mut PointerInnerHandle<'_, Ferese>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<Ferese> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut Ferese) {
        if self.finished {
            return;
        }

        let surface = self.window.toplevel().expect("managed window has a toplevel");
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Resizing);
            state.size = Some(self.initial_rect.size);
        });
        surface.send_pending_configure();
        data.set_floating_window_geometry(&self.window, self.initial_rect.loc, self.initial_rect.size);
    }
}

fn constrained_size(
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

fn resized_rect(
    initial: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    edges: ResizeEdge,
) -> Rectangle<i32, Logical> {
    let mut location = initial.loc;

    if edges.left() {
        location.x = location.x.saturating_add(initial.size.w.saturating_sub(size.w));
    }

    if edges.top() {
        location.y = location.y.saturating_add(initial.size.h.saturating_sub(size.h));
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
