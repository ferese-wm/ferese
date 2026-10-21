use smithay::desktop::{PopupKind, WindowSurfaceType, layer_map_for_output};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::input_method::{InputMethodHandler, PopupSurface};

use crate::Ferese;

impl InputMethodHandler for Ferese {
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(error) = self
            .popups
            .track_popup(PopupKind::InputMethod(surface.clone()))
        {
            tracing::warn!(?error, "failed to track input method popup");
        }
        crate::backends::direct::render_surface(self, surface.wl_surface());
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        let outputs = self.surface_outputs(surface.wl_surface());
        self.popups.cleanup();
        crate::backends::direct::render_on(self, &outputs);
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        crate::backends::direct::render_surface(self, surface.wl_surface());
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        if let Some(geometry) = self.space.elements().find_map(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == parent)
                .then(|| self.space.element_geometry(window))
                .flatten()
        }) {
            return geometry;
        }

        let Some(layer) = self
            .space
            .layer_for_surface(parent, WindowSurfaceType::TOPLEVEL)
        else {
            return Rectangle::default();
        };

        self.space
            .outputs()
            .find_map(|output| {
                let output_geometry = self.space.output_geometry(output)?;
                let layer_geometry = layer_map_for_output(output).layer_geometry(&layer)?;

                Some(Rectangle::new(
                    output_geometry.loc + layer_geometry.loc,
                    layer_geometry.size,
                ))
            })
            .unwrap_or_default()
    }
}
