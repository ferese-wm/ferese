use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::{Error as DamageError, OutputDamageTracker};
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::gles::{GlesError, GlesRenderer, GlesTarget, GlesTexture};
use smithay::backend::renderer::{Bind, Offscreen, Texture};
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::fmt::Display;
use std::time::{Duration, Instant};

use crate::Ferese;
use crate::render::{capture_output_elements, redraw_output};
use crate::state::FrameScene;

const CACHE_IDLE: Duration = Duration::from_secs(2);

pub(crate) mod changes;

pub(crate) struct CaptureTarget {
    texture: GlesTexture,
    damage: OutputDamageTracker,
    output: Output,
    revision: u64,
    id: smithay::backend::renderer::element::Id,
    commit: smithay::backend::renderer::utils::CommitCounter,
    stamp: Option<changes::Stamp>,
    preparations: u64,
    reuses: u64,
    compositions: u64,
    trace: bool,
    reported: Instant,
}

impl CaptureTarget {
    pub(crate) fn new(texture: GlesTexture, output: &Output) -> Self {
        Self {
            texture,
            damage: OutputDamageTracker::from_output(output),
            output: output.clone(),
            revision: 0,
            id: smithay::backend::renderer::element::Id::new(),
            commit: Default::default(),
            stamp: None,
            preparations: 0,
            reuses: 0,
            compositions: 0,
            trace: std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some(),
            reported: Instant::now(),
        }
    }
}

pub(crate) fn touch_cache(state: &mut Ferese) {
    if state.capture_last_used.replace(Instant::now()).is_some() {
        return;
    }
    if let Err(error) =
        state
            .loop_handle
            .insert_source(Timer::from_duration(CACHE_IDLE), |_, _, state| {
                let Some(last_used) = state.capture_last_used else {
                    return TimeoutAction::Drop;
                };
                let idle = last_used.elapsed();
                if idle < CACHE_IDLE {
                    return TimeoutAction::ToDuration(CACHE_IDLE - idle);
                }
                if let Some(backend) = state.nested_backend.clone()
                    && let Ok(mut backend) = backend.try_borrow_mut()
                {
                    crate::render::clear_stream_capture_targets(backend.renderer());
                }
                if let Some(backend) = state.direct_backend.as_mut() {
                    backend.clear_stream_capture_targets();
                }
                if !state.pending_screencopies.is_empty() || state.image_capture.has_sessions() {
                    // A damage-waiting stream must retain the comparison image.
                    return TimeoutAction::ToDuration(CACHE_IDLE);
                }
                // A stream can pause or fail without another repaint. Release its
                // offscreen targets and filtered-scene caches from the event loop.
                state.capture_last_used = None;
                state.capture_render = Default::default();
                state.nested_capture_texture = Default::default();
                if let Some(backend) = state.direct_backend.as_mut() {
                    for device in backend.devices.values_mut() {
                        for output in device.outputs.values_mut() {
                            output.capture_texture = Default::default();
                            output.mirror_capture_texture = Default::default();
                        }
                    }
                }
                TimeoutAction::Drop
            })
    {
        state.capture_last_used = None;
        tracing::warn!(%error, "could not schedule capture cache cleanup");
    }
}

pub(crate) fn capture_output(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    textures: &mut [Option<CaptureTarget>; 2],
    output: &Output,
    scene: &FrameScene,
) {
    for include_cursor in [true, false] {
        if !state.has_pending_screencopy(output, include_cursor) {
            continue;
        }

        let texture = &mut textures[usize::from(include_cursor)];
        if !prepare_capture(
            state,
            renderer,
            texture,
            output,
            output,
            include_cursor,
            scene,
        ) {
            continue;
        }
        let texture = texture.as_mut().expect("capture target allocated");
        let Some((target, revision)) = capture_request(state, output, include_cursor, || {
            renderer
                .bind(&mut texture.texture)
                .map(|target| (target, texture.revision))
        }) else {
            continue;
        };
        state.process_screencopies_with_revision(
            renderer,
            &target,
            output,
            include_cursor,
            Some(revision),
        );
        drop(target);
        state.process_output_streams(renderer, &texture.texture, output, include_cursor, revision);
    }
}

