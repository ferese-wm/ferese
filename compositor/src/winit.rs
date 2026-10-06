use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Frame, ImportDma, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::desktop::layer_map_for_output;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Physical, Rectangle, Size, Transform};

use crate::Ferese;
use crate::metrics::{FrameEffectMetrics, RenderMetrics};
use crate::render::{animated_window_elements, frame_effect_metrics, layer_surfaces};

pub(crate) type NestedBackend = Rc<RefCell<winit::WinitGraphicsBackend<GlesRenderer>>>;
type DamageRenderResult = Result<
    (
        Option<Vec<Rectangle<i32, Physical>>>,
        FrameEffectMetrics,
        RenderElementStates,
    ),
    Box<dyn Error>,
>;

pub fn init(event_loop: &mut EventLoop<Ferese>, state: &mut Ferese) -> Result<(), Box<dyn Error>> {
    let (mut backend, event_source) = winit::init::<GlesRenderer>()?;
    configure_nested_protocols(state);
    let initial_size = backend.window_size();
    let drawable = Rc::new(Cell::new(usable_size(initial_size)));
    let dmabuf_formats = backend.renderer().dmabuf_formats();
    let display_handle = state.display_handle.clone();
    let dmabuf_global = state
        .dmabuf_state
        .create_global::<Ferese>(&display_handle, dmabuf_formats);
    state.dmabuf_imports.register(dmabuf_global);
    let initial_scale = normalized_scale(backend.scale_factor());
    let mode = Mode {
        size: if drawable.get() { initial_size } else { (1, 1).into() },
        refresh: backend
            .window()
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .unwrap_or(60_000) as i32,
    };
    let output = Output::new(
        "ferese-winit".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Ferese".into(),
            model: "Nested".into(),
        },
    );
    output.create_global::<Ferese>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        Some(Scale::Fractional(initial_scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "nested-primary".to_owned());

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let mut missed_deadlines = 0_u64;
    let mut render_metrics = RenderMetrics::from_environment(output.name());

    // Do not request a redraw recursively: a no-damage redraw has no EGL
    // submission to pace it and otherwise spins at full CPU. Independently
    // scheduled frames also let clients receive callbacks without damage.
    let refresh = Rc::new(Cell::new(Duration::from_nanos(
        1_000_000_000_000 / mode.refresh.max(1) as u64,
    )));
    let backend = Rc::new(RefCell::new(backend));
    state.nested_backend = Some(backend.clone());
    let redraw_backend = backend.clone();
    let redraw_refresh = refresh.clone();
    let redraw_drawable = drawable.clone();
    event_loop
        .handle()
        .insert_source(Timer::from_duration(refresh.get()), move |_, _, _| {
            if redraw_drawable.get() {
                redraw_backend.borrow().window().request_redraw();
                TimeoutAction::ToDuration(redraw_refresh.get())
            } else {
                TimeoutAction::ToDuration(Duration::from_millis(250))
            }
        })?;

    let mut capture_texture = None;
    event_loop
        .handle()
        .insert_source(event_source, move |event, _, state| {
            let mut backend = backend.borrow_mut();
            match event {
                WinitEvent::Resized { size, scale_factor } => {
                    tracing::debug!(target: "ferese::nested_input", ?size, scale_factor, "host resized nested output");
                    let rate = backend
                        .window()
                        .current_monitor()
                        .and_then(|monitor| monitor.refresh_rate_millihertz())
                        .unwrap_or(60_000) as i32;
                    if !resize_nested_output(state, &output, &drawable, (size.w, size.h), scale_factor, rate) {
                        return;
                    }

                    refresh.set(Duration::from_nanos(1_000_000_000_000 / rate.max(1) as u64));
                    if let Err(error) = state.display_handle.flush_clients() {
                        tracing::debug!(%error, "failed to flush output-resize configure");
                    }
                    backend.window().request_redraw();
                }
                WinitEvent::Input(event) => {
                    let kind = std::mem::discriminant(&event);
                    state.process_input_event(event);
                    tracing::debug!(target: "ferese::nested_input", ?kind,
                        pointer = ?state.seat.get_pointer().map(|pointer| pointer.current_location()),
                        "host input in nested output");
                }
                WinitEvent::Redraw => {
                    if !drawable.get() || !usable_size(backend.window_size()) {
                        drawable.set(false);
                        return;
                    }

                    state.advance_animations(Instant::now());
                    let age = backend.buffer_age().unwrap_or(0);
                    let render_started = Instant::now();
                    let rendered = (|| -> DamageRenderResult {
                        {
                            let (renderer, mut framebuffer) = backend.bind()?;
                            let elements = animated_window_elements(state, renderer, &output);
                            let effects = frame_effect_metrics(&elements, output.current_scale().fractional_scale());
                            let result = damage_tracker.render_output(
                                renderer,
                                &mut framebuffer,
                                age,
                                &elements,
                                [0.035, 0.04, 0.055, 1.0],
                            )?;
                            // Keep the existing readback-only path when the
                            // displayed scene has no privacy exclusions.
                            capture_with_display_restore(
                                renderer,
                                &mut framebuffer,
                                |renderer, framebuffer| {
                                    let mut changed = !state.has_capture_exclusions()
                                        && state.process_screencopies(renderer, framebuffer, &output, true);
                                    if state.has_pending_screencopy(&output, false)
                                        || state.has_pending_screencopy(&output, true)
                                    {
                                        let scene = state.sample_frame(&output, Duration::ZERO);
                                        crate::backends::direct::capture::capture_output(
                                            state,
                                            renderer,
                                            &mut capture_texture,
                                            &output,
                                            &scene,
                                        );
                                        changed = true;
                                    }

                                    changed
                                },
                                |renderer, framebuffer| {
                                    // Capture failures are handled locally. Failure to restore
                                    // the display target means the renderer itself is unusable.
                                    let _ = renderer
                                        .render(
                                            framebuffer,
                                            output.current_mode().unwrap().size,
                                            output.current_transform(),
                                        )?
                                        .finish()?;
                                    Ok::<_, smithay::backend::renderer::gles::GlesError>(())
                                },
                            )?;

                            Ok((result.damage.cloned(), effects, result.states))
                        }
                    })();
                    let (damage, effects, rendered_states) = match rendered {
                        Ok((Some(damage), effects, states)) => (damage, effects, states),
                        Ok((None, _, _)) => {
                            render_metrics.record_no_damage(render_started.elapsed(), missed_deadlines);
                            // A callback means permission to draw the next client
                            // frame, not proof of a new compositor presentation.
                            // No-damage frames must still unblock layer clients.
                            send_nested_frame_callbacks(state, &output);
                            state.space.refresh();
                            state.popups.cleanup();
                            layer_map_for_output(&output).cleanup();
                            if let Err(error) = state.display_handle.flush_clients() {
                                tracing::debug!(%error, "failed to flush clients");
                            }
                            return;
                        }
                        Err(error) => {
                            tracing::error!(%error, "nested renderer failed");
                            state.loop_signal.stop();
                            return;
                        }
                    };
                    if let Err(error) = backend.submit(Some(&damage)) {
                        tracing::error!(%error, "nested buffer submission failed");
                        state.loop_signal.stop();
                        return;
                    }
                    state.display_presentation.queued(&output, &rendered_states);
                    state.display_presentation.presented(&output);
                    state.refresh_idle_inhibition();

                    if state.session_lock.active() {
                        state.lock_frame_presented(&output);
                    }
                    let elapsed = render_started.elapsed();
                    if elapsed > refresh.get() {
                        missed_deadlines += (elapsed.as_nanos() / refresh.get().as_nanos()) as u64;
                    }
                    render_metrics.record_frame(elapsed, &damage, missed_deadlines, effects);

                    send_nested_frame_callbacks(state, &output);
                    state.space.refresh();
                    state.popups.cleanup();
                    layer_map_for_output(&output).cleanup();
                    if let Err(error) = state.display_handle.flush_clients() {
                        tracing::debug!(%error, "failed to flush clients");
                    }
                }
                WinitEvent::CloseRequested => state.loop_signal.stop(),
                _ => {}
            }
        })?;
    Ok(())
}

