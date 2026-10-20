//! A target displays a composition of the source, with no second workspace or input domain.
//! Source alone releases client callbacks/presentation feedback. Targets have independent KMS clocks.
use super::*;
use smithay::backend::renderer::Texture;
use smithay::utils::{Buffer, Physical, Rectangle};

/// Fit the complete source into the transformed target; black letterbox, never crop.
fn fit(source: (i32, i32), target: (i32, i32)) -> Rectangle<i32, Physical> {
    let scale = (target.0 as f64 / source.0.max(1) as f64).min(target.1 as f64 / source.1.max(1) as f64);
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
) -> Result<Vec<crate::render::AnimatedWindowRenderElement>, smithay::backend::renderer::gles::GlesError> {
    let elements = sampled_output_elements(state, renderer, source, true, frame);
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
    compose(renderer, &mut target.mirror_texture, canvas, source, &elements)?;
    let texture = target
        .mirror_texture
        .as_ref()
        .expect("source composition has a texture")
        .clone();
    target.mirror_commit.increment();
    Ok(vec![
        fitted_texture(texture, &target.output, target.mirror_id.clone(), target.mirror_commit).into(),
    ])
}

pub(super) fn compose(
    renderer: &mut GlesRenderer,
    texture: &mut Option<GlesTexture>,
    canvas: &Output,
    source: &Output,
    elements: &[crate::render::AnimatedWindowRenderElement],
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
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
    let framebuffer = capture::render_capture(renderer, texture, canvas, elements)?;
    drop(framebuffer);
    Ok(())
}

pub(super) fn fitted_texture(
    texture: GlesTexture,
    target: &Output,
    id: smithay::backend::renderer::element::Id,
    commit: CommitCounter,
) -> crate::presentation::NativeTextureElement {
    let source_size = texture.size();
    let size = target
        .current_transform()
        .transform_size(target.current_mode().expect("target has a mode").size);
    crate::presentation::NativeTextureElement {
        id,
        commit,
        source: Rectangle::<f64, Buffer>::from_size(source_size.to_f64()),
        texture,
        geometry: fit((source_size.w, source_size.h), (size.w, size.h)),
        alpha: 1.0,
        program: None,
        uniforms: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
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
        let colors: Vec<crate::render::AnimatedWindowRenderElement> = [(0, [1., 0., 0., 1.]), (32, [0., 0., 1., 1.])]
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
        assert_eq!(texture.as_ref().unwrap().size(), (32, 64).into());
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
            let element = fitted_texture(
                texture.as_ref().unwrap().clone(),
                &target,
                Id::new(),
                CommitCounter::default(),
            );
            let mut target_texture = None;
            let framebuffer = capture::render_capture(&mut renderer, &mut target_texture, &target, &[element]).unwrap();
            let mapping = renderer
                .copy_framebuffer(&framebuffer, Rectangle::from_size((96, 64).into()), Fourcc::Abgr8888)
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
