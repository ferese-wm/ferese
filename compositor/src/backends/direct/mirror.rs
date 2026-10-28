//! A target displays a composition of the source, with no second workspace or input domain.
//! Source alone releases client callbacks/presentation feedback. Targets have independent KMS clocks.
#[cfg(test)]
use smithay::backend::allocator::Fourcc;
#[cfg(test)]
use smithay::output::Scale;

use super::{
    DirectOutput, Ferese, GlesRenderer, Output, OutputMode, PhysicalProperties, Subpixel,
    Transform, capture, sampled_output_elements,
};
use smithay::backend::renderer::damage::Error as DamageError;
use smithay::backend::renderer::{Renderer, Texture};
use smithay::utils::{Physical, Rectangle};

#[cfg(test)]
#[path = "mirror_damage_tests.rs"]
mod damage_tests;

/// Fit the complete source into the transformed target; black letterbox, never crop.
fn fit(source: (i32, i32), target: (i32, i32)) -> Rectangle<i32, Physical> {
    let scale =
        (target.0 as f64 / source.0.max(1) as f64).min(target.1 as f64 / source.1.max(1) as f64);
    let width = (source.0 as f64 * scale).round().max(1.0) as i32;
    let height = (source.1 as f64 * scale).round().max(1.0) as i32;
    Rectangle::new(
        ((target.0 - width) / 2, (target.1 - height) / 2).into(),
        (width, height).into(),
    )
}

pub(super) fn elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    source: &Output,
    frame: &crate::state::FrameScene,
    target: &mut DirectOutput,
) -> Result<
    Vec<crate::render::AnimatedWindowRenderElement>,
    DamageError<smithay::backend::renderer::gles::GlesError>,
> {
    let elements = target.mirror_materials.with_scene(
        state,
        source,
        renderer.context_id().erased(),
        |state| sampled_output_elements(state, renderer, source, true, frame),
    );
    // Target renderer owns this texture, including when source and target use different GPUs.
    let canvas = target.mirror_canvas.get_or_insert_with(|| {
        Output::new(
            "mirror-canvas".into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Ferese".into(),
                model: "Mirror composition".into(),
            },
        )
    });
    compose(
        renderer,
        &mut target.mirror_texture,
        canvas,
        source,
        &elements,
    )?;
    let image = target
        .mirror_texture
        .as_ref()
        .expect("source composition has a texture");
    Ok(vec![fitted_texture(image, &target.output).into()])
}

pub(super) fn compose(
    renderer: &mut GlesRenderer,
    texture: &mut Option<capture::CaptureTarget>,
    canvas: &Output,
    source: &Output,
    elements: &[crate::render::AnimatedWindowRenderElement],
) -> Result<(), DamageError<smithay::backend::renderer::gles::GlesError>> {
    configure_canvas(canvas, source);
    let result = capture::render_capture_with_revision(renderer, texture, canvas, elements)
        .map(|(framebuffer, _)| drop(framebuffer));
    if result.is_err() {
        // A failed draw may already have changed pixels. Recreate both image
        // and history before another attempt, rather than exposing that image.
        *texture = None;
    }
    result
}

pub(super) fn configure_canvas(canvas: &Output, source: &Output) {
    let source_mode = source.current_mode().expect("source has a mode");
    canvas.change_current_state(
        Some(OutputMode {
            size: source.current_transform().transform_size(source_mode.size),
            refresh: source_mode.refresh,
        }),
        Some(Transform::Normal),
        Some(source.current_scale()),
        None,
    );
}

pub(super) fn fitted_texture(
    image: &capture::CaptureTarget,
    target: &Output,
) -> crate::presentation::NativeTextureElement {
    let (texture, _) = image.image();
    let source_size = texture.size();
    let size = target
        .current_transform()
        .transform_size(target.current_mode().expect("target has a mode").size);
    image.element(fit((source_size.w, source_size.h), (size.w, size.h)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::utils::CommitCounter;

    #[test]
    fn different_native_sizes_fit_without_cropping() {
        assert_eq!(
            fit((1920, 1080), (1280, 1024)),
            Rectangle::new((0, 152).into(), (1280, 720).into())
        );
        assert_eq!(
            fit((1920, 1080), (1080, 1920)),
            Rectangle::new((0, 656).into(), (1080, 608).into())
        );
    }
    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn mirrored_pixels_fit_rotated_source_and_target_without_cropping() {
        use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
        use smithay::backend::renderer::element::solid::SolidColorRenderElement;
        use smithay::backend::renderer::element::{Id, Kind};
        use smithay::backend::renderer::{Color32F, ExportMem};
        let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
        // SAFETY: The native display comes from a live enumerated EGLDevice and its lifetime is
        // retained by the EGL objects created here.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues
        // GL work only through its renderer methods.
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
        let source = output("source");
        source.change_current_state(
            Some(OutputMode {
                size: (64, 32).into(),
                refresh: 60_000,
            }),
            Some(Transform::_90),
            Some(Scale::Fractional(2.0)),
            None,
        );
        let canvas = output("canvas");
        let colors: Vec<crate::render::AnimatedWindowRenderElement> =
            [(0, [1., 0., 0., 1.]), (32, [0., 0., 1., 1.])]
                .into_iter()
                .map(|(y, color)| {
                    SolidColorRenderElement::new(
                        Id::new(),
                        Rectangle::new((0, y).into(), (32, 32).into()),
                        CommitCounter::default(),
                        Color32F::from(color),
                        Kind::Unspecified,
                    )
                    .into()
                })
                .collect();
        let mut texture = None;
        compose(&mut renderer, &mut texture, &canvas, &source, &colors).unwrap();
        assert_eq!(texture.as_ref().unwrap().image().0.size(), (32, 64).into());
        let target = output("target");
        for (transform, colored_area) in [(Transform::Normal, 2048), (Transform::_90, 4608)] {
            target.change_current_state(
                Some(OutputMode {
                    size: (96, 64).into(),
                    refresh: 120_000,
                }),
                Some(transform),
                Some(Scale::Fractional(1.5)),
                None,
            );
            let element = fitted_texture(texture.as_ref().unwrap(), &target);
            let mut target_texture = None;
            let framebuffer =
                capture::render_capture(&mut renderer, &mut target_texture, &target, &[element])
                    .unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &framebuffer,
                    Rectangle::from_size((96, 64).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            let pixels = renderer.map_texture(&mapping).unwrap();
            let red = pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[0] > 100 && pixel[0] > pixel[2])
                .count();
            let blue = pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[2] > 100 && pixel[2] > pixel[0])
                .count();
            assert_eq!(red, colored_area / 2, "{transform:?}");
            assert_eq!(blue, colored_area / 2, "{transform:?}");
        }
    }
}
