//! Transactional client presentation hints, advertised only with an enabled DRM path.
use std::cell::RefCell;

use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::{
    wp_tearing_control_manager_v1::{self, WpTearingControlManagerV1},
    wp_tearing_control_v1::{self, WpTearingControlV1},
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum, Weak,
};
use smithay::wayland::compositor::{Cacheable, with_states};
use smithay::wayland::fifo::FifoCachedState;

use crate::Ferese;

#[derive(Default)]
pub(crate) struct TearingControl {
    global: Option<GlobalId>,
}

#[derive(Clone, Copy, Default)]
struct Hint {
    asynchronous: bool,
}

impl Cacheable for Hint {
    fn commit(&mut self, _display: &DisplayHandle) -> Self {
        *self
    }
    fn merge_into(self, current: &mut Self, _display: &DisplayHandle) {
        *current = self;
    }
}

#[derive(Default)]
struct Controller(RefCell<Option<WpTearingControlV1>>);

pub(crate) struct SurfaceData(Weak<WlSurface>);

pub(crate) fn sync(state: &mut Ferese) {
    let enabled = crate::backends::direct::supports_tearing(state);
    match (enabled, state.tearing_control.global.is_some()) {
        (true, false) => {
            state.tearing_control.global = Some(
                state
                    .display_handle
                    .create_global::<Ferese, WpTearingControlManagerV1, _>(1, ()),
            );
        }
        (false, true) => {
            if let Some(global) = state.tearing_control.global.take() {
                state.display_handle.disable_global::<Ferese>(global);
            }
        }
        _ => {}
    }
}

pub(crate) fn surface_allows_tearing(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        let hint = states.cached_state.get::<Hint>().current().asynchronous;
        let fifo = *states.cached_state.get::<FifoCachedState>().current();
        hint && !fifo.wait_barrier && !fifo.set_barrier
    })
}

pub(crate) fn fullscreen_surface(state: &Ferese, output: &Output) -> Option<WlSurface> {
    let workspace = state
        .output_id(output)
        .and_then(|id| state.output_workspaces.active_workspace(id))
        .and_then(|workspace| state.workspaces.workspace(workspace))?;
    state
        .windows
        .window(workspace.fullscreen?)?
        .toplevel()
        .map(|toplevel| toplevel.wl_surface().clone())
}

impl GlobalDispatch<WpTearingControlManagerV1, ()> for Ferese {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpTearingControlManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<WpTearingControlManagerV1, ()> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } => {
                let duplicate = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<Controller>()
                        .is_some_and(|controller| {
                            controller
                                .0
                                .borrow()
                                .as_ref()
                                .is_some_and(Resource::is_alive)
                        })
                });
                if duplicate {
                    resource.post_error(
                        wp_tearing_control_manager_v1::Error::TearingControlExists,
                        "surface already has a tearing controller",
                    );
                    return;
                }
                let controller = init.init(id, SurfaceData(surface.downgrade()));
                with_states(&surface, |states| {
                    *states
                        .data_map
                        .get_or_insert(Controller::default)
                        .0
                        .borrow_mut() = Some(controller);
                });
            }
            wp_tearing_control_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<WpTearingControlV1, SurfaceData> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &SurfaceData,
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        let Ok(surface) = data.0.upgrade() else {
            return;
        };
        match request {
            wp_tearing_control_v1::Request::SetPresentationHint { hint } => {
                with_states(&surface, |states| {
                    // Unknown future hint values fail closed to synchronized presentation.
                    states.cached_state.get::<Hint>().pending().asynchronous = matches!(
                        hint,
                        WEnum::Value(wp_tearing_control_v1::PresentationHint::Async)
                    );
                });
            }
            wp_tearing_control_v1::Request::Destroy => {
                with_states(&surface, |states| {
                    states.cached_state.get::<Hint>().pending().asynchronous = false;
                    if let Some(controller) = states.data_map.get::<Controller>() {
                        controller.0.borrow_mut().take();
                    }
                });
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
#[path = "tearing_control_tests.rs"]
mod tests;
