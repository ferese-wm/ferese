use ferese_core::OutputId;
#[cfg(test)]
use smithay::backend::allocator::Fourcc;
#[cfg(test)]
use smithay::backend::renderer::Offscreen;
#[cfg(test)]
use smithay::backend::renderer::element::Id;
#[cfg(test)]
use smithay::backend::renderer::gles::GlesRenderer;
#[cfg(test)]
use smithay::backend::renderer::gles::GlesTexture;
#[cfg(test)]
use smithay::backend::renderer::utils::CommitCounter;

use ferese_layout::WindowId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use super::{
    BlurProgram, ClosedWindow, CornerShape, ErasedContextId, HashMap, MaterialBuffers,
    MaterialProgram, OverviewChromePart, OverviewScrim, ResizeSnapshot, RoundedClipPrograms,
    WindowBorderBuffers, WindowShadowBuffers,
};

/// Resource ownership follows the lifetime of a window, output, surface or GL
/// context. Desktop policy and animation values do not live in these caches.
#[derive(Default)]
pub(crate) struct RenderResources {
    pub closing: Vec<ClosedWindow>,
    pub(super) windows: HashMap<WindowId, WindowBuffers>,
    pub(super) contexts: HashMap<ErasedContextId, ContextPrograms>,
    pub(super) outputs: HashMap<OutputId, OverviewScrim>,
    pub(super) surfaces: HashMap<WlSurface, MaterialBuffers>,
}

#[derive(Default)]
pub(super) struct WindowBuffers {
    pub borders: WindowBorderBuffers,
    pub dim: WindowBorderBuffers,
    pub resize_fill: WindowBorderBuffers,
    pub blur: HashMap<(ErasedContextId, OutputId), super::window_blur::WindowBlur>,
    pub shadow: WindowShadowBuffers,
    corner_shape: Option<CornerShape>,
    thumbnail_shapes: HashMap<OutputId, CornerShape>,
    snapshot: Option<ResizeSnapshot>,
}

#[derive(Default)]
pub(super) struct ContextPrograms {
    pub rounded: Option<RoundedClipPrograms>,
    pub rounded_warned: bool,
    pub continuous: Option<RoundedClipPrograms>,
    pub continuous_warned: bool,
    pub window_material: Option<MaterialProgram>,
    pub material: Option<MaterialProgram>,
    pub blur: Option<BlurProgram>,
}

impl RenderResources {
    pub(crate) fn retain_workspace_windows(
        &mut self,
        windows: &std::collections::HashSet<WindowId>,
    ) {
        self.windows.retain(|id, _| windows.contains(id));
        for output in self.outputs.values_mut() {
            output
                .chrome
                .retain(|(id, _), _| windows.contains(&WindowId(*id)));
        }
    }

    pub(super) fn prepare_window_corners(&mut self, id: WindowId, shape: CornerShape) -> bool {
        let buffers = self.windows.entry(id).or_default();

        let changed = buffers.corner_shape.is_some_and(|old| old != shape);

        if changed {
            buffers.borders.contexts.clear();
            buffers.dim.contexts.clear();
            buffers.resize_fill.contexts.clear();
            buffers.shadow.contexts.clear();
            buffers.blur.clear();

            if let Some(snapshot) = &mut buffers.snapshot {
                snapshot.commit.increment();
            }
        }

        buffers.corner_shape = Some(shape);
        changed
    }

    pub(super) fn thumbnail_corner_changed(
        &mut self,
        id: WindowId,
        output: OutputId,
        shape: CornerShape,
    ) -> bool {
        self.windows
            .entry(id)
            .or_default()
            .thumbnail_shapes
            .insert(output, shape)
            .is_none_or(|previous| previous != shape)
    }

    pub fn remove_window(&mut self, id: WindowId) {
        self.windows.remove(&id);
        for output in self.outputs.values_mut() {
            output.chrome.retain(|(key, part), _| {
                *key != id.0
                    || !matches!(
                        part,
                        OverviewChromePart::Tab
                            | OverviewChromePart::TabActive
                            | OverviewChromePart::TabRail
                            | OverviewChromePart::TabListOutline
                            | OverviewChromePart::TabList
                            | OverviewChromePart::TabRow
                            | OverviewChromePart::TabFlash
                    )
            });
        }
    }

    pub fn remove_output(&mut self, id: OutputId) {
        let removed: Vec<_> = self
            .closing
            .iter()
            .filter(|window| window.output == id)
            .map(|window| window.presentation.id)
            .collect();
        self.closing.retain(|window| window.output != id);
        for id in removed {
            self.remove_window(id);
        }
        self.outputs.remove(&id);

        for window in self.windows.values_mut() {
            window.thumbnail_shapes.remove(&id);
            window.blur.retain(|(_, output), _| *output != id);
        }

        for surface in self.surfaces.values_mut() {
            surface.contexts.retain(|(_, output, _), _| *output != id);
            surface.captures.retain(|(_, output, _), _| *output != id);
        }
    }

