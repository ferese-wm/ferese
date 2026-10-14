use smithay::desktop::{LayerSurface as DesktopLayerSurface, PopupKind, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::PopupSurface;

use crate::Ferese;

impl WlrLayerShellHandler for Ferese {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(&mut self, surface: LayerSurface, output: Option<WlOutput>, _layer: Layer, namespace: String) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            tracing::warn!(%namespace, "layer surface has no available output");
            return;
        };
        let layer = DesktopLayerSurface::new(surface, namespace.clone());

        if let Err(error) = layer_map_for_output(&output).map_layer(&layer) {
            tracing::warn!(%namespace, %error, "failed to map layer surface");
            return;
        }

        tracing::debug!(%namespace, output = %output.name(), "mapped layer surface");
        super::set_surface_tree_output(layer.wl_surface(), &output);
        self.relayout_on(&[output]);
    }

    fn new_popup(&mut self, parent: LayerSurface, popup: PopupSurface) {
        let output = self.space.outputs().find(|output| {
            layer_map_for_output(output)
                .layers()
                .any(|layer| layer.wl_surface() == parent.wl_surface())
        });
        if let Some(output) = output {
            super::set_surface_tree_output(popup.wl_surface(), output);
        }
        self.unconstrain_popup(&popup);
        if let Err(error) = self.popups.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!(?error, "failed to track layer-shell popup");
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let Some(layer) = self
            .space
            .layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL)
        else {
            return;
        };
        let restore_focus = layer_has_keyboard_focus(self, &layer);
        let outputs = self
            .space
            .outputs()
            .filter(|output| layer_map_for_output(output).layer_geometry(&layer).is_some())
            .cloned()
            .collect::<Vec<_>>();

        for output in &outputs {
            layer_map_for_output(output).unmap_layer(&layer);
        }

        self.relayout_on(&outputs);
        if restore_focus {
            self.restore_keyboard_focus();
        }
        crate::backends::direct::render_on(self, &outputs);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct LayoutState {
    size: smithay::utils::Size<i32, smithay::utils::Logical>,
    anchor: smithay::wayland::shell::wlr_layer::Anchor,
    zone: smithay::wayland::shell::wlr_layer::ExclusiveZone,
    margin: [i32; 4],
    layer: Layer,
    bounds: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
}

impl LayoutState {
    fn current(layer: &DesktopLayerSurface) -> Self {
        Self::from_policy(layer.cached_state(), layer.bbox())
    }

    fn from_policy(
        policy: smithay::wayland::shell::wlr_layer::LayerSurfaceCachedState,
        bounds: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> Self {
        Self {
            size: policy.size,
            anchor: policy.anchor,
            zone: policy.exclusive_zone,
            margin: [
                policy.margin.top,
                policy.margin.right,
                policy.margin.bottom,
                policy.margin.left,
            ],
            layer: policy.layer,
            bounds,
        }
    }
}

pub fn handle_commit(state: &mut Ferese, surface: &WlSurface) {
    let Some(layer) = state.space.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) else {
        return;
    };
    let outputs = state
        .space
        .outputs()
        .filter(|output| layer_map_for_output(output).layer_geometry(&layer).is_some())
        .cloned()
        .collect::<Vec<_>>();

    let policy = layer.cached_state();
    let current = LayoutState::current(&layer);
    let previous = layer
        .user_data()
        .get_or_insert_threadsafe(|| std::sync::Mutex::new(None::<(LayoutState, KeyboardInteractivity)>));
    let (changed, requested_focus) = {
        let mut previous = previous.lock().unwrap();
        let changed = previous.as_ref().map(|(layout, _)| layout) != Some(&current);
        let requested_focus = policy.keyboard_interactivity == KeyboardInteractivity::OnDemand
            && previous
                .as_ref()
                .is_some_and(|(_, keyboard)| *keyboard == KeyboardInteractivity::None);
        *previous = Some((current, policy.keyboard_interactivity));
        (changed, requested_focus)
    };

    if changed {
        state.relayout_on(&outputs);
    }
    // Configure acknowledgement and keyboard policy are independent of layout.
    layer.layer_surface().send_pending_configure();

    // A click can enable keyboard interaction after its button event was
    // delivered. Give that layer focus without requiring a second click.
    let pointer_on_layer = requested_focus
        && !state.seat.get_keyboard().is_some_and(|keyboard| keyboard.is_grabbed())
        && state
            .seat
            .get_pointer()
            .and_then(|pointer| pointer.current_focus())
            .is_some_and(|surface| {
                state
                    .space
                    .layer_for_surface(&surface, WindowSurfaceType::ALL)
                    .is_some_and(|focused| focused == layer)
            });
    state.reconcile_layer_keyboard_focus();
    if pointer_on_layer
        && !state.session_lock.active()
        && !state.input_capture.captures(1)
        && state.layer_keyboard_eligible(&layer)
        && state.exclusive_keyboard_layer().is_none()
        && matches!(policy.layer, Layer::Top | Layer::Overlay)
        && !layer_has_keyboard_focus(state, &layer)
    {
        state.seat.get_keyboard().expect("seat has a keyboard").set_focus(
            state,
            Some(layer.wl_surface().clone()),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
    }

    if let Err(error) = state.display_handle.flush_clients() {
        tracing::debug!(%error, "failed to flush layer-surface configure");
    }
}

impl Ferese {
    /// Layer-map membership tracks the role and output, not buffer mapping.
    /// Use the renderer state updated by on_commit_buffer_handler, as for windows.
    pub(crate) fn layer_keyboard_eligible(&self, layer: &DesktopLayerSurface) -> bool {
        layer.layer_surface().alive()
            && layer.can_receive_keyboard_focus()
            && smithay::backend::renderer::utils::with_renderer_surface_state(layer.wl_surface(), |state| {
                state.buffer().is_some()
            })
            .unwrap_or(false)
            && self.space.outputs().any(|output| {
                self.layer_visible_on_output(output, layer.layer())
                    && layer_map_for_output(output)
                        .layers()
                        .any(|candidate| candidate == layer)
            })
    }

