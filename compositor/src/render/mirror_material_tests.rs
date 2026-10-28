use super::*;

#[test]
#[ignore = "requires an EGL rendering device"]
fn mirrored_material_repaint_preserves_the_normalized_backdrop() {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    if !crate::startup_tests::private_runtime(
        "render::mirror_material_tests::mirrored_material_repaint_preserves_the_normalized_backdrop",
    ) {
        return;
    }
    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "centered-material".into(),
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
            size: (128, 96).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "centered-material".into());
    let (window, _client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
    let surface = window.toplevel().unwrap().wl_surface().clone();
    let mut renderer = super::corner_tests::renderer();
    state.theme_settings.shell_opacity = 0.7;
    state.theme_settings.backdrop_blur = 12.0;
    state.theme_settings.material_style = crate::config::MaterialStyle::Translucent;
    // Morph panels have no shadow offset: a centered transform keeps all
    // MaterialParameters equal, making the dirty/commit distinction explicit.
    state.panel_style = ferese_panel::PanelStyle::Morph;
    let geometry = Rectangle::new((32, 24).into(), (64, 48).into());
    let make = |state: &mut Ferese, renderer: &mut GlesRenderer| {
        let (AnimatedWindowRenderElement::Blur(blur), _) = materials::material_element_with_role(
            state,
            renderer,
            &output,
            &surface,
            MaterialSurface {
                shape_origin: geometry.loc,
                geometry,
                corners: RoundedRect::from_logical(geometry, 1.0, 4.0),
                index: 0,
                capture_geometry: geometry,
                alpha: 1.0,
            },
            (crate::effects::SemanticRole::Panel, 0, 1.0),
        )
        .unwrap() else {
            panic!("translucent material must have a blur element")
        };
        blur
    };
    let backgrounds: Vec<SolidColorRenderElement> = [
        ((0, 0), [0.9_f32, 0.0, 0.0, 1.0]),
        ((64, 0), [0.0_f32, 0.0, 0.9, 1.0]),
    ]
    .into_iter()
    .map(|(origin, color)| {
        SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(origin.into(), (64, 96).into()),
            CommitCounter::default(),
            color,
            RenderElementKind::Unspecified,
        )
    })
    .collect();
    let mut target: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (128, 96).into())
        .unwrap();
    let render =
        |renderer: &mut GlesRenderer, target: &mut GlesTexture, blur: BlurRenderElement| {
            let mut scene = vec![AnimatedWindowRenderElement::Blur(blur)];
            scene.extend(
                backgrounds
                    .iter()
                    .cloned()
                    .map(AnimatedWindowRenderElement::from),
            );
            super::backdrop::update(&scene, 1.0);
            let mut framebuffer = renderer.bind(target).unwrap();
            redraw_output(renderer, &mut framebuffer, &output, &scene).unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &framebuffer,
                    Rectangle::from_size((128, 96).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            renderer.map_texture(&mapping).unwrap().to_vec()
        };
    let canvas = Output::new("normalized-mirror".into(), output.physical_properties());
    canvas.change_current_state(output.current_mode(), Some(Transform::Normal), None, None);
    let cursor_id = Id::new();
    let compose = |renderer: &mut GlesRenderer,
                   image: &mut Option<crate::backends::direct::capture::CaptureTarget>,
                   blur: BlurRenderElement,
                   cursor_x: i32| {
        // This is precisely mirror::compose's retained render call; canvas is
        // normalized to Normal and transformed source mode, unchanged at 180.
        let cursor = SolidColorRenderElement::new(
            cursor_id.clone(),
            Rectangle::new((cursor_x, 36).into(), (3, 4).into()),
            CommitCounter::default(),
            [0.0_f32, 1.0, 0.0, 1.0],
            RenderElementKind::Cursor,
        );
        let mut scene: Vec<AnimatedWindowRenderElement> = vec![cursor.into(), blur.into()];
        scene.extend(
            backgrounds
                .iter()
                .cloned()
                .map(AnimatedWindowRenderElement::from),
        );
        super::backdrop::update(&scene, 1.0);
        drop(
            crate::backends::direct::capture::render_capture_with_revision(
                renderer, image, &canvas, &scene,
            )
            .unwrap(),
        );
        let (texture, revision) = image.as_ref().unwrap().image();
        let mapping = renderer
            .copy_texture(
                &texture,
                Rectangle::from_size(texture.size()),
                Fourcc::Abgr8888,
            )
            .unwrap();
        (renderer.map_texture(&mapping).unwrap().to_vec(), revision)
    };
    let mut retained = None;
    let mut materials = MaterialResources::default();
    let initial = materials.with_scene(
        &mut state,
        &output,
        renderer.context_id().erased(),
        |state| make(state, &mut renderer),
    );
    let (_, first_revision) = compose(&mut renderer, &mut retained, initial.clone(), 40);
    let first_commit = initial.current_commit();
    output.change_current_state(None, Some(Transform::_180), None, None);
    let rotated = make(&mut state, &mut renderer);
    assert_eq!(rotated.capture_rect, initial.capture_rect);
    assert_ne!(rotated.texture.tex_id(), initial.texture.tex_id());
    assert_ne!(rotated.id(), initial.id());
    assert!(rotated.capture_dirty.load(Ordering::Relaxed));
    // The physical source uses its transformed framebuffer first. This is the
    // same context/output/region, but owns separate framebuffer samples.
    render(&mut renderer, &mut target, rotated.clone());
    assert!(!rotated.capture_dirty.load(Ordering::Relaxed));
    let normalized = materials.with_scene(
        &mut state,
        &output,
        renderer.context_id().erased(),
        |state| make(state, &mut renderer),
    );
    assert_eq!(normalized.texture.tex_id(), initial.texture.tex_id());
    assert_ne!(
        normalized.current_commit(),
        first_commit,
        "sampling invalidation must propagate through damage tracking"
    );
    let (_, after_rotation) = compose(&mut renderer, &mut retained, normalized.clone(), 40);
    assert_ne!(after_rotation, first_revision);
    let (_, duplicate) = compose(&mut renderer, &mut retained, normalized.clone(), 40);
    assert_eq!(
        duplicate, after_rotation,
        "unchanged requests must stay idle"
    );
    render(&mut renderer, &mut target, rotated);
    // Cursor damage above the material must not invalidate its backdrop, but
    // does repaint a few material pixels under the old cursor rectangle.
    let (incremental, _) = compose(&mut renderer, &mut retained, normalized.clone(), 50);
    normalized.capture_dirty.store(true, Ordering::Relaxed);
    let (fresh, _) = compose(&mut renderer, &mut None, normalized, 50);
    let differences = incremental
        .iter()
        .zip(&fresh)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        differences, 0,
        "physical and normalized mirror framebuffer caches diverged"
    );
}
