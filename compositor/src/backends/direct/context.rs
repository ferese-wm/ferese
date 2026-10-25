use std::fmt::Display;

use smithay::backend::egl::context::ContextPriority;
use smithay::backend::egl::{EGLContext, EGLDisplay, Error};

pub(super) fn try_new(display: &EGLDisplay) -> Result<EGLContext, Error> {
    // Smithay omits the hint without EGL_IMG_context_priority and logs the
    // queried priority when available. A successful request need not grant High.
    try_with_high_priority(|priority| match priority {
        Some(priority) => EGLContext::new_with_priority(display, priority),
        None => EGLContext::new(display),
    })
}

fn try_with_high_priority<T, E: Display>(
    mut create: impl FnMut(Option<ContextPriority>) -> Result<T, E>,
) -> Result<T, E> {
    create(Some(ContextPriority::High)).or_else(|error| {
        tracing::warn!(
            %error,
            "EGL context creation with a high-priority hint failed; retrying without the hint"
        );
        create(None)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_high_without_recreating_a_successful_context() {
        let mut requests = Vec::new();
        let context = try_with_high_priority(|priority| {
            requests.push(priority);
            Ok::<_, &str>("context")
        });

        assert_eq!(context, Ok("context"));
        assert_eq!(requests, [Some(ContextPriority::High)]);
    }

    #[test]
    fn retries_without_a_hint_when_priority_creation_fails() {
        let mut requests = Vec::new();
        let context = try_with_high_priority(|priority| {
            requests.push(priority);
            match priority {
                Some(_) => Err("priority rejected"),
                None => Ok("fallback context"),
            }
        });

        assert_eq!(context, Ok("fallback context"));
        assert_eq!(requests, [Some(ContextPriority::High), None]);
    }

    #[test]
    fn propagates_fallback_failure_without_retrying_again() {
        let mut requests = Vec::new();
        let context = try_with_high_priority(|priority| {
            requests.push(priority);
            match priority {
                Some(_) => Err::<(), _>("priority rejected"),
                None => Err("default creation failed"),
            }
        });

        assert_eq!(context, Err("default creation failed"));
        assert_eq!(requests, [Some(ContextPriority::High), None]);
    }

    #[test]
    #[ignore = "requires an EGL device (software Mesa is supported)"]
    fn creates_a_usable_egl_context() {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::egl::EGLDevice;
        use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
        use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
        use smithay::utils::{Physical, Rectangle, Transform};

        let software_only = std::env::var("LIBGL_ALWAYS_SOFTWARE").as_deref() == Ok("1");
        let mut tested = 0;
        for device in EGLDevice::enumerate().unwrap() {
            let software = device
                .extensions()
                .iter()
                .any(|name| name == "EGL_MESA_device_software");
            if software_only && !software {
                continue;
            }
            eprintln!(
                "EGL context test: software={software}, render_node={:?}",
                device.render_device_path().ok()
            );
            // SAFETY: the enumerated EGL device remains owned by the display.
            let display = unsafe { EGLDisplay::new(device).unwrap() };
            let context = try_new(&display).unwrap();
            // SAFETY: this fresh context is not current on another thread.
            let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
            let mut texture: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, (8, 8).into())
                .unwrap();
            let rect = Rectangle::<i32, Physical>::from_size((8, 8).into());
            {
                let mut target = renderer.bind(&mut texture).unwrap();
                {
                    let mut frame = renderer
                        .render(&mut target, (8, 8).into(), Transform::Normal)
                        .unwrap();
                    frame.clear(Color32F::new(1., 0., 0., 1.), &[rect]).unwrap();
                    frame.finish().unwrap().wait().unwrap();
                }
                let mapping = renderer
                    .copy_framebuffer(
                        &target,
                        Rectangle::from_size((8, 8).into()),
                        Fourcc::Abgr8888,
                    )
                    .unwrap();
                let pixels = renderer.map_texture(&mapping).unwrap();
                assert_eq!(pixels.len(), 8 * 8 * 4);
                assert!(
                    pixels
                        .chunks_exact(4)
                        .all(|pixel| pixel == [255, 0, 0, 255])
                );
            }
            renderer.egl_context().unbind().unwrap();
            tested += 1;
        }
        assert!(tested > 0, "no matching EGL device was exercised");
    }
}
