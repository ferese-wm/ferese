use super::*;
use smithay::wayland::compositor::{BufferAssignment, SurfaceAttributes};

impl Ferese {
    pub(crate) fn capture_close_before_commit(&mut self, surface: &WlSurface) {
        // At this point cached attributes contain the committed removal, but
        // on_commit_buffer_handler has not reset the previous renderer state.
        let removed = with_states(surface, |states| {
            matches!(
                states.cached_state.get::<SurfaceAttributes>().current().buffer,
                Some(BufferAssignment::Removed)
            )
        });
        if removed
            && let Some(window) = self
                .windows
                .id_for_surface(surface)
                .and_then(|id| self.windows.window(id))
                .cloned()
        {
            self.retain_closed_window(&window);
        }
    }

    pub(super) fn retain_closed_window(&mut self, window: &Window) {
        if !self.animations_enabled || self.session_lock.active() || self.render.closing.len() >= 64 {
            return;
        }
        let Some(&id) = self.windows.ids().get(window) else {
            return;
        };
        if self.render.closing.iter().any(|window| window.presentation.id == id) || !self.window_content_ready(window) {
            return;
        }
        let Some(output) = self
            .space
            .outputs()
            .find(|output| self.window_belongs_to_output(id, output))
            .cloned()
        else {
            return;
        };
        let Some(output_id) = self.output_id(&output) else {
            return;
        };
        let Some(mut presentation) = self.current_window_presentation(id) else {
            return;
        };
        let record = self.windows.record(id).unwrap();
        let geometry = record.geometry.unwrap();
        let decorations = geometry.decorations.clamp(0.0, 1.0);
        let handoff_size = (!presentation.scale_content).then(|| {
            presentation
                .native_size
                .unwrap_or_else(|| ClientSize::from_rect(presentation.bounds.current))
        });
        presentation.close();
        let mut source_geometry = window.geometry();
        if !self.overview.is_presenting() {
            let size = ClientSize::from_rect(geometry.visual.current);
            source_geometry.size = (size.width, size.height).into();
        }
        let order: Vec<_> = if self.overview.is_active() {
            self.windows
                .overview_windows()
                .filter_map(|window| self.windows.ids().get(window).copied())
                .collect()
        } else {
            self.space
                .elements()
                .rev()
                .filter_map(|window| self.windows.ids().get(window).copied())
                .collect()
        };
        let below = order
            .iter()
            .position(|candidate| *candidate == id)
            .and_then(|index| order.get(index + 1))
            .copied();
        let used = self.render.snapshots().map(|snapshot| snapshot.bytes()).sum::<usize>()
            + self.render.closing.iter().map(|window| window.bytes()).sum::<usize>();
        let remaining = crate::presentation::SNAPSHOT_BUDGET.saturating_sub(used);
        let scale = output.current_scale().fractional_scale();
        let result = if let Some(backend) = &self.nested_backend {
            match backend.try_borrow_mut() {
                Ok(mut backend) => crate::render::capture_resize_snapshot(
                    backend.renderer(),
                    window,
                    source_geometry,
                    scale,
                    remaining,
                ),
                Err(_) => return,
            }
        } else if let Some(backend) = &mut self.direct_backend {
            backend.capture_resize_snapshot(window, source_geometry, &output, remaining)
        } else {
            return;
        };
        match result {
            Ok(Some(snapshot)) => {
                tracing::debug!(?id, bytes = snapshot.bytes(), "retained close presentation");
                let handoff = self
                    .render
                    .snapshot(&id)
                    .filter(|old| old.context == snapshot.context && (old.scale - scale).abs() < 0.001)
                    .cloned()
                    .map(|old| (old, handoff_size));
                let material = window.toplevel().and_then(|toplevel| {
                    let surface = toplevel.wl_surface();
                    let (role, generation) = crate::effects::surface_role(surface)?;
                    (role == crate::effects::SemanticRole::Modal).then(|| {
                        (
                            surface.clone(),
                            role,
                            generation,
                            crate::effects::surface_opacity(surface),
                        )
                    })
                });
                let radius = if material.is_some() {
                    self.theme_settings.material_radius
                } else {
                    self.theme_settings.window_radius
                } * decorations;
                let client = window.geometry().size;
                let fill = (!self.overview.is_presenting()
                    && material.is_none()
                    && (client.w < source_geometry.size.w || client.h < source_geometry.size.h))
                    .then_some(self.theme_settings.surface_base_color.0);
                self.render.closing.push(crate::render::ClosedWindow {
                    presentation,
                    output: output_id,
                    below,
                    snapshot,
                    handoff,
                    radius,
                    shape: crate::render::window_corner_shape(window),
                    decorations,
                    fill,
                    material,
                });
                self.last_animation_tick = Instant::now();
                self.defer_output_redraw(output);
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(%error, ?id, "close snapshot unavailable"),
        }
    }
}
