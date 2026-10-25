use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};

use crate::Ferese;

pub(crate) struct WindowPointerGrab {
    pub start_data: GrabStartData<Ferese>,
    pub finished: bool,
}

impl PointerGrab<Ferese> for WindowPointerGrab {
    fn motion(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        if !self.finished
            && let Some(operation) = data.pointer_operation.as_mut()
        {
            operation.location = event.location;
        }
    }

    fn relative_motion(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut Ferese,
        handle: &mut PointerInnerHandle<'_, Ferese>,
        event: &ButtonEvent,
    ) {
        if let Some(operation) = data.pointer_operation.as_mut() {
            if event.state == smithay::backend::input::ButtonState::Pressed {
                operation.pressed.insert(event.button);
                if event.button == 0x111 && event.button != self.start_data.button {
                    operation.toggle_drop_mode();
                }
            } else {
                operation.pressed.remove(&event.button);
            }
        }
        handle.button(data, event);
        if event.button == self.start_data.button
            && event.state == smithay::backend::input::ButtonState::Released
        {
            self.finished = true;
            if let Some(operation) = data.pointer_operation.as_mut() {
                operation.finished = true;
            }
        }
        // Extra buttons are drained with focus cleared, so their releases cannot
        // reach a client that never received the corresponding presses.
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, false);
        }
    }

    fn axis(
        &mut self,
        data: &mut Ferese,
        _handle: &mut PointerInnerHandle<'_, Ferese>,
        frame: AxisFrame,
    ) {
        if let Some(operation) = data.pointer_operation.as_mut() {
            let amount = if frame.axis.0 != 0.0 {
                frame.axis.0
            } else {
                frame.axis.1
            };
            operation.scroll(amount);
        }
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
        // Resolve focus after the layout settles and the pointer mutex is released.
        data.window_grab_focus_pending = true;
        if !self.finished
            && let Some(operation) = data.pointer_operation.as_mut()
        {
            operation.cancelled = true;
        }
    }
}