pub(super) fn capture_mirror(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    source: &Output,
    scene: &FrameScene,
    output: &mut super::DirectOutput,
) {
    for include_cursor in [true, false] {
        if !state.has_pending_screencopy(&output.output, include_cursor) {
            continue;
        }
        // Never sample mirror_texture: it contains the unfiltered display.
        let canvas = output
            .mirror_canvas
            .as_ref()
            .expect("mirror display composed");
        super::mirror::configure_canvas(canvas, source);
        let source_target = &mut output.mirror_capture_texture[usize::from(include_cursor)];
        if !prepare_capture(
            state,
            renderer,
            source_target,
            source,
            canvas,
            include_cursor,
            scene,
        ) {
            continue;
        }
        let source_target = source_target
            .as_ref()
            .expect("mirror capture target allocated");
        let element = super::mirror::fitted_texture(
            source_target.texture.clone(),
            &output.output,
            source_target.id.clone(),
            source_target.commit,
        );
        let Some((target, revision)) =
            capture_request(state, &output.output, include_cursor, || {
                render_capture_with_revision(
                    renderer,
                    &mut output.capture_texture[usize::from(include_cursor)],
                    &output.output,
                    &[element],
                )
            })
        else {
            continue;
        };
        state.process_screencopies_with_revision(
            renderer,
            &target,
            &output.output,
            include_cursor,
            Some(revision),
        );
        drop(target);
        state.process_output_streams(
            renderer,
            &output.capture_texture[usize::from(include_cursor)]
                .as_ref()
                .expect("capture target allocated")
                .texture
                .clone(),
            &output.output,
            include_cursor,
            revision,
        );
    }
}

/// Display damage cannot prove a filtered scene is unchanged: an opaque private
/// window may hide updates that must remain visible to capture underneath it.
/// Retain a separate source revision and only prepare the filtered scene on a
/// real mutation. Delivery buffers still receive a complete image.
fn prepare_capture(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    target: &mut Option<CaptureTarget>,
    source: &Output,
    destination: &Output,
    include_cursor: bool,
    scene: &FrameScene,
) -> bool {
    touch_cache(state);
    let stamp = changes::Stamp::new(state, source, include_cursor);
    let reusable = target.as_ref().is_some_and(|target| {
        target.stamp.as_ref() == Some(&stamp)
            && target.output == *destination
            && destination
                .current_mode()
                .is_some_and(|mode| target.texture.size() == (mode.size.w, mode.size.h).into())
    });
    if reusable {
        target.as_mut().expect("cached capture target").reuses += 1;
    } else {
        // Keep the capture image separate from the display swapchain and planes.
        let elements = capture_output_elements(state, renderer, source, include_cursor, scene);
        let Some(framebuffer) = capture_request(state, destination, include_cursor, || {
            render_capture_with_revision(renderer, target, destination, &elements)
        }) else {
            // A failed render can have changed the retained pixels already.
            if let Some(target) = target.as_mut() {
                target.stamp = None;
            }
            return false;
        };
        drop(framebuffer);
        let target = target.as_mut().expect("capture target allocated");
        target.stamp = Some(stamp);
        target.preparations += 1;
    }
    let target = target.as_mut().expect("capture target allocated");
    if target.trace && target.reported.elapsed() >= Duration::from_secs(5) {
        tracing::info!(target: "ferese::render", output = %source.name(), include_cursor,
            preparations = target.preparations, scene_reuses = target.reuses,
            compositions = target.compositions, "capture scene outcomes");
        target.reported = Instant::now();
        target.preparations = 0;
        target.reuses = 0;
        target.compositions = 0;
    }
    true
}

pub(crate) fn capture_request<T, E: Display>(
    state: &mut Ferese,
    output: &Output,
    include_cursor: bool,
    render: impl FnOnce() -> Result<T, E>,
) -> Option<T> {
    match render() {
        Ok(target) => Some(target),
        Err(error) => {
            tracing::warn!(?output, include_cursor, %error, "screencopy render failed");
            state.fail_screencopies(output, include_cursor);
            None
        }
    }
}

