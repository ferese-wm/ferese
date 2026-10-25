//! Workspace capture owns a view, not a second desktop. Targets exist only for
//! subscribed sources; surface commits invalidate only their owning workspace.
use std::time::{Duration, Instant};

use ferese_core::WorkspaceId;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::utils::{Buffer, Logical, Rectangle, Size, Transform};

use crate::Ferese;
use crate::backends::direct::capture::CaptureTarget;

#[derive(Clone)]
pub(crate) struct WorkspaceSource {
    pub id: WorkspaceId,
    pub output: Output,
    pub render_output: Output,
    native_size: Size<i32, smithay::utils::Physical>,
    pub geometry: Rectangle<i32, Logical>,
    pub size: Size<i32, Buffer>,
    scale: f64,
    transform: Transform,
}

impl WorkspaceSource {
    pub fn new(state: &Ferese, id: WorkspaceId) -> Option<Self> {
        state.workspaces.workspace(id)?;
        let owner = state.output_workspaces.output_for_workspace(id)?;
        let output = state
            .output_ids
            .iter()
            .find(|(_, id)| **id == owner)?
            .0
            .clone();
        let native_size = output.current_mode()?.size;
        let geometry = state.space.output_geometry(&output)?;
        if native_size.w <= 0 || native_size.h <= 0 || geometry.size.w <= 0 || geometry.size.h <= 0
        {
            return None;
        }
        let scale = output.current_scale().fractional_scale();
        let (size, capture_scale) = capture_size(geometry.size, scale);
        // A render descriptor only: never registered, mapped, advertised, or
        // attached to input/layout. It gives shaders a bounded upright canvas.
        let render_output = Output::new(
            format!("workspace-capture-{}", id.0),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        render_output.change_current_state(
            Some(smithay::output::Mode {
                size: (size.w, size.h).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            Some(smithay::output::Scale::Fractional(capture_scale)),
            None,
        );
        render_output
            .user_data()
            .insert_if_missing(|| CaptureOutputId(owner));
        Some(Self {
            id,
            output: output.clone(),
            render_output,
            geometry,
            size,
            native_size,
            scale,
            transform: output.current_transform(),
        })
    }

    pub fn valid(&self, state: &Ferese) -> bool {
        state.workspaces.workspace(self.id).is_some()
            && state.output_id(&self.output).is_some_and(|owner| {
                state.output_workspaces.output_for_workspace(self.id) == Some(owner)
            })
            && state.space.output_geometry(&self.output) == Some(self.geometry)
            && self
                .output
                .current_mode()
                .is_some_and(|mode| mode.size == self.native_size)
            && self.output.current_scale().fractional_scale() == self.scale
            && self.output.current_transform() == self.transform
    }

    pub fn cursor_visible(&self, state: &Ferese) -> bool {
        !state.overview.is_presenting()
            && !state.input_capture.active()
            && state
                .output_id(&self.output)
                .is_some_and(|output| state.workspace_capture_visible(self.id, output))
            && state.seat.get_pointer().is_some_and(|pointer| {
                self.geometry
                    .contains(pointer.current_location().to_i32_floor())
            })
            && !state.capture_cursor_protected()
    }
}

pub(super) struct WorkspaceCapture {
    pub source: WorkspaceSource,
    pub content: u64,
    pub prepared: Option<u64>,
    pub cursor: Option<super::image_capture::CursorStamp>,
    pub epoch: u64,
    pub next_frame: Instant,
    pub callbacks_checked: Option<u64>,
    pub callbacks_pending: bool,
    pub target: Option<CaptureTarget>,
    pub resources: crate::render::RenderResources,
    pub scene: Option<crate::state::FrameScene>,
    pub scene_dirty: bool,
    pub last_used: Instant,
    trace: bool,
    preparations: u64,
    compositions: u64,
    last_revision: Option<u64>,
}

impl WorkspaceCapture {
    pub fn new(source: WorkspaceSource, epoch: u64) -> Self {
        Self {
            source,
            content: 0,
            prepared: None,
            cursor: None,
            epoch,
            next_frame: Instant::now(),
            callbacks_checked: None,
            callbacks_pending: false,
            target: None,
            resources: Default::default(),
            scene: None,
            scene_dirty: true,
            last_used: Instant::now(),
            trace: std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some(),
            preparations: 0,
            compositions: 0,
            last_revision: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.content_changed();
        self.scene_dirty = true;
    }

    pub fn release_idle_target(&mut self) {
        self.target = None;
        self.resources = Default::default();
        self.scene = None;
        self.prepared = None;
        if self.trace {
            tracing::info!(target: "ferese::render", workspace = self.source.id.0, "released idle workspace capture target");
        }
    }

    pub fn content_changed(&mut self) {
        self.content = self.content.wrapping_add(1);
    }

    fn callbacks(&self, state: &Ferese, send: bool) -> bool {
        let (Some(scene), Some(target)) = (&self.scene, &self.target) else {
            return false;
        };
        let mut pending = false;
        for id in scene.windows.keys() {
            let Some(window) = state
                .windows
                .window(*id)
                .filter(|window| !state.capture_protected(window))
            else {
                continue;
            };
            let mut visible = false;
            window.with_surfaces(|surface, _| visible |= target.surface_visible(surface));
            if !visible {
                continue;
            }
            // Normal display callbacks may already have drained these. Capture
            // never generates presentation feedback or changes scanout ownership.
            if send {
                window.send_frame(
                    &self.source.output,
                    state.start_time.elapsed(),
                    Some(interval()),
                    |_, _| None,
                );
            }
            window.with_surfaces(|_, states| {
                pending |= !states
                    .cached_state
                    .get::<smithay::wayland::compositor::SurfaceAttributes>()
                    .current()
                    .frame_callbacks
                    .is_empty();
            });
        }
        pending
    }

    pub fn render(
        &mut self,
        state: &mut Ferese,
        renderer: &mut GlesRenderer,
        cursor: bool,
    ) -> Result<(GlesTexture, u64), String> {
        if self.epoch != state.capture_epoch {
            self.resources = Default::default();
            self.target = None;
            self.epoch = state.capture_epoch;
            self.callbacks_checked = None;
        }
        if self.scene_dirty || self.scene.is_none() {
            let scene = state.sample_workspace_frame(self.source.id, &self.source.output);
            let members = state
                .workspaces
                .workspace(self.source.id)
                .into_iter()
                .flat_map(|workspace| {
                    workspace
                        .layout
                        .window_ids()
                        .chain(workspace.floating.iter().copied())
                })
                .collect();
            self.resources.retain_workspace_windows(&members);
            self.scene = Some(scene);
            self.scene_dirty = false;
        }
        let scene = self.scene.as_ref().expect("capture scene sampled");
        let elements = crate::render::capture_workspace_elements(
            state,
            renderer,
            &self.source,
            cursor,
            scene,
            &mut self.resources,
        );
        let result = crate::backends::direct::capture::render_capture_with_revision(
            renderer,
            &mut self.target,
            &self.source.render_output,
            &elements,
        )
        .map(|_| ())
        .map_err(|error| error.to_string());
        if let Err(error) = result {
            self.target = None;
            self.prepared = None;
            return Err(error);
        }
        self.preparations += 1;
        self.prepared = Some(self.content);
        let target = self
            .target
            .as_ref()
            .expect("successful capture has a target");
        let image = target.image();
        if self.last_revision != Some(image.1) {
            self.compositions += 1;
        }
        self.last_revision = Some(image.1);
        Ok(image)
    }
}

impl Drop for WorkspaceCapture {
    fn drop(&mut self) {
        if self.trace {
            tracing::info!(target: "ferese::render", workspace = self.source.id.0, preparations = self.preparations, compositions = self.compositions, "workspace capture outcomes");
        }
    }
}

/// The compositor enforces a ceiling even for a client that bypasses PipeWire's
/// pacing. A shared deadline is used for both hidden-client callbacks and frames.
pub(super) fn interval() -> Duration {
    static INTERVAL: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *INTERVAL.get_or_init(|| {
        let fps = std::env::var("FERESE_CAPTURE_MAX_FPS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(1, 240);
        Duration::from_nanos(1_000_000_000 / fps)
    })
}

/// Shader cache identity; this does not grant the descriptor a desktop output.
pub(crate) struct CaptureOutputId(pub ferese_core::OutputId);

fn capture_size(logical: Size<i32, Logical>, native_scale: f64) -> (Size<i32, Buffer>, f64) {
    let (width, height) = if logical.w >= logical.h {
        (1920.0, 1080.0)
    } else {
        (1080.0, 1920.0)
    };
    let scale = native_scale
        .min(width / f64::from(logical.w.max(1)))
        .min(height / f64::from(logical.h.max(1)));
    (
        (
            (f64::from(logical.w) * scale).round().max(1.0) as i32,
            (f64::from(logical.h) * scale).round().max(1.0) as i32,
        )
            .into(),
        scale,
    )
}

fn with_renderer<T>(
    state: &mut Ferese,
    output: &Output,
    render: impl FnOnce(&mut Ferese, &mut GlesRenderer) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(backend) = state.nested_backend.clone() {
        let mut backend = backend
            .try_borrow_mut()
            .map_err(|error| error.to_string())?;
        render(state, backend.renderer())
    } else if let Some(mut backend) = state.direct_backend.take() {
        let result = backend
            .capture_renderer(output)
            .ok_or_else(|| "Capture output is unavailable".to_string())
            .and_then(|renderer| render(state, renderer));
        state.direct_backend = Some(backend);
        result
    } else {
        Err("Capture renderer is unavailable".into())
    }
}

pub(super) fn process(
    view: &mut WorkspaceCapture,
    state: &mut Ferese,
    active: &mut super::image_capture::Active,
    frame: smithay::wayland::image_copy_capture::Frame,
    compositions_left: &mut usize,
) -> Option<Duration> {
    use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::FailureReason;
    let now = Instant::now();
    view.last_used = now;
    let cursor = active.session.draw_cursor() && view.source.cursor_visible(state);
    let cursor_stamp = cursor
        .then(|| state.capture_cursor_stamp(view.source.geometry))
        .flatten();
    let changed = view.prepared != Some(view.content)
        || view.epoch != state.capture_epoch
        || view.cursor != cursor_stamp;
    let interval = super::workspace_capture::interval();
    let mut retry = None;
    let image = if changed && (now < view.next_frame || *compositions_left == 0) {
        retry = Some(
            view.next_frame
                .saturating_duration_since(now)
                .max(Duration::from_millis(1)),
        );
        None
    } else if changed {
        *compositions_left -= 1;
        let output = view.source.output.clone();
        let result = with_renderer(state, &output, |state, renderer| {
            view.render(state, renderer, cursor)
        });
        match result {
            Ok(image) => {
                view.cursor = cursor_stamp;
                view.next_frame = now + interval;
                Some(image)
            }
            Err(error) => {
                tracing::warn!(%error, "workspace capture failed");
                frame.fail(FailureReason::Unknown);
                return None;
            }
        }
    } else {
        view.target.as_ref().map(|target| target.image())
    };
    if let Some((texture, revision)) =
        image.filter(|(_, revision)| active.revision != Some(*revision))
    {
        crate::backends::direct::capture::touch_cache(state);
        let result = with_renderer(state, &view.source.output, |_, renderer| {
            crate::render::capture_stream_texture(
                renderer,
                &texture,
                &frame.buffer(),
                Transform::Normal,
                false,
            )
        });
        active.revision = result.as_ref().ok().map(|_| revision);
        state.finish_stream_frame(frame, active.session.as_ref(), result);
    } else {
        active.pending = Some(frame);
    }

    let deadline = state
        .image_capture
        .workspace_callback_deadline(view.source.id, now);
    if view.prepared == Some(view.content)
        && (view.callbacks_checked != Some(view.content)
            || (view.callbacks_pending && now >= deadline))
    {
        view.callbacks_pending = view.callbacks(state, now >= deadline);
        view.callbacks_checked = Some(view.content);
        if now >= deadline {
            state
                .image_capture
                .set_workspace_callback_deadline(view.source.id, now + interval);
        }
    }
    if view.callbacks_pending {
        let delay = state
            .image_capture
            .workspace_callback_deadline(view.source.id, now)
            .saturating_duration_since(now);
        retry = Some(retry.map_or(delay, |existing| existing.min(delay)));
    }
    retry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_canvas_is_bounded_and_preserves_fractional_and_portrait_geometry() {
        for (logical, native, expected) in [
            ((1920, 1080), 2.0, (1920, 1080)),
            ((1080, 1920), 2.0, (1080, 1920)),
            ((800, 600), 1.25, (1000, 750)),
            ((3440, 1440), 1.0, (1920, 804)),
        ] {
            let (size, scale) = capture_size(logical.into(), native);
            assert_eq!(size, expected.into());
            assert!(scale <= native);
            assert!(size.w as u64 * size.h as u64 <= 1920 * 1080);
        }
    }
}
