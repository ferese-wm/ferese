use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::gles::{GlesError, GlesRenderer, GlesTarget, GlesTexture};
use smithay::backend::renderer::{Bind, Offscreen, Texture};
use smithay::output::Output;
use std::fmt::Display;

use crate::Ferese;
use crate::render::{capture_output_elements, redraw_output};
use crate::state::FrameScene;

pub(crate) fn capture_output(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    texture: &mut Option<GlesTexture>,
    output: &Output,
    scene: &FrameScene,
) {
    for include_cursor in [true, false] {
        if !state.has_pending_screencopy(output, include_cursor) {
            continue;
        }

        // Sample the same scene into an independent target: the displayed
        // primary buffer may omit both a scanned-out client and hardware cursor.
        let elements = capture_output_elements(state, renderer, output, include_cursor, scene);
        let Some(target) = capture_request(state, output, include_cursor, || {
            render_capture(renderer, texture, output, &elements)
        }) else {
            continue;
        };
        state.process_screencopies(renderer, &target, output, include_cursor);
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
        let elements = capture_output_elements(state, renderer, source, include_cursor, scene);
        // Never sample mirror_texture: it contains the unfiltered display.
        let canvas = output.mirror_canvas.as_ref().expect("mirror display composed");
        let Some(()) = capture_request(state, &output.output, include_cursor, || {
            super::mirror::compose(renderer, &mut output.mirror_capture_texture, canvas, source, &elements)
        }) else {
            continue;
        };
        let element = super::mirror::fitted_texture(
            output.mirror_capture_texture.as_ref().unwrap().clone(),
            &output.output,
            smithay::backend::renderer::element::Id::new(),
            Default::default(),
        );
        let Some(target) = capture_request(state, &output.output, include_cursor, || {
            render_capture(renderer, &mut output.capture_texture, &output.output, &[element])
        }) else {
            continue;
        };
        state.process_screencopies(renderer, &target, &output.output, include_cursor);
    }
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

pub(super) fn render_capture<'a, E: RenderElement<GlesRenderer>>(
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
    fn capture_recomposes_cursor_without_touching_the_display_target() {
        let device = EGLDevice::enumerate().unwrap().last().expect("an EGL device");
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let output = Output::new(
            "capture".into(),
            PhysicalProperties {
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
            let _ = render_capture(
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
                .copy_framebuffer(&target, Rectangle::from_size((32, 24).into()), Fourcc::Abgr8888)
                .unwrap();
            let pixels = renderer.map_texture(&mapping).unwrap();
            assert_eq!(&pixels[..4], &expected);
            assert_eq!(&pixels[(12 * 32 + 16) * 4..(12 * 32 + 16) * 4 + 4], &[255, 0, 0, 255]);
            drop(target);

            let target = renderer.bind(displayed.as_mut().unwrap()).unwrap();
            let mapping = renderer
                .copy_framebuffer(&target, Rectangle::from_size((32, 24).into()), Fourcc::Abgr8888)
                .unwrap();
            assert_eq!(&renderer.map_texture(&mapping).unwrap()[..4], &[255, 0, 0, 255]);
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
        let _ = render_capture::<SolidColorRenderElement>(&mut renderer, &mut capture, &output, &[]).unwrap();
        assert_eq!(capture.as_ref().unwrap().size(), (48, 16).into());
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
            assert!(submitted.get(), "capture must run after the display queue succeeds");
            assert!(
                capture_request(&mut state, &output, false, || Err::<(), _>(
                    "injected allocation failure"
                ))
                .is_none()
            );
        })
        .unwrap();

        assert!(submitted.get(), "capture failure must not undo queued display work");
        assert_eq!(state.pending_screencopies.len(), 1);
        assert_eq!(state.pending_screencopies[0].output, other_output);
        let outcome = receiver.try_recv().unwrap();
        assert_eq!(outcome.request, 41);
        assert_eq!(outcome.part, 0);
        assert!(outcome.result.is_err());
        assert!(receiver.try_recv().is_err(), "request should fail exactly once");
        assert!(
            other_receiver.try_recv().is_err(),
            "other output's request must stay pending"
        );
    }
}
