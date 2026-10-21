//! Allocation hints retain the import global's renderer as a composition fallback.
use std::collections::HashMap;

use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufGlobal, SurfaceDmabufFeedbackState};

struct Surface {
    global: DmabufGlobal,
    fallback: DmabufFeedback,
}

#[derive(Default)]
pub(crate) struct FeedbackManager {
    globals: HashMap<DmabufGlobal, DmabufFeedback>,
    surfaces: HashMap<WlSurface, Surface>,
}

impl FeedbackManager {
    pub fn register(&mut self, global: DmabufGlobal, render: DmabufFeedback) {
        self.globals.insert(global, render);
    }

    pub fn new_surface(
        &mut self,
        surface: &WlSurface,
        global: DmabufGlobal,
    ) -> Option<DmabufFeedback> {
        let fallback = self.globals.get(&global)?.clone();
        self.surfaces.insert(
            surface.clone(),
            Surface {
                global,
                fallback: fallback.clone(),
            },
        );
        Some(fallback)
    }

    pub fn requested(&self) -> impl Iterator<Item = (&WlSurface, DmabufGlobal)> {
        self.surfaces
            .iter()
            .map(|(surface, record)| (surface, record.global))
    }

    pub fn refresh(&mut self, preferred: &HashMap<WlSurface, DmabufFeedback>) {
        self.surfaces.retain(|surface, _| surface.is_alive());
        for (surface, record) in &self.surfaces {
            let feedback = if self.globals.contains_key(&record.global) {
                preferred.get(surface).unwrap_or(&record.fallback)
            } else {
                &record.fallback
            };
            // Smithay owns subscriptions, protocol messages and equality suppression.
            with_states(surface, |states| {
                if let Some(state) = SurfaceDmabufFeedbackState::from_states(states) {
                    state.set_feedback(feedback);
                }
            });
        }
    }

    pub fn remove(&mut self, global: DmabufGlobal) {
        self.globals.remove(&global);
        for (surface, record) in self
            .surfaces
            .iter()
            .filter(|(_, record)| record.global == global)
        {
            // V4 requires a main-device tranche. Retain last-known render
            // capabilities, withdraw scanout hints and retire the global. Bound
            // resources cannot import through the retired global or another GPU.
            with_states(surface, |states| {
                if let Some(state) = SurfaceDmabufFeedbackState::from_states(states) {
                    state.set_feedback(&record.fallback);
                }
            });
        }
    }

    pub fn destroyed(&mut self, surface: &WlSurface) {
        self.surfaces.remove(surface);
    }
}

#[cfg(test)]
mod tests;
