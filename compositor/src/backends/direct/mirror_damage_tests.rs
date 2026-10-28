//! Retained mirror pixels must equal a fresh source and target composition.
use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Bind, Color32F, ExportMem};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Physical, Rectangle, Transform};

use super::{capture, compose, fitted_texture};
use crate::render::AnimatedWindowRenderElement;

fn output(name: &str, size: (i32, i32)) -> Output {
    let output = Output::new(
        name.into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: String::new(),
            model: String::new(),
            serial_number: String::new(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: size.into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    output
}

fn pixels(renderer: &mut GlesRenderer, image: &capture::CaptureTarget) -> Vec<u8> {
    use smithay::backend::renderer::Texture;
    let (mut texture, _) = image.image();
    let size = texture.size();
    let framebuffer = renderer.bind(&mut texture).unwrap();
    let mapping = renderer
        .copy_framebuffer(
            &framebuffer,
            Rectangle::from_size((size.w, size.h).into()),
            Fourcc::Abgr8888,
        )
        .unwrap();
    renderer.map_texture(&mapping).unwrap().to_vec()
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn incremental_mirrors_match_fresh_pixels_and_unchanged_requests_keep_the_revision() {
    let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
    // SAFETY: the enumerated EGLDevice supplies a live display retained by the context.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    let context = EGLContext::new(&display).unwrap();
    // SAFETY: this thread exclusively owns the fresh context through its renderer.
    let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
    for transform in [Transform::Normal, Transform::_90, Transform::Flipped270] {
        for scale in [1.0, 1.25] {
            let source = output("mirror-source", (96, 64));
            source.change_current_state(
                None,
                Some(transform),
                Some(Scale::Fractional(scale)),
                None,
            );
            let canvas = output("mirror-canvas", (1, 1));
            let target = output("mirror-target", (151, 91));
            target.change_current_state(None, None, Some(Scale::Fractional(1.5)), None);
            let mut retained = None;
            let mut destination = None;
            let background_id = Id::new();
            let cursor_id = Id::new();
            let mut cursor_commit = CommitCounter::default();
            let mut previous = None;
            let mut previous_source = None;
            for step in 0..12 {
                cursor_commit.increment();
                let bounds = transform.transform_size((96, 64).into());
                let cursor = SolidColorRenderElement::new(
                    cursor_id.clone(),
                    Rectangle::new((3 + step * 3, 9).into(), (3, 5).into()),
                    cursor_commit,
                    Color32F::new(1.0, (step % 2) as f32, 0.0, 1.0),
                    Kind::Cursor,
                );
                let background = SolidColorRenderElement::new(
                    background_id.clone(),
                    Rectangle::from_size(bounds),
                    CommitCounter::default(),
                    Color32F::new(0.0, 0.0, 0.8, 1.0),
                    Kind::Unspecified,
                );
                let elements: Vec<AnimatedWindowRenderElement> =
                    vec![cursor.into(), background.into()];
                compose(&mut renderer, &mut retained, &canvas, &source, &elements).unwrap();
                let revision = retained.as_ref().unwrap().image().1;
                compose(&mut renderer, &mut retained, &canvas, &source, &elements).unwrap();
                assert_eq!(
                    retained.as_ref().unwrap().image().1,
                    revision,
                    "redundant mirror render advanced revision"
                );
                let mut fresh = None;
                compose(&mut renderer, &mut fresh, &canvas, &source, &elements).unwrap();
                assert_eq!(
                    pixels(&mut renderer, retained.as_ref().unwrap()),
                    pixels(&mut renderer, fresh.as_ref().unwrap()),
                    "source step={step} {transform:?} {scale}"
                );
                let element = fitted_texture(retained.as_ref().unwrap(), &target);
                let damage = element.damage_since(1.5.into(), previous_source);
                previous_source = Some(element.current_commit());
                if step > 0 {
                    let area: i32 = damage.iter().map(|rect| rect.size.w * rect.size.h).sum();
                    assert!(
                        area > 0 && area < element.geometry.size.w * element.geometry.size.h,
                        "cursor must damage only a portion of the fitted target: {damage:?}"
                    );
                }
                if step == 3 {
                    // Same source pixels, different fitting/rotation on the target.
                    target.change_current_state(
                        Some(Mode {
                            size: (73, 51).into(),
                            refresh: 144_000,
                        }),
                        Some(Transform::_90),
                        None,
                        None,
                    );
                }
                // Deliberately leave several source revisions unconsumed. Damage
                // history expiry must fall back to a full target update.
                if (4..10).contains(&step) {
                    continue;
                }
                let element = fitted_texture(retained.as_ref().unwrap(), &target);
                if step == 10 {
                    assert_eq!(
                        &*element.damage_since(1.5.into(), previous),
                        &[Rectangle::<i32, Physical>::from_size(element.geometry.size)]
                    );
                }
                let commit = element.current_commit();
                drop(
                    capture::render_capture_with_revision(
                        &mut renderer,
                        &mut destination,
                        &target,
                        &[element],
                    )
                    .unwrap(),
                );
                let mut reference = None;
                let fresh_element = fitted_texture(fresh.as_ref().unwrap(), &target);
                drop(
                    capture::render_capture_with_revision(
                        &mut renderer,
                        &mut reference,
                        &target,
                        &[fresh_element],
                    )
                    .unwrap(),
                );
                assert_eq!(
                    pixels(&mut renderer, destination.as_ref().unwrap()),
                    pixels(&mut renderer, reference.as_ref().unwrap()),
                    "target step={step} {transform:?} {scale}"
                );
                previous = Some(commit);
            }
            let old_id = fitted_texture(retained.as_ref().unwrap(), &target)
                .id()
                .clone();
            source.change_current_state(
                Some(Mode {
                    size: (80, 60).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                None,
            );
            compose(&mut renderer, &mut retained, &canvas, &source, &[]).unwrap();
            assert_ne!(
                fitted_texture(retained.as_ref().unwrap(), &target).id(),
                &old_id,
                "a resized source needs a new image identity"
            );
        }
    }
}