pub(super) fn after_queue<T, E>(queued: Result<T, E>, capture: impl FnOnce()) -> Result<T, E> {
    let queued = queued?;
    capture();
    Ok(queued)
}

#[cfg(test)]
pub(super) fn render_capture<'a, E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    target: &'a mut Option<CaptureTarget>,
    output: &Output,
    elements: &[E],
) -> Result<GlesTarget<'a>, DamageError<GlesError>> {
    render_capture_with_revision(renderer, target, output, elements).map(|(target, _)| target)
}

pub(super) fn render_capture_with_revision<'a, E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    target: &'a mut Option<CaptureTarget>,
    output: &Output,
    elements: &[E],
) -> Result<(GlesTarget<'a>, u64), DamageError<GlesError>> {
    let size = output.current_mode().expect("output has a mode").size;
    if target
        .as_ref()
        .is_none_or(|target| target.texture.size() != (size.w, size.h).into())
    {
        *target = Some(CaptureTarget::new(
            Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                (size.w, size.h).into(),
            )
            .map_err(DamageError::Rendering)?,
            output,
        ));
    }
    let target = target.as_mut().expect("capture target allocated");
    if target.output != *output {
        target.damage = OutputDamageTracker::from_output(output);
        target.output = output.clone();
    }
    // This texture retains the previous capture, independently of display
    // scanout and damage. Age one lets unchanged pixels survive; a new target
    // has no history and is repainted in full. Rebind after every readback.
    let mut framebuffer = renderer
        .bind(&mut target.texture)
        .map_err(DamageError::Rendering)?;
    let result = target.damage.render_output(
        renderer,
        &mut framebuffer,
        1,
        elements,
        [0.035, 0.04, 0.055, 1.0],
    )?;
    if result
        .damage
        .as_ref()
        .is_some_and(|damage| !damage.is_empty())
    {
        static NEXT_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        target.commit.increment();
        target.compositions += 1;
        target.revision = NEXT_REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok((framebuffer, target.revision))
}