    fn keyboard_layer_root(&self) -> Option<WlSurface> {
        let surface = self.seat.get_keyboard()?.current_focus()?;
        let root = self
            .popups
            .find_popup(&surface)
            .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok())
            .unwrap_or(surface);
        (smithay::wayland::compositor::get_role(&root) == Some(smithay::wayland::shell::wlr_layer::LAYER_SURFACE_ROLE))
            .then_some(root)
    }

    pub(crate) fn keyboard_focused_layer(&self) -> Option<DesktopLayerSurface> {
        self.space
            .layer_for_surface(&self.keyboard_layer_root()?, WindowSurfaceType::ALL)
    }

    pub(crate) fn exclusive_keyboard_layer(&self) -> Option<DesktopLayerSurface> {
        // Overlay outranks Top. Keep the current owner among peers; the protocol
        // leaves ordering within one layer to the compositor.
        let focused = self.keyboard_focused_layer();
        for priority in [Layer::Overlay, Layer::Top] {
            if let Some(layer) = focused.as_ref().filter(|layer| {
                layer.layer() == priority
                    && layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && self.layer_keyboard_eligible(layer)
            }) {
                return Some(layer.clone());
            }
            for output in self.space.outputs() {
                let layers = layer_map_for_output(output)
                    .layers_on(priority)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(layer) = layers.into_iter().find(|layer| {
                    layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                        && self.layer_keyboard_eligible(layer)
                }) {
                    return Some(layer.clone());
                }
            }
        }
        None
    }

    pub(crate) fn reconcile_layer_keyboard_focus(&mut self) {
        if self.session_lock.active() || self.input_capture.captures(1) {
            return;
        }
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        if let Some(root) = self.keyboard_layer_root().filter(|_| {
            !self
                .keyboard_focused_layer()
                .is_some_and(|layer| self.layer_keyboard_eligible(&layer))
        }) {
            // A popup grab cannot keep an ineligible layer alive as input owner.
            for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&root) {
                let _ = smithay::desktop::PopupManager::dismiss_popup(&root, &popup);
            }
            keyboard.unset_grab(self);
            self.restore_keyboard_focus();
        } else if !keyboard.is_grabbed()
            && let Some(layer) = self.exclusive_keyboard_layer()
            && !layer_has_keyboard_focus(self, &layer)
        {
            keyboard.set_focus(
                self,
                Some(layer.wl_surface().clone()),
                smithay::utils::SERIAL_COUNTER.next_serial(),
            );
        }
    }
}

fn layer_has_keyboard_focus(state: &Ferese, layer: &DesktopLayerSurface) -> bool {
    state.keyboard_focused_layer().is_some_and(|focused| focused == *layer)
}

#[cfg(test)]
mod tests {
    use smithay::utils::Rectangle;
    use smithay::wayland::shell::wlr_layer::{Anchor, ExclusiveZone, LayerSurfaceCachedState};

    use super::*;

    #[test]
    fn only_arrangement_and_mapping_changes_require_layout() {
        let policy = LayerSurfaceCachedState::default();
        let bounds = Rectangle::from_size((800, 32).into());
        let original = LayoutState::from_policy(policy, bounds);
        assert_eq!(original, LayoutState::from_policy(policy, bounds));

        let mut keyboard = policy;
        keyboard.keyboard_interactivity = KeyboardInteractivity::Exclusive;
        assert_eq!(original, LayoutState::from_policy(keyboard, bounds));

        for mutate in [
            |p: &mut LayerSurfaceCachedState| p.size.w += 1,
            |p: &mut LayerSurfaceCachedState| p.anchor = Anchor::TOP,
            |p: &mut LayerSurfaceCachedState| p.exclusive_zone = ExclusiveZone::Exclusive(32),
            |p: &mut LayerSurfaceCachedState| p.margin.left += 1,
            |p: &mut LayerSurfaceCachedState| p.layer = Layer::Overlay,
        ] {
            let mut changed = policy;
            mutate(&mut changed);
            assert_ne!(original, LayoutState::from_policy(changed, bounds));
        }
        assert_ne!(
            original,
            LayoutState::from_policy(policy, Rectangle::from_size((800, 33).into()))
        );
        assert_ne!(original, LayoutState::from_policy(policy, Rectangle::default()));
    }
}
