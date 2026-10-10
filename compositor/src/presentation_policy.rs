//! Eligibility and callback ownership are policy, not presentation history.
use std::collections::HashMap;

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::layer_map_for_output;
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::get_parent;

use crate::Ferese;

fn mapped_tree(surface: &WlSurface) -> bool {
    let mut current = Some(surface.clone());
    while let Some(surface) = current {
        if !surface.is_alive()
            || !with_renderer_surface_state(&surface, |state| state.buffer().is_some()).unwrap_or(false)
        {
            return false;
        }
        current = get_parent(&surface);
    }
    true
}

impl Ferese {
    pub(crate) fn callback_outputs(&self) -> HashMap<Id, Output> {
        let mut surfaces = Vec::new();
        if self.session_lock.active() {
            for lock in self.session_lock.surfaces.values() {
                smithay::desktop::utils::with_surfaces_surface_tree(lock.wl_surface(), |surface, _| {
                    surfaces.push(surface.clone());
                });
            }
        } else {
            for window in self.space.elements() {
                window.with_surfaces(|surface, _| surfaces.push(surface.clone()));
            }
            for output in self.space.outputs() {
                for layer in layer_map_for_output(output).layers() {
                    layer.with_surfaces(|surface, _| surfaces.push(surface.clone()));
                }
            }
        }
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.cursor_status {
            surfaces.push(surface.clone());
        }

        surfaces
            .into_iter()
            .filter(mapped_tree)
            .filter_map(|surface| {
                let id = Id::from(&surface);
                let output = self
                    .display_presentation
                    .outputs_for(&id)
                    .max_by_key(|output| (output.current_mode().map_or(0, |mode| mode.refresh), output.name()))?
                    .clone();
                Some((id, output))
            })
            .collect()
    }
}