    pub fn snapshot(&self, id: &WindowId) -> Option<&ResizeSnapshot> {
        self.windows.get(id)?.snapshot.as_ref()
    }

    pub fn snapshots(&self) -> impl Iterator<Item = &ResizeSnapshot> {
        self.windows
            .values()
            .filter_map(|window| window.snapshot.as_ref())
    }

    pub fn set_snapshot(&mut self, id: WindowId, snapshot: ResizeSnapshot) {
        self.windows.entry(id).or_default().snapshot = Some(snapshot);
    }

    pub fn clear_snapshot(&mut self, id: &WindowId) {
        if let Some(window) = self.windows.get_mut(id) {
            window.snapshot = None;
        }
    }

    pub fn retain_snapshots(
        &mut self,
        mut keep: impl FnMut(&WindowId, &mut ResizeSnapshot) -> bool,
    ) {
        for (id, window) in &mut self.windows {
            if let Some(snapshot) = &mut window.snapshot
                && !keep(id, snapshot)
            {
                window.snapshot = None;
            }
        }
    }

    pub(super) fn clear_tint(&mut self, id: WindowId, resize_fill: bool) {
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };

        if resize_fill {
            window.resize_fill.contexts.clear();
        } else {
            window.dim.contexts.clear();
        }
    }

    pub fn forget_context(&mut self, context: &ErasedContextId) {
        let removed: Vec<_> = self
            .closing
            .iter()
            .filter(|window| &window.snapshot.context == context)
            .map(|window| window.presentation.id)
            .collect();
        self.closing
            .retain(|window| &window.snapshot.context != context);
        for id in removed {
            self.remove_window(id);
        }
        self.contexts.remove(context);

        for window in self.windows.values_mut() {
            window.borders.contexts.remove(context);
            window.dim.contexts.remove(context);
            window.resize_fill.contexts.remove(context);
            window.shadow.contexts.remove(context);
            window.blur.retain(|(key, _), _| key != context);

            if window
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| &snapshot.context == context)
            {
                window.snapshot = None;
            }
        }

        for output in self.outputs.values_mut() {
            for contexts in output.chrome.values_mut() {
                contexts.remove(context);
            }
        }

        for surface in self.surfaces.values_mut() {
            surface.contexts.retain(|(id, _, _), _| id != context);
            surface.captures.retain(|(id, _, _), _| id != context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::blur_program;
    use crate::render::material_program;
    use crate::render::rounded_clip_program;
    use smithay::backend::renderer::Renderer;
    use std::time::Duration;

    #[test]
    fn window_removal_releases_all_its_caches_without_removing_other_windows() {
        let mut resources = RenderResources::default();
        resources
            .windows
            .insert(WindowId(1), WindowBuffers::default());
        resources
            .windows
            .insert(WindowId(2), WindowBuffers::default());

        resources.remove_window(WindowId(1));
        resources.clear_snapshot(&WindowId(1));
        resources.clear_tint(WindowId(1), true);

        assert!(!resources.windows.contains_key(&WindowId(1)));
        assert!(resources.windows.contains_key(&WindowId(2)));
    }

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn context_removal_evicts_programs_and_snapshots_without_touching_another_gpu() {
        use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};

        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL device");
        // SAFETY: The native display comes from a live enumerated EGLDevice and its lifetime is
        // retained by the EGL objects created here.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues
        // GL work only through its renderer methods.
        let mut first = unsafe { GlesRenderer::new(EGLContext::new(&display).unwrap()).unwrap() };
        // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues
        // GL work only through its renderer methods.
        let mut second = unsafe { GlesRenderer::new(EGLContext::new(&display).unwrap()).unwrap() };
        let first_id = first.context_id().erased();
        let second_id = second.context_id().erased();
        let mut resources = RenderResources::default();

        for (id, renderer) in [(WindowId(1), &mut first), (WindowId(2), &mut second)] {
            rounded_clip_program(&mut resources, renderer).unwrap();
            material_program(&mut resources, renderer).unwrap();
            blur_program(&mut resources, renderer).unwrap();
            let texture = Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                (16, 16).into(),
            )
            .unwrap();
            resources.set_snapshot(
                id,
                ResizeSnapshot {
                    texture,
                    context: renderer.context_id().erased(),
                    id: Id::new(),
                    commit: CommitCounter::default(),
                    elapsed: Duration::ZERO,
                    last_tick: None,
                    scale: 1.0,
                },
            );
        }

        resources.forget_context(&first_id);

        assert!(!resources.contexts.contains_key(&first_id));
        assert!(resources.contexts.contains_key(&second_id));
        assert!(resources.snapshot(&WindowId(1)).is_none());
        assert!(resources.snapshot(&WindowId(2)).is_some());
        assert_eq!(resources.snapshots().count(), 1);

        // Reconnecting a context can build fresh resources after eviction.
        rounded_clip_program(&mut resources, &mut first).unwrap();
        assert!(resources.contexts.contains_key(&first_id));
    }
}
