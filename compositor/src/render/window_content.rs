//! Live, overview and retained views use the same snapshot mapping and clip.
use super::*;

pub(super) fn snapshot_element(
    snapshot: &ResizeSnapshot,
    presentation: crate::presentation::WindowPresentation,
    corners: RoundedRect,
    scale: f64,
    output: &Output,
    programs: &RoundedClipPrograms,
    handoff: f32,
) -> Option<NativeTextureElement> {
    let pixels = corners.rect;
    let size = snapshot.texture.size();
    let (sx, sy) = if presentation.scale_content {
        (
            f64::from(pixels.size.w) / f64::from(size.w),
            f64::from(pixels.size.h) / f64::from(size.h),
        )
    } else if let Some(native) = presentation.native_size {
        (
            f64::from(pixels.size.w) / (f64::from(native.width) * scale).round().max(1.0),
            f64::from(pixels.size.h) / (f64::from(native.height) * scale).round().max(1.0),
        )
    } else {
        (1.0, 1.0)
    };
    let visible = Rectangle::new(
        pixels.loc,
        (
            (f64::from(size.w) * sx).round() as i32,
            (f64::from(size.h) * sy).round() as i32,
        )
            .into(),
    )
    .intersection(pixels)?;
    let clip = framebuffer_clip_rect(pixels, output.current_mode()?.size, output.current_transform().invert());
    Some(NativeTextureElement {
        id: snapshot.id.clone(),
        commit: snapshot.commit,
        texture: snapshot.texture.clone(),
        geometry: visible,
        source: Rectangle::from_size(Size::from((
            f64::from(visible.size.w) / sx,
            f64::from(visible.size.h) / sy,
        ))),
        alpha: presentation.alpha() * handoff,
        program: Some(programs.texture.clone()),
        uniforms: vec![Uniform::new("clip_rect", clip), Uniform::new("radius", corners.radius)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn snapshot_views_keep_fractional_edges_and_close_pixels() {
        use ferese_animation::{AnimatedRect, AnimatedValue};
        use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
        use smithay::output::{Mode, PhysicalProperties, Subpixel};
        let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let output = Output::new(
            "snapshot-test".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (128, 128).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        let mut resources = RenderResources::default();
        let programs = corner_program(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();
        for (scale, focus_alpha) in [1.0, 1.25, 1.5, 2.0]
            .into_iter()
            .flat_map(|scale| [1.0, 0.8].map(|alpha| (scale, alpha)))
        {
            let live = crate::presentation::WindowPresentation {
                focus_alpha,
                dim: 0.0,
                id: ferese_layout::WindowId(1),
                bounds: AnimatedRect::new(ferese_layout::Rect::new(20.25, 20.5, 32.25, 24.25)),
                opacity: AnimatedValue::new(1.0),
                emphasis: AnimatedValue::new(1.0),
                shadow: AnimatedValue::new(1.0),
                scale_content: false,
                native_size: None,
            };
            let corners =
                RoundedRect::new(live.bounds.current, (0, 0).into(), scale, 8.0).with_shape(CornerShape::Continuous);
            let size = corners.rect.size;
            let mut texture: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())
                .unwrap();
            {
                let mut target = renderer.bind(&mut texture).unwrap();
                let mut frame = renderer.render(&mut target, size, Transform::Normal).unwrap();
                frame
                    .clear(Color32F::new(1.0, 0.0, 0.0, 1.0), &[Rectangle::from_size(size)])
                    .unwrap();
                let _ = frame.finish().unwrap();
            }
            let snapshot = ResizeSnapshot {
                texture,
                context: renderer.context_id().erased(),
                id: Id::new(),
                commit: CommitCounter::default(),
                elapsed: Duration::ZERO,
                last_tick: None,
                scale,
            };
            let mut closing = live;
            closing.close();
            let mut pixels = Vec::new();
            for presentation in [live, closing] {
                let element =
                    snapshot_element(&snapshot, presentation, corners, scale, &output, &programs, 1.0).unwrap();
                assert_eq!(element.geometry, corners.rect);
                let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (128, 128).into()).unwrap();
                {
                    let mut target = renderer.bind(&mut texture).unwrap();
                    let mut frame = renderer
                        .render(&mut target, (128, 128).into(), Transform::Normal)
                        .unwrap();
                    let damage = Rectangle::from_size((128, 128).into());
                    frame.clear(Color32F::TRANSPARENT, &[damage]).unwrap();
                    draw_render_elements(
                        &mut frame,
                        scale,
                        &[AnimatedWindowRenderElement::from(element)],
                        &[damage],
                    )
                    .unwrap();
                    let _ = frame.finish().unwrap();
                }
                let mapping = renderer
                    .copy_texture(&texture, Rectangle::from_size((128, 128).into()), Fourcc::Abgr8888)
                    .unwrap();
                pixels.push(renderer.map_texture(&mapping).unwrap().to_vec());
            }
            assert_eq!(pixels[0], pixels[1], "close handoff changed pixels at scale {scale}");
            let expected = (focus_alpha * 255.0).round() as u8;
            assert!(
                pixels[0].chunks_exact(4).any(|pixel| pixel[3].abs_diff(expected) <= 1),
                "snapshot alpha must be applied once"
            );
            assert!(
                pixels[0]
                    .chunks_exact(4)
                    .all(|pixel| pixel[3] <= expected.saturating_add(1))
            );
            let mut retained = super::super::closing::ClosedWindow {
                presentation: closing,
                output: ferese_core::OutputId(1),
                below: None,
                snapshot: snapshot.clone(),
                handoff: Some((snapshot.clone(), None)),
                radius: 8.0,
                shape: CornerShape::Continuous,
                decorations: 1.0,
                fill: None,
                material: None,
            };
            // A partially faded resize must keep fading after unmap, including
            // in predicted frames, without advancing authoritative state twice.
            retained.handoff.as_mut().unwrap().0.elapsed = Duration::from_millis(20);
            let mut predicted = retained.clone();
            predicted.advance(Duration::from_millis(20), Default::default());
            assert_eq!(predicted.presentation.focus_alpha, focus_alpha);
            let faded = &predicted.handoff.as_ref().unwrap().0;
            assert_eq!(faded.elapsed, Duration::from_millis(40));
            assert_eq!(retained.handoff.as_ref().unwrap().0.elapsed, Duration::from_millis(20));
            let element = snapshot_element(
                faded,
                predicted.presentation,
                corners,
                scale,
                &output,
                &programs,
                crate::presentation::handoff_alpha(faded.elapsed),
            )
            .unwrap();
            assert!((element.alpha - predicted.presentation.alpha() * 0.5).abs() < 1e-6);
            assert_eq!(element.geometry, corners.rect);
            retained.advance(Duration::from_millis(60), Default::default());
            assert!(retained.handoff.is_none());
            let thumbnail = live.thumbnail(ferese_layout::Rect::new(4.25, 3.75, 16.5, 12.25));
            let corners = RoundedRect::new(thumbnail.bounds.current, (0, 0).into(), scale, 4.0);
            let element = snapshot_element(&snapshot, thumbnail, corners, scale, &output, &programs, 1.0).unwrap();
            assert_eq!(element.geometry, corners.rect);
            assert!((element.source.size.w - f64::from(size.w)).abs() < 1e-9);
            assert!((element.source.size.h - f64::from(size.h)).abs() < 1e-9);
        }
    }
}