pub(super) fn render_texture<'a, E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    texture: &'a mut Option<GlesTexture>,
    output: &Output,
    elements: &[E],
) -> Result<GlesTarget<'a>, GlesError> {
    let size = output.current_mode().expect("output has a mode").size;

    if texture
        .as_ref()
        .is_none_or(|texture| texture.size() != (size.w, size.h).into())
    {
        *texture = Some(Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            (size.w, size.h).into(),
        )?);
    }

    // Rebinding for each capture restores GL state after readback.
    let mut target = renderer.bind(texture.as_mut().expect("capture target allocated"))?;
    redraw_output(renderer, &mut target, output, elements)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::{Id, Kind};
    use smithay::backend::renderer::utils::CommitCounter;
    use smithay::backend::renderer::{Color32F, ExportMem};
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    use smithay::utils::{Physical, Rectangle, Transform};

    use super::*;

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn unchanged_capture_skips_preparation_without_losing_source_updates() {
        if !crate::startup_tests::private_runtime(
            "backends::direct::capture::tests::unchanged_capture_skips_preparation_without_losing_source_updates",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut config = crate::config::Config::default().runtime_config().unwrap();
        config.wallpaper.path = None;
        config.animations_enabled = false;
        let mut state = Ferese::new(
            &mut events,
            smithay::reexports::wayland_server::Display::new().unwrap(),
            config,
        )
        .unwrap();
        let output = Output::new(
            "capture-cache".into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (320, 240).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "capture-cache".into());
        let (_window, _client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
        state.advance_animations(Instant::now());
        let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
        // SAFETY: the device and display outlive this exclusively owned context.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this test thread exclusively owns the fresh context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let mut target = None;
        let prepare = |state: &mut Ferese,
                       renderer: &mut GlesRenderer,
                       target: &mut Option<CaptureTarget>| {
            let scene = state.sample_frame(&output, Duration::ZERO);
            assert!(prepare_capture(
                state, renderer, target, &output, &output, false, &scene
            ));
        };
        let pixels = |renderer: &mut GlesRenderer, target: &mut Option<CaptureTarget>| {
            let target = target.as_mut().unwrap();
            let framebuffer = renderer.bind(&mut target.texture).unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &framebuffer,
                    Rectangle::from_size((320, 240).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            drop(framebuffer);
            renderer.map_texture(&mapping).unwrap().to_vec()
        };
        prepare(&mut state, &mut renderer, &mut target);
        // Keep assertions independent of the opt-in periodic reporting clock.
        target.as_mut().unwrap().trace = false;
        let original = pixels(&mut renderer, &mut target);
        for _ in 0..100 {
            super::super::schedule_capture(&mut state, std::slice::from_ref(&output));
            prepare(&mut state, &mut renderer, &mut target);
        }
        assert_eq!(target.as_ref().unwrap().preparations, 1);
        assert_eq!(target.as_ref().unwrap().reuses, 100);
        assert_eq!(pixels(&mut renderer, &mut target), original);
        super::super::render_cursor(&mut state);
        prepare(&mut state, &mut renderer, &mut target);
        assert_eq!(target.as_ref().unwrap().preparations, 1);
        state.capture_epoch += 1;
        prepare(&mut state, &mut renderer, &mut target);
        assert_eq!(target.as_ref().unwrap().preparations, 2);
        let (_cover, _cover_client) =
            crate::state::test_window(&mut state, &mut events, 0xff00ff00);
        state.advance_animations(Instant::now());
        prepare(&mut state, &mut renderer, &mut target);
        assert_eq!(target.as_ref().unwrap().preparations, 3);
        assert_ne!(pixels(&mut renderer, &mut target), original);
    }

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn incremental_capture_matches_full_redraw_after_scene_and_output_changes() {
        let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
        // SAFETY: the enumerated device remains alive through its EGL display.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this thread exclusively owns the renderer and its fresh context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let output = |name: &str| {
            Output::new(
                name.into(),
                PhysicalProperties {
                    serial_number: String::new(),
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            )
        };
        let outputs = [output("first"), output("second")];
        let mut capture = None;
        let mut reference = None;
        let background = SolidColorRenderElement::new(
            Id::new(),
            Rectangle::from_size((96, 96).into()),
            CommitCounter::default(),
            Color32F::new(0., 0., 1., 1.),
            Kind::Unspecified,
        );
        let item_id = Id::new();
        for output in outputs.iter().chain(std::iter::once(&outputs[0])) {
            for (width, height) in [(64, 48), (48, 64)] {
                for scale in [1., 1.25, 1.5] {
                    for transform in [
                        Transform::Normal,
                        Transform::_90,
                        Transform::_180,
                        Transform::Flipped,
                    ] {
                        output.change_current_state(
                            Some(Mode {
                                size: (width, height).into(),
                                refresh: 60_000,
                            }),
                            Some(transform),
                            Some(smithay::output::Scale::Fractional(scale)),
                            None,
                        );
                        let mut commit = CommitCounter::default();
                        // Include moves, color/alpha changes, removal, cursor
                        // inclusion/exclusion and an entirely empty scene.
                        for step in [6, 0, 1, 2, 3, 4, 5, 6] {
                            if step == 2 {
                                commit.increment();
                            }
                            let mut elements = Vec::new();
                            if matches!(step, 0..=2 | 4) {
                                elements.push(SolidColorRenderElement::new(
                                    item_id.clone(),
                                    Rectangle::new(
                                        (if step == 0 { 4 } else { 20 }, 4).into(),
                                        (12, 12).into(),
                                    ),
                                    commit,
                                    if step < 2 {
                                        Color32F::new(0., 1., 0., 1.)
                                    } else {
                                        Color32F::new(0.5, 0., 0., 0.5)
                                    },
                                    Kind::Cursor,
                                ));
                            }
                            if step != 6 {
                                elements.push(background.clone());
                            }
                            drop(
                                render_capture(&mut renderer, &mut capture, output, &elements)
                                    .unwrap(),
                            );
                            // The repeated scene must preserve the same pixels when
                            // damage tracking skips all drawing on the next capture.
                            let actual =
                                render_capture(&mut renderer, &mut capture, output, &elements)
                                    .unwrap();
                            let mapping = renderer
                                .copy_framebuffer(
                                    &actual,
                                    Rectangle::from_size((width, height).into()),
                                    Fourcc::Abgr8888,
                                )
                                .unwrap();
                            let actual_pixels = renderer.map_texture(&mapping).unwrap().to_vec();
                            drop(actual);
                            let expected =
                                render_texture(&mut renderer, &mut reference, output, &elements)
                                    .unwrap();
                            let mapping = renderer
                                .copy_framebuffer(
                                    &expected,
                                    Rectangle::from_size((width, height).into()),
                                    Fourcc::Abgr8888,
                                )
                                .unwrap();
                            let expected_pixels = renderer.map_texture(&mapping).unwrap();
                            assert_eq!(
                                actual_pixels,
                                expected_pixels,
                                "{} {width}x{height} scale={scale} {transform:?} step={step}",
                                output.name()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "manual 4K capture benchmark; requires a hardware EGL device"]
    fn profile_output_capture() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .find(|device| device.render_device_path().is_ok())
            .expect("a hardware EGL rendering device");
        eprintln!("capture_benchmark device={:?}", device.render_device_path());
        // SAFETY: the enumerated device remains alive through its EGL display.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this thread exclusively owns the renderer and its fresh context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let output = Output::new(
            "capture-benchmark".into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (3840, 2160).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            None,
            None,
        );
        let mut texture = None;
        let background_id = Id::new();
        let cursor_id = Id::new();
        let mut commit = CommitCounter::default();
        let panes: Vec<_> = (0..20)
            .map(|i| {
                SolidColorRenderElement::new(
                    Id::new(),
                    Rectangle::new((i * 150, i * 75).into(), (800, 600).into()),
                    CommitCounter::default(),
                    Color32F::new(0.1, 0.2, 0.3, 0.5),
                    Kind::Unspecified,
                )
            })
            .collect();
        for readback in [false, true] {
            let size = if readback { (3840, 2160) } else { (1, 1) };
            let mut pixels = vec![0u8; size.0 as usize * size.1 as usize * 4];
            for workload in ["static", "cursor", "full"] {
                for run in 0..=3 {
                    let started = Instant::now();
                    commit.increment();
                    for tick in 0..60 {
                        let color = if workload == "full" {
                            commit.increment();
                            tick as f32 / 60.
                        } else {
                            0.1
                        };
                        let mut elements = vec![SolidColorRenderElement::new(
                            cursor_id.clone(),
                            Rectangle::new(
                                (if workload == "cursor" { tick * 30 } else { 0 }, 0).into(),
                                (24, 24).into(),
                            ),
                            CommitCounter::default(),
                            Color32F::new(1., 1., 1., 1.),
                            Kind::Cursor,
                        )];
                        elements.extend_from_slice(&panes);
                        elements.push(SolidColorRenderElement::new(
                            background_id.clone(),
                            Rectangle::from_size((3840, 2160).into()),
                            commit,
                            Color32F::new(color, 0.1, 0.2, 1.),
                            Kind::Unspecified,
                        ));
                        let target =
                            render_capture(&mut renderer, &mut texture, &output, &elements)
                                .unwrap();
                        // Mapping waits for GPU completion in both workloads. The full
                        // readback also includes the copy into the client's SHM buffer.
                        let mapping = renderer
                            .copy_framebuffer(
                                &target,
                                Rectangle::from_size(size.into()),
                                Fourcc::Argb8888,
                            )
                            .unwrap();
                        let source = renderer.map_texture(&mapping).unwrap();
                        let bytes = pixels.len();
                        pixels.copy_from_slice(&source[..bytes]);
                        std::hint::black_box(&pixels);
                    }
                    if run != 0 {
                        eprintln!(
                            "output_capture_benchmark readback={readback} workload={workload} run={run} frames=60 elapsed_us={}",
                            started.elapsed().as_micros()
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn capture_recomposes_cursor_without_touching_the_display_target() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL device");
        // SAFETY: The native display comes from a live enumerated EGLDevice and its lifetime is
        // retained by the EGL objects created here.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues
        // GL work only through its renderer methods.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let output = Output::new(
            "capture".into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (32, 24).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            None,
            None,
        );
        let background = SolidColorRenderElement::new(
            Id::new(),
            Rectangle::<i32, Physical>::from_size((32, 24).into()),
            CommitCounter::default(),
            Color32F::new(1.0, 0.0, 0.0, 1.0),
            Kind::Unspecified,
        );
        let cursor = SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new((0, 0).into(), (4, 4).into()),
            CommitCounter::default(),
            Color32F::new(0.0, 1.0, 0.0, 1.0),
            Kind::Cursor,
        );
        let mut displayed = None;
        let mut capture = None;
        {
            let _ = render_texture(
                &mut renderer,
                &mut displayed,
                &output,
                std::slice::from_ref(&background),
            )
            .unwrap();
        }

        for (elements, expected) in [
            (vec![cursor, background.clone()], [0, 255, 0, 255]),
            (vec![background], [255, 0, 0, 255]),
        ] {
            let target = render_capture(&mut renderer, &mut capture, &output, &elements).unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((32, 24).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            let pixels = renderer.map_texture(&mapping).unwrap();
            assert_eq!(&pixels[..4], &expected);
            assert_eq!(
                &pixels[(12 * 32 + 16) * 4..(12 * 32 + 16) * 4 + 4],
                &[255, 0, 0, 255]
            );
            drop(target);

            let target = renderer.bind(displayed.as_mut().unwrap()).unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((32, 24).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            assert_eq!(
                &renderer.map_texture(&mapping).unwrap()[..4],
                &[255, 0, 0, 255]
            );
        }

        output.change_current_state(
            Some(Mode {
                size: (48, 16).into(),
                refresh: 60_000,
            }),
            Some(Transform::_90),
            None,
            None,
        );
        let _ =
            render_capture::<SolidColorRenderElement>(&mut renderer, &mut capture, &output, &[])
                .unwrap();
        assert_eq!(capture.as_ref().unwrap().texture.size(), (48, 16).into());
    }
}

#[cfg(test)]
mod failure_tests {
    use std::cell::Cell;

    use calloop::channel::channel;
    use smithay::output::Output;
    use smithay::utils::{Buffer, Rectangle};

    use super::*;

    #[test]
    fn capture_allocation_failure_fails_one_request_after_display_submission() {
        if !crate::startup_tests::private_runtime(
            "backends::direct::capture::failure_tests::capture_allocation_failure_fails_one_request_after_display_submission",
        ) {
            return;
        }

        let mut events = calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = Output::new(
            "capture-failure".into(),
            smithay::output::PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        let (sender, receiver) = channel();
        state
            .pending_screencopies
            .push(crate::handlers::screencopy::PendingScreencopy::owned(
                41,
                0,
                sender,
                output.clone(),
                Rectangle::<i32, Buffer>::from_size((16, 16).into()),
            ));
        let other_output = Output::new(
            "other-capture".into(),
            smithay::output::PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        let (other_sender, other_receiver) = channel();
        state
            .pending_screencopies
            .push(crate::handlers::screencopy::PendingScreencopy::owned(
                42,
                0,
                other_sender,
                other_output.clone(),
                Rectangle::<i32, Buffer>::from_size((16, 16).into()),
            ));

        let submitted = Cell::new(false);
        let queued = {
            submitted.set(true);
            Ok::<(), ()>(())
        };
        after_queue(queued, || {
            assert!(
                submitted.get(),
                "capture must run after the display queue succeeds"
            );
            assert!(
                capture_request(&mut state, &output, false, || Err::<(), _>(
                    "injected allocation failure"
                ))
                .is_none()
            );
        })
        .unwrap();

        assert!(
            submitted.get(),
            "capture failure must not undo queued display work"
        );
        assert_eq!(state.pending_screencopies.len(), 1);
        assert_eq!(state.pending_screencopies[0].output, other_output);
        let outcome = receiver.try_recv().unwrap();
        assert_eq!(outcome.request, 41);
        assert_eq!(outcome.part, 0);
        assert!(outcome.result.is_err());
        assert!(
            receiver.try_recv().is_err(),
            "request should fail exactly once"
        );
        assert!(
            other_receiver.try_recv().is_err(),
            "other output's request must stay pending"
        );
    }
}