fn configure_nested_protocols(state: &mut Ferese) {
    // Winit's swap has no host presentation timestamp or retrace counter.
    // Do not advertise timing we cannot report accurately.
    state
        .display_handle
        .disable_global::<Ferese>(state.presentation_state.global());
}

fn usable_dimensions(width: i32, height: i32) -> bool {
    width > 0 && height > 0
}

fn usable_size(size: Size<i32, Physical>) -> bool {
    usable_dimensions(size.w, size.h)
}

fn resize_nested_output(
    state: &mut Ferese,
    output: &Output,
    drawable: &Cell<bool>,
    size: (i32, i32),
    scale_factor: f64,
    rate: i32,
) -> bool {
    if !usable_dimensions(size.0, size.1) {
        drawable.set(false);
        return false;
    }

    let mut runtime = state.current_desktop_outputs();
    let Some(resized) = runtime.iter_mut().find(|entry| entry.output == *output) else {
        drawable.set(false);
        return false;
    };
    resized.mode = Mode {
        size: (size.0, size.1).into(),
        refresh: rate.max(1),
    };
    resized.scale = Scale::Fractional(normalized_scale(scale_factor));
    // Paused host time must not advance springs when publication relayouts.
    if !drawable.get() {
        state.reset_animation_clock();
    }

    state.begin_desktop_transition();
    let changes = match state.publish_desktop(runtime) {
        Ok(changes) => changes,
        Err(error) => {
            state.desktop_transition = None;
            drawable.set(false);
            tracing::warn!(%error, "ignoring invalid nested output geometry");
            return false;
        }
    };
    state.finish_desktop_transition(changes);
    drawable.set(true);
    true
}

