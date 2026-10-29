use smithay::input::Seat;
use smithay::input::dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source};
use smithay::input::pointer::Focus;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::selection::data_device::WaylandDndGrabHandler;

use crate::Ferese;

pub(crate) struct Drag {
    kind: GrabType,
}

impl WaylandDndGrabHandler for Ferese {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        kind: GrabType,
    ) {
        if seat != self.seat
            || self.drag.is_some()
            || self.session_lock.active()
            || self.input_capture.active()
        {
            source.cancel();
            return;
        }
        self.drag = Some(Drag { kind });
        match kind {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().expect("validated pointer drag");
                let start = pointer.grab_start_data().expect("validated pointer grab");
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let touch = seat.get_touch().expect("validated touch drag");
                let start = touch.grab_start_data().expect("validated touch grab");
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start, source, seat),
                    serial,
                );
            }
        }
    }
}

impl DndGrabHandler for Ferese {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.drag = None;
        self.cursor_redraw_pending = true;
    }

    fn cancelled(&mut self, _seat: Seat<Self>, _location: Point<f64, Logical>) {
        self.drag = None;
        self.cursor_redraw_pending = true;
    }
}

impl Ferese {
    pub(crate) fn pointer_drag_active(&self) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|drag| drag.kind == GrabType::Pointer)
    }

    pub(crate) fn touch_drag_active(&self) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|drag| drag.kind == GrabType::Touch)
    }

    pub(crate) fn cancel_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        match drag.kind {
            GrabType::Pointer => {
                if let Some(pointer) = self.seat.get_pointer() {
                    pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), self.last_pointer_time);
                }
            }
            GrabType::Touch => {
                if let Some(touch) = self.seat.get_touch() {
                    touch.unset_grab(self);
                }
            }
        }
        self.cursor_redraw_pending = true;
        true
    }

    /// Source destruction and null-source origin destruction can occur without
    /// another input event. Retire their grabs in the event-loop epilogue.
    pub(crate) fn poll_drag(&mut self) {
        let Some(drag) = &self.drag else {
            return;
        };
        let alive = match drag.kind {
            GrabType::Pointer => self
                .seat
                .get_pointer()
                .and_then(|pointer| {
                    pointer.with_grab(|_, grab| {
                        grab.downcast_ref::<DnDGrab<Self, WlDataSource, WlSurface>>()
                            .map(|grab| grab.source().alive())
                            .or_else(|| {
                                grab.downcast_ref::<DnDGrab<Self, WlSurface, WlSurface>>()
                                    .map(|grab| grab.source().alive())
                            })
                    })
                })
                .flatten(),
            GrabType::Touch => self
                .seat
                .get_touch()
                .and_then(|touch| {
                    touch.with_grab(|_, grab| {
                        grab.downcast_ref::<DnDGrab<Self, WlDataSource, WlSurface>>()
                            .map(|grab| grab.source().alive())
                            .or_else(|| {
                                grab.downcast_ref::<DnDGrab<Self, WlSurface, WlSurface>>()
                                    .map(|grab| grab.source().alive())
                            })
                    })
                })
                .flatten(),
        };
        if alive != Some(true) {
            self.cancel_drag();
        }
    }
}

#[cfg(test)]
mod tests;