fn capture_with_display_restore<R, F, E>(
    renderer: &mut R,
    framebuffer: &mut F,
    capture: impl FnOnce(&mut R, &mut F) -> bool,
    restore: impl FnOnce(&mut R, &mut F) -> Result<(), E>,
) -> Result<(), E> {
    if capture(renderer, framebuffer) {
        restore(renderer, framebuffer)?;
    }

    Ok(())
}

fn send_nested_frame_callbacks(state: &mut Ferese, output: &Output) {
    let eligible = state.callback_outputs();
    if state.session_lock.active() {
        state.lock_frame_callbacks(output);
        return;
    }
    let time = state.start_time.elapsed();
    for window in state.space.elements().filter(|window| {
        state
            .windows
            .ids()
            .get(*window)
            .is_some_and(|id| state.window_belongs_to_output(*id, output))
    }) {
        window.send_frame(output, time, None, |surface, _| eligible.get(&surface.into()).cloned());
    }
    for layer in layer_surfaces(output) {
        layer.send_frame(output, time, None, |surface, _| eligible.get(&surface.into()).cloned());
    }
    state.send_cursor_frame(output);
}

fn normalized_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_host_resize_preserves_geometry_and_recovers() {
        if !crate::startup_tests::private_runtime("winit::tests::zero_host_resize_preserves_geometry_and_recovers") {
            return;
        }

        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = Output::new(
            "nested".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "nested-primary".into());
        let drawable = Cell::new(true);
        assert!(resize_nested_output(
            &mut state,
            &output,
            &drawable,
            (900, 600),
            1.25,
            60_000
        ));
        let before = output.current_mode();
        for size in [(0, 600), (900, 0), (0, 0), (-1, 600)] {
            assert!(!resize_nested_output(
                &mut state, &output, &drawable, size, 2.0, 144_000
            ));
            assert_eq!(output.current_mode(), before);
            assert_eq!(output.current_scale().fractional_scale(), 1.25);
            assert!(!drawable.get());
            assert!(state.desktop_transition.is_none());
        }

        assert!(resize_nested_output(
            &mut state,
            &output,
            &drawable,
            (1200, 800),
            1.5,
            120_000
        ));
        assert_eq!(output.current_mode().unwrap().size, (1200, 800).into());
        assert!(drawable.get());
        assert!(state.desktop_transition.is_none());
    }

    #[test]
    fn nested_registry_does_not_advertise_unobserved_presentation_timing() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::sync::Arc;
        if !crate::startup_tests::private_runtime(
            "winit::tests::nested_registry_does_not_advertise_unobserved_presentation_timing",
        ) {
            return;
        }

        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        configure_nested_protocols(&mut state);
        let (server, mut wire) = UnixStream::pair().unwrap();
        let _client = state
            .display_handle
            .insert_client(server, Arc::new(crate::state::ClientState::default()))
            .unwrap();
        wire.write_all(
            &[1u32, 12 << 16 | 1, 2]
                .into_iter()
                .flat_map(u32::to_ne_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        events.dispatch(Duration::from_millis(1), &mut state).unwrap();
        state.display_handle.flush_clients().unwrap();
        wire.set_nonblocking(true).unwrap();
        let mut globals = Vec::new();
        let _ = wire.read_to_end(&mut globals);
        assert!(
            globals
                .windows(b"wl_compositor\0".len())
                .any(|bytes| bytes == b"wl_compositor\0")
        );
        assert!(
            !globals
                .windows(b"wp_presentation\0".len())
                .any(|bytes| bytes == b"wp_presentation\0")
        );
    }

    #[test]
    #[ignore = "requires an EGL device and private Wayland sockets"]
    fn capture_failure_restores_display_target_and_subsequent_frames_render() {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
        use smithay::backend::renderer::gles::{GlesError, GlesTexture};
        use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};
        use smithay::reexports::calloop::channel;
        use smithay::utils::Buffer;
        if !crate::startup_tests::private_runtime(
            "winit::tests::capture_failure_restores_display_target_and_subsequent_frames_render",
        ) {
            return;
        }

        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let display = unsafe { EGLDisplay::new(EGLDevice::enumerate().unwrap().last().unwrap()).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let output = Output::new(
            "nested".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        let size: Size<i32, Physical> = (32, 24).into();
        let mut displayed: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (32, 24).into()).unwrap();
        let mut offscreen: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (32, 24).into()).unwrap();
        let damage = [Rectangle::from_size(size)];
        let mut framebuffer = renderer.bind(&mut displayed).unwrap();
        for changed_binding in [false, true] {
            let (sender, receiver) = channel::channel();
            state
                .pending_screencopies
                .push(crate::handlers::screencopy::PendingScreencopy::owned(
                    1,
                    0,
                    sender,
                    output.clone(),
                    Rectangle::<i32, Buffer>::from_size((32, 24).into()),
                ));
            capture_with_display_restore(
                &mut renderer,
                &mut framebuffer,
                |renderer, _| {
                    if changed_binding {
                        let _target = renderer.bind(&mut offscreen).unwrap();
                    }
                    assert!(
                        crate::backends::direct::capture::capture_request(&mut state, &output, false, || {
                            Err::<(), _>("injected offscreen allocation/render failure")
                        })
                        .is_none()
                    );
                    true
                },
                |renderer, framebuffer| {
                    let _ = renderer.render(framebuffer, size, Transform::Normal)?.finish()?;
                    Ok::<_, GlesError>(())
                },
            )
            .unwrap();
            assert!(receiver.try_recv().unwrap().result.is_err());
            assert!(receiver.try_recv().is_err());
            assert!(state.pending_screencopies.is_empty());
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Normal).unwrap();
            frame.clear(Color32F::new(0.0, 0.0, 1.0, 1.0), &damage).unwrap();
            frame.finish().unwrap().wait().unwrap();
            let pixels = renderer
                .copy_framebuffer(&framebuffer, Rectangle::from_size((32, 24).into()), Fourcc::Abgr8888)
                .unwrap();
            assert_eq!(&renderer.map_texture(&pixels).unwrap()[..4], &[0, 0, 255, 255]);
        }
    }

    #[test]
    fn accepts_positive_finite_scale() {
        assert_eq!(normalized_scale(1.25), 1.25);
    }

    #[test]
    fn rejects_invalid_scale() {
        assert_eq!(normalized_scale(0.0), 1.0);
        assert_eq!(normalized_scale(f64::NAN), 1.0);
        assert_eq!(normalized_scale(f64::INFINITY), 1.0);
    }
}
