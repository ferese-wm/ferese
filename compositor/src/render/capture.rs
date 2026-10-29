use super::{
    Arc, Bind, Color32F, CommitCounter, Duration, ExportMem, Ferese, Fourcc, Frame, GlesError,
    GlesRenderer, GlesTexture, Id, Logical, Offscreen, Physical, Point, Rectangle,
    RenderElementKind, Renderer, ResizeSnapshot, Size, Texture, Transform,
    WaylandSurfaceRenderElement, cursor_elements, draw_render_elements,
    render_elements_from_surface_tree,
};

#[derive(Default)]
struct CaptureTarget(std::sync::Mutex<Option<GlesTexture>>);

pub(crate) fn capture_resize_snapshot(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    remaining: usize,
) -> Result<Option<ResizeSnapshot>, GlesError> {
    // Called before on_commit_buffer_handler: renderer surface state still
    // owns the previous buffer during an actual resize handoff.
    render_window_texture(renderer, window, geometry, scale, remaining, None, &[], 1.0)
}

#[allow(clippy::too_many_arguments)]
fn render_window_texture(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    remaining: usize,
    reusable: Option<GlesTexture>,
    background: &[super::AnimatedWindowRenderElement],
    alpha: f32,
) -> Result<Option<ResizeSnapshot>, GlesError> {
    let Some(toplevel) = window.toplevel() else {
        return Ok(None);
    };
    let size: Size<i32, Physical> = geometry.size.to_physical_precise_round(scale);
    let bytes = (size.w.max(0) as usize)
        .saturating_mul(size.h.max(0) as usize)
        .saturating_mul(4);
    if bytes == 0 || bytes > remaining {
        return Ok(None);
    }
    let content = render_elements_from_surface_tree::<
        GlesRenderer,
        WaylandSurfaceRenderElement<GlesRenderer>,
    >(
        renderer,
        toplevel.wl_surface(),
        Point::<i32, Logical>::from((-geometry.loc.x, -geometry.loc.y))
            .to_physical_precise_round(scale),
        scale,
        alpha,
        RenderElementKind::Unspecified,
    );
    if content.is_empty() {
        return Ok(None);
    }
    let mut texture = match reusable.filter(|texture| texture.size() == (size.w, size.h).into()) {
        Some(texture) => texture,
        None => Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            (size.w, size.h).into(),
        )?,
    };
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
        let damage = Rectangle::from_size(size);
        let clear = if background.is_empty() {
            Color32F::new(0.0, 0.0, 0.0, 0.0)
        } else {
            Color32F::new(0.035, 0.04, 0.055, 1.0)
        };
        frame.clear(clear, &[damage])?;
        draw_render_elements(&mut frame, scale, background, &[damage])?;
        draw_render_elements(&mut frame, scale, &content, &[damage])?;
        let _ = frame.finish()?;
    }
    Ok(Some(ResizeSnapshot {
        texture,
        opaque: super::window_blur::client_is_opaque(window),
        context: renderer.context_id().erased(),
        id: Id::new(),
        commit: CommitCounter::default(),
        elapsed: Duration::ZERO,
        last_tick: None,
        scale,
    }))
}

fn window_mapping(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    cursor: Option<(&Ferese, Rectangle<i32, Logical>)>,
) -> Result<
    (
        smithay::backend::renderer::gles::GlesMapping,
        Size<i32, smithay::utils::Buffer>,
        usize,
    ),
    String,
> {
    // One reusable render target per EGL context. Resize handoff snapshots keep
    // their own textures because they outlive the capture call.
    let cache = renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Arc::new(CaptureTarget::default()))
        .clone();
    let reusable = cache
        .0
        .lock()
        .expect("the reusable capture texture cache must remain consistent across callbacks")
        .take();
    let mut snapshot = render_window_texture(
        renderer,
        window,
        geometry,
        scale,
        128 * 1024 * 1024,
        reusable,
        &[],
        1.0,
    )
    .map_err(|error| error.to_string())?
    .ok_or("Window has no capturable content")?;
    let size = snapshot.texture.size();
    if let Some((state, cursor_geometry)) = cursor {
        let elements = cursor_elements(state, renderer, cursor_geometry, scale);
        let mut target = renderer
            .bind(&mut snapshot.texture)
            .map_err(|e| e.to_string())?;
        let mut frame = renderer
            .render(&mut target, (size.w, size.h).into(), Transform::Normal)
            .map_err(|e| e.to_string())?;
        draw_render_elements(
            &mut frame,
            scale,
            &elements,
            &[Rectangle::from_size((size.w, size.h).into())],
        )
        .map_err(|e| e.to_string())?;
        let _ = frame.finish().map_err(|e| e.to_string())?;
    }
    let mapping = renderer
        .copy_texture(
            &snapshot.texture,
            Rectangle::from_size(size),
            Fourcc::Abgr8888,
        )
        .map_err(|error| error.to_string())?;
    if mapping.format() != Some(Fourcc::Abgr8888) {
        return Err("Unsupported window readback format".into());
    }
    let bytes = (size.w as usize)
        .checked_mul(size.h as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("Window is too large")?;
    *cache
        .0
        .lock()
        .expect("the reusable capture texture cache must remain consistent across callbacks") =
        Some(snapshot.texture);

    Ok((mapping, size, bytes))
}

pub(crate) struct WindowReadback {
    pub readback: crate::handlers::readback::Readback,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
}

pub(crate) fn prepare_window_readback(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    cursor: Option<(&Ferese, Rectangle<i32, Logical>)>,
    admission: &crate::handlers::readback::Readbacks,
) -> Result<WindowReadback, String> {
    let size: Size<i32, Physical> = geometry.size.to_physical_precise_round(scale);
    let bytes = usize::try_from(size.w)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .and_then(|row| {
            usize::try_from(size.h)
                .ok()
                .and_then(|height| row.checked_mul(height))
        })
        .ok_or("Invalid window readback dimensions")?;
    admission.prepare(renderer, bytes)?;
    let (mapping, size, bytes) = window_mapping(renderer, window, geometry, scale, cursor)?;
    Ok(WindowReadback {
        readback: crate::handlers::readback::Readback::try_new(renderer, mapping, bytes)?,
        width: size.w,
        height: size.h,
        stride: size.w as usize * 4,
    })
}

/// Convert premultiplied ABGR readback into the capture protocol's straight ARGB.
pub(crate) fn convert_window_pixels(pixels: &mut [u8]) {
    // Compile the rounded, saturated conversion into 64 KiB of read-only data.
    // Capture threads share this table without allocating or dividing per pixel.
    static STRAIGHT_CHANNELS: [[u8; 256]; 256] = {
        let mut values = [[0; 256]; 256];
        let mut alpha = 1;
        while alpha < 256 {
            let mut channel = 0;
            while channel < 256 {
                let straight = (channel * 255 + alpha / 2) / alpha;
                values[alpha][channel] = if straight > 255 { 255 } else { straight as u8 };
                channel += 1;
            }
            alpha += 1;
        }
        values
    };
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        let alpha = pixel[3];
        // Opaque content needs only the channel swap; avoid three divisions
        // per pixel for the usual window-sharing frame.
        if alpha == 255 {
            continue;
        }
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        let channels = &STRAIGHT_CHANNELS[usize::from(alpha)];
        for channel in &mut pixel[..3] {
            *channel = channels[usize::from(*channel)];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires private Wayland sockets and an offscreen EGL device"]
    fn window_stream_blurs_wallpaper_without_sampling_other_clients() {
        if !crate::startup_tests::private_runtime(
            "render::capture::tests::window_stream_blurs_wallpaper_without_sampling_other_clients",
        ) {
            return;
        }
        use smithay::backend::renderer::ImportMem;
        let mut renderer = super::super::corner_tests::renderer();
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = smithay::output::Output::new(
            "window-stream-test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "window-stream-test".into());
        let (window, _client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
        // An unrelated green client must never enter the window backdrop.
        let (_other, _other_client) =
            crate::state::test_window(&mut state, &mut events, 0xff00ff00);
        state.window_effects.enabled = true;
        state.window_effects.blur = true;
        state.window_effects.opacity = 0.5;
        state.theme_settings.backdrop_blur = 16.0;
        let workspace = state.workspaces.active_id();
        let view =
            crate::handlers::workspace_capture::WorkspaceSource::new(&state, workspace, false)
                .unwrap();
        let scene = state.sample_workspace_frame(workspace, &output);
        assert!(
            scene
                .windows
                .values()
                .all(|sample| sample.presentation.window_alpha == 0.5)
        );
        let mut resources = super::super::RenderResources::default();
        let elements = super::super::capture_workspace_elements(
            &mut state,
            &mut renderer,
            &view,
            false,
            &scene,
            &mut resources,
        );
        assert!(
            elements.iter().any(|element| matches!(
                element,
                super::super::AnimatedWindowRenderElement::Blur(_)
            )),
            "workspace descriptors must support window blur"
        );
        assert!(
            state
                .render
                .windows
                .values()
                .all(|window| window.blur.is_empty()),
            "capture must not populate display blur caches"
        );
        let info = crate::handlers::window_capture::WindowInfo {
            geometry: window.geometry(),
            window,
            output,
            scale: 1.0,
            bytes: 64 * 48 * 4,
        };
        let background = stream_window_background(&mut state, &mut renderer, &info, 0.5);
        assert!(matches!(
            background.first(),
            Some(super::super::AnimatedWindowRenderElement::Blur(_))
        ));
        // A stripe pattern provides a visible filtering signal independently
        // of asynchronous wallpaper decoding.
        let size = info.geometry.size;
        let pixels = (0..size.w * size.h)
            .flat_map(|i| {
                if i % 2 == 0 {
                    [0, 0, 255, 255]
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect::<Vec<_>>();
        let texture = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (size.w, size.h).into(), false)
            .unwrap();
        let wallpaper: super::super::AnimatedWindowRenderElement =
            crate::presentation::NativeTextureElement {
                id: Id::new(),
                commit: CommitCounter::default(),
                damage: None,
                texture,
                geometry: Rectangle::from_size((size.w, size.h).into()),
                source: Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into()),
                alpha: 1.0,
                program: None,
                uniforms: vec![],
            }
            .into();
        let elements = vec![background.into_iter().next().unwrap(), wallpaper];
        super::super::backdrop::update(&elements, 1.0);
        let snapshot = render_window_texture(
            &mut renderer,
            &info.window,
            info.geometry,
            1.0,
            128 * 1024 * 1024,
            None,
            &elements,
            0.5,
        )
        .unwrap()
        .unwrap();
        let mapping = renderer
            .copy_texture(
                &snapshot.texture,
                Rectangle::from_size(snapshot.texture.size()),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let pixels = renderer.map_texture(&mapping).unwrap();
        let center = ((size.h / 2 * size.w + size.w / 2) * 4) as usize;
        for pixel in pixels[center..center + 8].chunks_exact(4) {
            assert!((126..=129).contains(&pixel[0]), "red content: {pixel:?}");
            assert!(pixel[1] <= 1, "another client leaked: {pixel:?}");
            assert!(
                (40..=90).contains(&pixel[2]),
                "wallpaper was not blurred: {pixel:?}"
            );
            assert_eq!(pixel[3], 255);
        }
        state.window_effects.blur = false;
        assert!(stream_window_background(&mut state, &mut renderer, &info, 0.5).is_empty());
    }

    #[test]
    #[ignore = "requires private Wayland sockets and an offscreen EGL device"]
    fn retained_window_capture_matches_fresh_after_cursor_and_appearance_changes() {
        if !crate::startup_tests::private_runtime(
            "render::capture::tests::retained_window_capture_matches_fresh_after_cursor_and_appearance_changes",
        ) {
            return;
        }
        use smithay::input::pointer::{CursorImageStatus, MotionEvent};
        use smithay::utils::SERIAL_COUNTER;
        let mut renderer = super::super::corner_tests::renderer();
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = smithay::output::Output::new(
            "retained-window-test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "retained-window-test".into());
        let (window, mut wire) = crate::state::test_window(&mut state, &mut events, 0x80800000);
        let (cursor, _cursor_client) =
            crate::state::test_window(&mut state, &mut events, 0xff00ff00);
        state.window_effects.enabled = false;
        state.window_effects.blur = false;
        let mut info = crate::handlers::window_capture::WindowInfo {
            geometry: window.geometry(),
            window,
            output,
            scale: 1.0,
            bytes: 64 * 48 * 4,
        };
        let read = |renderer: &mut GlesRenderer, texture: &GlesTexture| {
            let mapping = renderer
                .copy_texture(
                    texture,
                    Rectangle::from_size(texture.size()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            renderer.map_texture(&mapping).unwrap().to_vec()
        };
        let (texture, first, straight) =
            stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        assert!(straight);
        let first_pixels = read(&mut renderer, &texture);
        assert!(
            first_pixels
                .chunks_exact(4)
                .all(|pixel| pixel[1] == 0 && pixel[3] == 128),
            "transparent window capture must not become an opaque output composition"
        );
        let (same, second, _) =
            stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        assert_eq!(
            first, second,
            "matching consumers must reuse an unchanged source composition"
        );
        assert_eq!(texture.tex_id(), same.tex_id());
        use smithay::reexports::wayland_server::Resource;
        let source_buffer = info.window.toplevel().unwrap().wl_surface().client().unwrap()
            .object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer>(&state.display_handle, 6).unwrap();
        assert!(
            matches!(
                capture_stream_window(
                    &mut renderer,
                    &mut state,
                    &info,
                    None,
                    &source_buffer,
                    Some(first)
                )
                .unwrap(),
                WindowStreamCopy::Unchanged
            ),
            "an unchanged source must not validate, submit or overwrite another destination"
        );

        let pointer = state.seat.get_pointer().unwrap();
        pointer.motion(
            &mut state,
            None,
            &MotionEvent {
                location: (10.0, 10.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: smithay::backend::input::InputTime::from_millis(0),
            },
        );
        state.cursor_status =
            CursorImageStatus::Surface(cursor.toplevel().unwrap().wl_surface().clone());
        let (with_cursor, old_cursor_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        let old_pixels = read(&mut renderer, &with_cursor);
        pointer.motion(
            &mut state,
            None,
            &MotionEvent {
                location: (13.0, 10.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: smithay::backend::input::InputTime::from_millis(1),
            },
        );
        let (moved, new_cursor_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        assert_eq!(
            with_cursor.tex_id(),
            moved.tex_id(),
            "cursor movement must retain the source allocation"
        );
        assert_ne!(old_cursor_revision, new_cursor_revision);
        let moved_pixels = read(&mut renderer, &moved);
        assert_ne!(old_pixels, moved_pixels);
        let old_only = (10 * 64 + 10) * 4;
        assert_eq!(
            &moved_pixels[old_only..old_only + 4],
            &first_pixels[old_only..old_only + 4],
            "old cursor pixels must restore selected-window content"
        );
        clear_stream_capture_targets(&mut renderer);
        let (fresh, _, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        assert_eq!(
            moved_pixels,
            read(&mut renderer, &fresh),
            "incremental cursor damage must match a complete fresh composition"
        );
        state.capture_epoch = state.capture_epoch.wrapping_add(1);
        state.window_effects.enabled = true;
        state.window_effects.opacity = 0.5;
        state.window_effects.blur = false;
        let (changed, changed_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        assert_eq!(
            fresh.tex_id(),
            changed.tex_id(),
            "epoch and appearance invalidation must retain a compatible texture"
        );
        let changed_pixels = read(&mut renderer, &changed);
        assert_ne!(moved_pixels, changed_pixels);
        let (unchanged, settled_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        assert_eq!(changed_revision, settled_revision);
        assert_eq!(changed.tex_id(), unchanged.tex_id());
        clear_stream_capture_targets(&mut renderer);
        let (fresh, fresh_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        assert_eq!(changed_pixels, read(&mut renderer, &fresh));
        let output = smithay::output::Output::new(
            "other-window-output".into(),
            info.output.physical_properties(),
        );
        output.change_current_state(
            info.output.current_mode(),
            Some(Transform::Normal),
            Some(info.output.current_scale()),
            None,
        );
        info.output = output;
        let (migrated, migrated_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, Some(info.geometry)).unwrap();
        assert_eq!(fresh.tex_id(), migrated.tex_id());
        assert_ne!(
            migrated_revision, fresh_revision,
            "same-scale output migration must invalidate source ownership"
        );
        assert_eq!(changed_pixels, read(&mut renderer, &migrated));

        // The pinned Smithay applies place_above/below without a parent commit.
        // Exercise overlapping child buffers so stale ordering changes pixels.
        use crate::state::{dispatch_test_client, test_client_request as request};
        use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
        use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
        use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileExt;
        let client = info
            .window
            .toplevel()
            .unwrap()
            .wl_surface()
            .client()
            .unwrap();
        let compositor = client
            .create_resource::<WlCompositor, _, Ferese>(
                &state.display_handle,
                6,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let subcompositor = client
            .create_resource::<WlSubcompositor, _, Ferese>(
                &state.display_handle,
                1,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let shm = client
            .create_resource::<WlShm, _, Ferese>(
                &state.display_handle,
                1,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        for (surface, role, pool, buffer, color, position) in [
            (7, 8, 9, 10, 0xff00ff00u32, 4),
            (11, 12, 13, 14, 0xff0000ffu32, 8),
        ] {
            let file = tempfile::tempfile().unwrap();
            file.write_all_at(&color.to_ne_bytes().repeat(16 * 16), 0)
                .unwrap();
            request(
                &mut wire,
                compositor.id().protocol_id(),
                0,
                &[surface],
                None,
            );
            request(
                &mut wire,
                subcompositor.id().protocol_id(),
                1,
                &[role, surface, 2],
                None,
            );
            request(
                &mut wire,
                shm.id().protocol_id(),
                0,
                &[pool, 16 * 16 * 4],
                Some(file.as_raw_fd()),
            );
            request(&mut wire, pool, 0, &[buffer, 0, 16, 16, 16 * 4, 0], None);
            request(&mut wire, role, 1, &[position, position], None);
            request(&mut wire, surface, 1, &[buffer, 0, 0], None);
            request(&mut wire, surface, 6, &[], None);
        }
        request(&mut wire, 2, 6, &[], None);
        dispatch_test_client(&mut events, &mut state);
        let (before_reorder, before_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        let before_pixels = read(&mut renderer, &before_reorder);
        request(&mut wire, 8, 2, &[11], None);
        dispatch_test_client(&mut events, &mut state);
        let (after_reorder, after_revision, _) =
            stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        let after_pixels = read(&mut renderer, &after_reorder);
        assert_eq!(before_reorder.tex_id(), after_reorder.tex_id());
        assert_ne!(before_revision, after_revision);
        let overlap = (10 * 64 + 10) * 4;
        assert_ne!(
            &before_pixels[overlap..overlap + 4],
            &after_pixels[overlap..overlap + 4]
        );
        clear_stream_capture_targets(&mut renderer);
        let (fresh, _, _) = stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        assert_eq!(
            after_pixels,
            read(&mut renderer, &fresh),
            "incremental subsurface reorder must match fresh composition without a parent commit"
        );
    }

    #[test]
    #[ignore = "requires private Wayland sockets and an offscreen EGL device"]
    fn retained_transparent_crop_matches_legacy_material_and_excludes_outside_child() {
        if !crate::startup_tests::private_runtime(
            "render::capture::tests::retained_transparent_crop_matches_legacy_material_and_excludes_outside_child",
        ) {
            return;
        }
        use crate::state::{dispatch_test_client, test_client_request as request};
        use smithay::reexports::wayland_server::Resource;
        use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
        use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
        use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileExt;
        let mut renderer = super::super::corner_tests::renderer();
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = smithay::output::Output::new(
            "transparent-crop".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (1320, 990).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            Some(smithay::output::Scale::Fractional(1.65)),
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "transparent-crop".into());
        let (window, mut wire) = crate::state::test_window(&mut state, &mut events, 0);
        let client = window.toplevel().unwrap().wl_surface().client().unwrap();
        let compositor = client
            .create_resource::<WlCompositor, _, Ferese>(
                &state.display_handle,
                6,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let subcompositor = client
            .create_resource::<WlSubcompositor, _, Ferese>(
                &state.display_handle,
                1,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let shm = client
            .create_resource::<WlShm, _, Ferese>(
                &state.display_handle,
                1,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let file = tempfile::tempfile().unwrap();
        file.write_all_at(&0xff00ff00u32.to_ne_bytes().repeat(16 * 16), 0)
            .unwrap();
        request(&mut wire, compositor.id().protocol_id(), 0, &[7], None);
        request(
            &mut wire,
            subcompositor.id().protocol_id(),
            1,
            &[8, 7, 2],
            None,
        );
        request(
            &mut wire,
            shm.id().protocol_id(),
            0,
            &[9, 16 * 16 * 4],
            Some(file.as_raw_fd()),
        );
        request(&mut wire, 9, 0, &[10, 0, 16, 16, 16 * 4, 0], None);
        request(&mut wire, 8, 1, &[1280, 0], None);
        request(&mut wire, 7, 1, &[10, 0, 0], None);
        request(&mut wire, 7, 6, &[], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch_test_client(&mut events, &mut state);
        let size: Size<i32, Physical> = window.geometry().size.to_physical_precise_round(1.65);
        let info = crate::handlers::window_capture::WindowInfo {
            geometry: window.geometry(),
            window,
            output,
            scale: 1.65,
            bytes: size.w as usize * size.h as usize * 4,
        };
        let read = |renderer: &mut GlesRenderer, texture: &GlesTexture| {
            let mapping = renderer
                .copy_texture(
                    texture,
                    Rectangle::from_size(texture.size()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            renderer.map_texture(&mapping).unwrap().to_vec()
        };
        state.window_effects.enabled = true;
        state.window_effects.blur = true;
        state.theme_settings.backdrop_blur = 16.0;
        let alpha = state.window_effects.resolve(1.0, false, 0.0).0;
        let background = stream_window_background(&mut state, &mut renderer, &info, alpha);
        assert!(!background.is_empty());
        let legacy = render_window_texture(
            &mut renderer,
            &info.window,
            info.geometry,
            info.scale,
            128 * 1024 * 1024,
            None,
            &background,
            alpha,
        )
        .unwrap()
        .unwrap();
        let (retained, _, straight) =
            stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        assert!(!straight);
        let retained_pixels = read(&mut renderer, &retained);
        assert_eq!(
            retained_pixels,
            read(&mut renderer, &legacy.texture),
            "retained capture must preserve legacy material pixels for the identical cropped source"
        );
        assert!(
            retained_pixels
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [0, 0, 0]),
            "material backgrounds legitimately appear behind fully transparent clients"
        );
        state.window_effects.enabled = false;
        state.window_effects.blur = false;
        let (plain, _, straight) =
            stream_window_image(&mut renderer, &mut state, &info, None).unwrap();
        assert!(straight);
        assert!(
            read(&mut renderer, &plain).iter().all(|byte| *byte == 0),
            "a child entirely outside the selected crop must contribute no pixels"
        );
    }

    #[test]
    #[ignore = "manual release benchmark for window-capture pixel conversion"]
    fn profile_window_conversion() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        for (width, height) in [(1920usize, 1080usize), (3840, 2160)] {
            for workload in ["opaque", "edges", "translucent", "mixed"] {
                let mut source = vec![0u8; width * height * 4];
                let pixel_iter = source.chunks_exact_mut(4).enumerate();
                for (i, pixel) in pixel_iter {
                    let alpha: u8 = if workload == "mixed" {
                        i.wrapping_mul(37) as u8
                    } else if workload == "translucent"
                        || (workload == "edges"
                            && (i % width < 16
                                || i % width >= width - 16
                                || i / width < 16
                                || i / width >= height - 16))
                    {
                        128
                    } else {
                        255
                    };
                    let color = (i as u32).wrapping_mul(1664525).wrapping_add(1013904223);
                    let premultiplied =
                        |shift: u32| (((color >> shift) & 255) * u32::from(alpha) / 255) as u8;
                    pixel.copy_from_slice(&[
                        premultiplied(0),
                        premultiplied(8),
                        premultiplied(16),
                        alpha,
                    ]);
                }
                let mut pixels = source.clone();
                for run in 0..=3 {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..3 {
                        pixels.copy_from_slice(&source);
                        let started = Instant::now();
                        convert_window_pixels(black_box(&mut pixels));
                        elapsed += started.elapsed();
                        black_box(&pixels);
                    }
                    if run != 0 {
                        eprintln!(
                            "window_conversion_benchmark width={width} workload={workload} run={run} frames=3 elapsed_us={}",
                            elapsed.as_micros()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn conversion_matches_straight_argb_for_every_alpha_and_channel_value() {
        let mut pixels = Vec::new();
        let mut expected = Vec::new();
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                let source = [channel, 255 - channel, channel ^ alpha, alpha];
                pixels.extend_from_slice(&source);
                let straight = |value: u8| {
                    if alpha == 0 {
                        0
                    } else {
                        ((u32::from(value) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                            .min(255) as u8
                    }
                };
                expected.extend_from_slice(&[
                    straight(source[2]),
                    straight(source[1]),
                    straight(source[0]),
                    alpha,
                ]);
            }
        }
        // Incomplete trailing pixels are outside the conversion's input domain.
        pixels.extend_from_slice(&[17, 29, 43]);
        expected.extend_from_slice(&[17, 29, 43]);
        convert_window_pixels(&mut pixels);
        assert_eq!(pixels, expected);
    }

    #[test]
    fn readback_conversion_preserves_color_and_handles_zero_alpha() {
        let mut pixels = vec![10, 20, 30, 255, 32, 64, 128, 128, 9, 8, 7, 0, 1, 1, 1, 1];
        convert_window_pixels(&mut pixels);
        assert_eq!(
            pixels,
            [
                30, 20, 10, 255, 255, 128, 64, 128, 0, 0, 0, 0, 255, 255, 255, 1
            ]
        );
    }
}

#[derive(PartialEq)]
struct WindowCaptureConfiguration {
    output: smithay::output::Output,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    epoch: u64,
    effects: crate::config::WindowEffectsSettings,
    blur: f64,
    reduce_transparency: bool,
    wallpaper: CommitCounter,
}

struct WindowCaptureTarget {
    key: (ferese_layout::WindowId, bool),
    canvas: smithay::output::Output,
    image: Option<crate::backends::direct::capture::CaptureTarget>,
    configuration: Option<WindowCaptureConfiguration>,
    bytes: usize,
}

impl WindowCaptureTarget {
    fn new(
        key: (ferese_layout::WindowId, bool),
        info: &crate::handlers::window_capture::WindowInfo,
    ) -> Self {
        Self {
            key,
            canvas: smithay::output::Output::new(
                "window-stream-canvas".into(),
                info.output.physical_properties(),
            ),
            image: None,
            configuration: None,
            bytes: 0,
        }
    }
}

#[derive(Default)]
struct StreamResources {
    textures: std::collections::VecDeque<GlesTexture>,
    bytes: usize,
    windows: std::collections::VecDeque<WindowCaptureTarget>,
    window_bytes: usize,
    straight: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    copy: Option<smithay::backend::renderer::gles::GlesTexProgram>,
}

impl StreamResources {
    fn take_window(&mut self, key: (ferese_layout::WindowId, bool)) -> Option<WindowCaptureTarget> {
        let index = self.windows.iter().position(|window| window.key == key)?;
        let target = self.windows.remove(index)?;
        self.window_bytes -= target.bytes;
        Some(target)
    }

    fn put_window(&mut self, target: WindowCaptureTarget) {
        // Per-context source compositions are shared across matching consumers.
        // Keep their total storage bounded independently of transfer scratch targets.
        const MAX_BYTES: usize = 128 * 1024 * 1024;
        if target.bytes > MAX_BYTES {
            return;
        }
        while self.windows.len() >= 16 || self.window_bytes + target.bytes > MAX_BYTES {
            let Some(old) = self.windows.pop_front() else {
                break;
            };
            self.window_bytes -= old.bytes;
        }
        self.window_bytes += target.bytes;
        self.windows.push_back(target);
    }

    fn take_texture(&mut self, size: Size<i32, smithay::utils::Buffer>) -> Option<GlesTexture> {
        let index = self
            .textures
            .iter()
            .position(|texture| texture.size() == size)?;
        let texture = self.textures.remove(index)?;
        self.bytes -= texture.size().w as usize * texture.size().h as usize * 4;
        Some(texture)
    }

    fn put_texture(&mut self, texture: GlesTexture) {
        let bytes = texture.size().w as usize * texture.size().h as usize * 4;
        // Scratch targets are shared by size across sources. Keep alternating
        // window sizes from reallocating every frame without retaining an
        // unbounded collection of old source sizes.
        const MAX_BYTES: usize = 128 * 1024 * 1024;
        if bytes > MAX_BYTES {
            return;
        }
        while self.textures.len() >= 8 || self.bytes + bytes > MAX_BYTES {
            let Some(old) = self.textures.pop_front() else {
                break;
            };
            self.bytes -= old.size().w as usize * old.size().h as usize * 4;
        }
        self.bytes += bytes;
        self.textures.push_back(texture);
    }
}

#[derive(Default)]
struct StreamCache(std::sync::Mutex<StreamResources>);

const STREAM_CAPTURE_SHADER: &str = r#"
//_DEFINES
#ifdef EXTERNAL
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#ifdef EXTERNAL
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
varying vec2 v_coords;
uniform float alpha;
uniform float y_offset;
void main() {
    // The renderer negates Y for inverted textures; translate that origin
    // back into [0, 1] so full-buffer captures sample both rows correctly.
    vec4 pixel = texture2D(tex, v_coords + vec2(0.0, y_offset));
#ifdef NO_ALPHA
    pixel.a = 1.0;
#endif
#ifdef STRAIGHT
    float a = floor(pixel.a * 255.0 + 0.5);
    vec3 numerator = floor(pixel.rgb * 255.0 + 0.5) * 255.0 + floor(a * 0.5);
    vec3 quotient = floor(numerator / max(a, 1.0));
    vec3 remainder = numerator - quotient * max(a, 1.0);
    quotient += step(vec3(max(a, 1.0)), remainder);
    quotient -= vec3(lessThan(remainder, vec3(0.0)));
    gl_FragColor = vec4(a == 0.0 ? vec3(0.0) : clamp(quotient, 0.0, 255.0) / 255.0, 1.0);
#else
    gl_FragColor = vec4(pixel.rgb, 1.0);
#endif
}
"#;

pub(crate) enum StreamCopy {
    Gpu(smithay::backend::renderer::sync::SyncPoint),
    Shm {
        readback: crate::handlers::readback::Readback,
        size: Size<i32, smithay::utils::Buffer>,
    },
}

pub(crate) enum WindowStreamCopy {
    Unchanged,
    Copied { revision: u64, copy: StreamCopy },
}

pub(crate) fn capture_stream_window(
    renderer: &mut GlesRenderer,
    state: &mut Ferese,
    info: &crate::handlers::window_capture::WindowInfo,
    cursor: Option<Rectangle<i32, Logical>>,
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    previous_revision: Option<u64>,
) -> Result<WindowStreamCopy, String> {
    let (texture, revision, straight) = stream_window_image(renderer, state, info, cursor)?;
    if previous_revision == Some(revision) {
        return Ok(WindowStreamCopy::Unchanged);
    }
    // Destination pool buffers remain fully stale. Retaining source damage
    // only reduces composition; every returned buffer still receives all pixels.
    let copy = capture_stream_texture(
        renderer,
        &texture,
        buffer,
        Transform::Normal,
        straight,
        &state.readbacks,
    )?;
    Ok(WindowStreamCopy::Copied { revision, copy })
}

fn stream_window_image(
    renderer: &mut GlesRenderer,
    state: &mut Ferese,
    info: &crate::handlers::window_capture::WindowInfo,
    cursor: Option<Rectangle<i32, Logical>>,
) -> Result<(GlesTexture, u64, bool), String> {
    let cache = renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Arc::new(StreamCache::default()))
        .clone();
    let id = *state
        .windows
        .ids()
        .get(&info.window)
        .ok_or("Capture window was removed")?;
    let key = (id, cursor.is_some());
    let mut source = cache
        .0
        .lock()
        .expect("stream texture cache mutex poisoned")
        .take_window(key)
        .unwrap_or_else(|| WindowCaptureTarget::new(key, info));
    let size = info.geometry.size.to_physical_precise_round(info.scale);
    source.canvas.change_current_state(
        Some(smithay::output::Mode {
            size,
            refresh: info
                .output
                .current_mode()
                .map_or(60_000, |mode| mode.refresh),
        }),
        Some(Transform::Normal),
        Some(smithay::output::Scale::Fractional(info.scale)),
        None,
    );
    let configuration = WindowCaptureConfiguration {
        output: info.output.clone(),
        geometry: info.geometry,
        scale: info.scale,
        epoch: state.capture_epoch,
        effects: state.window_effects,
        blur: state.theme_settings.backdrop_blur,
        reduce_transparency: state.theme_settings.reduce_transparency,
        wallpaper: state.wallpaper.revision(),
    };
    if source.configuration.as_ref() != Some(&configuration) {
        if let Some(image) = &mut source.image {
            image.invalidate();
        }
        source.configuration = Some(configuration);
    }
    let result = (|| {
        let Some(toplevel) = info.window.toplevel() else {
            return Err("Window has no capturable content".into());
        };
        let alpha = state.window_effects.resolve(1.0, false, 0.0).0;
        let background = stream_window_background(state, renderer, info, alpha);
        let straight = background.is_empty();
        let mut elements = cursor.map_or_else(Vec::new, |cursor| {
            cursor_elements(state, renderer, cursor, info.scale)
        });
        let content = render_elements_from_surface_tree::<
            GlesRenderer,
            WaylandSurfaceRenderElement<GlesRenderer>,
        >(
            renderer,
            toplevel.wl_surface(),
            Point::<i32, Logical>::from((-info.geometry.loc.x, -info.geometry.loc.y))
                .to_physical_precise_round(info.scale),
            info.scale,
            alpha,
            RenderElementKind::Unspecified,
        );
        if content.is_empty() {
            return Err("Window has no capturable content".into());
        }
        elements.extend(
            content
                .into_iter()
                .map(super::AnimatedWindowRenderElement::from),
        );
        elements.extend(background);
        let clear = if straight {
            [0.0; 4]
        } else {
            [0.035, 0.04, 0.055, 1.0]
        };
        let (framebuffer, _) = crate::backends::direct::capture::render_capture_with_clear(
            renderer,
            &mut source.image,
            &source.canvas,
            &elements,
            clear,
        )
        .map_err(|error| error.to_string())?;
        drop(framebuffer);
        let (texture, revision) = source
            .image
            .as_ref()
            .expect("window composition was allocated")
            .image();
        source.bytes = info.bytes;
        Ok((texture, revision, straight))
    })();
    if let Some(image) = &mut source.image {
        let (texture, _) = image.image();
        source.bytes = texture.size().w as usize * texture.size().h as usize * 4;
        if result.is_err() {
            image.invalidate();
        }
    }
    cache
        .0
        .lock()
        .expect("stream texture cache mutex poisoned")
        .put_window(source);
    result
}

/// Window streams have no consent to sample other clients. Build their blur
/// from wallpaper only, using capture-owned resources and a window-sized canvas.
fn stream_window_background(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    info: &crate::handlers::window_capture::WindowInfo,
    alpha: f32,
) -> Vec<super::AnimatedWindowRenderElement> {
    use ferese_animation::{AnimatedRect, AnimatedValue};
    use smithay::output::{Mode, Output, Scale};

    let Some(&id) = state.windows.ids().get(&info.window) else {
        return Vec::new();
    };
    let Some(owner) = state.output_id(&info.output) else {
        return Vec::new();
    };
    let size = info.geometry.size.to_physical_precise_round(info.scale);
    let output = Output::new("window-capture".into(), info.output.physical_properties());
    output.change_current_state(
        Some(Mode {
            size,
            refresh: 60_000,
        }),
        Some(Transform::Normal),
        Some(Scale::Fractional(info.scale)),
        None,
    );
    output
        .user_data()
        .insert_if_missing(|| crate::handlers::workspace_capture::CaptureOutputId(owner));
    let bounds = ferese_layout::Rect {
        x: 0.0,
        y: 0.0,
        width: f64::from(info.geometry.size.w),
        height: f64::from(info.geometry.size.h),
    };
    let presentation = crate::presentation::WindowPresentation {
        id,
        bounds: AnimatedRect::new(bounds),
        opacity: AnimatedValue::new(1.0),
        window_alpha: alpha,
        dim: 0.0,
        emphasis: AnimatedValue::new(1.0),
        shadow: AnimatedValue::new(1.0),
        scale_content: false,
        native_size: None,
    };
    std::mem::swap(&mut state.render, &mut state.capture_render);
    let blur = super::window_blur::window_blur_element(
        state,
        renderer,
        &output,
        || super::window_blur::client_is_opaque(&info.window),
        presentation,
        super::RoundedRect::new(bounds, (0, 0).into(), info.scale, 0.0),
    );
    std::mem::swap(&mut state.render, &mut state.capture_render);
    let Some(blur) = blur else {
        return Vec::new();
    };
    state.prepare_theme_wallpaper(renderer, &info.output);
    let mut elements = vec![blur];
    if let Some(wallpaper) = state.wallpaper.element(renderer, &output) {
        elements.push(wallpaper.into());
    }
    super::backdrop::update(&elements, info.scale);
    elements
}

pub(crate) fn capture_stream_texture(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    transform: Transform,
    straight: bool,
    admission: &crate::handlers::readback::Readbacks,
) -> Result<StreamCopy, String> {
    use smithay::wayland::dmabuf::get_dmabuf;
    use smithay::wayland::shm::with_buffer_contents_mut;
    let cache = renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Arc::new(StreamCache::default()))
        .clone();
    let mut cache = cache.0.lock().expect("stream texture cache mutex poisoned");
    let program = if straight {
        &mut cache.straight
    } else {
        &mut cache.copy
    };
    if program.is_none() {
        *program =
            Some(compile_stream_program(renderer, straight).map_err(|error| error.to_string())?);
    }
    let program = program.as_ref().expect("stream shader compiled").clone();
    let source_size = texture.size();
    let size: Size<i32, Physical> = transform.transform_size((source_size.w, source_size.h).into());
    let draw = |renderer: &mut GlesRenderer,
                target: &mut smithay::backend::renderer::gles::GlesTarget<'_>| {
        draw_stream_texture(renderer, texture, target, transform, &program)
    };
    if let Ok(dmabuf) = get_dmabuf(buffer) {
        let mut dmabuf = dmabuf.clone();
        let mut target = renderer
            .bind(&mut dmabuf)
            .map_err(|error| error.to_string())?;
        return draw(renderer, &mut target).map(StreamCopy::Gpu);
    }
    let valid = with_buffer_contents_mut(buffer, |_, length, data| {
        data.width == size.w
            && data.height == size.h
            && data.format == smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888
            && data.stride >= size.w * 4
            && data.offset >= 0
            && usize::try_from(
                i64::from(data.offset) + i64::from(data.stride) * i64::from(data.height),
            )
            .is_ok_and(|required| required <= length)
    })
    .unwrap_or(false);
    if !valid {
        return Err("Invalid stream SHM buffer".into());
    }
    let row = usize::try_from(size.w)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or("Invalid stream dimensions")?;
    let bytes = usize::try_from(size.h)
        .ok()
        .and_then(|height| row.checked_mul(height))
        .ok_or("Invalid stream dimensions")?;
    admission.prepare(renderer, bytes)?;
    let mut texture = match cache.take_texture((size.w, size.h).into()) {
        Some(texture) => texture,
        None => Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            (size.w, size.h).into(),
        )
        .map_err(|error| error.to_string())?,
    };
    {
        let mut target = renderer
            .bind(&mut texture)
            .map_err(|error| error.to_string())?;
        let _ = draw(renderer, &mut target)?;
    }
    let mapping = renderer
        .copy_texture(
            &texture,
            Rectangle::from_size((size.w, size.h).into()),
            Fourcc::Argb8888,
        )
        .map_err(|error| error.to_string())?;
    let readback = crate::handlers::readback::Readback::try_new(renderer, mapping, bytes)?;
    cache.put_texture(texture);
    Ok(StreamCopy::Shm {
        readback,
        size: (size.w, size.h).into(),
    })
}

fn compile_stream_program(
    renderer: &mut GlesRenderer,
    straight: bool,
) -> Result<smithay::backend::renderer::gles::GlesTexProgram, GlesError> {
    use smithay::backend::renderer::gles::{UniformName, UniformType};
    let source = if straight {
        format!("#define STRAIGHT\n{STREAM_CAPTURE_SHADER}")
    } else {
        STREAM_CAPTURE_SHADER.to_string()
    };
    renderer
        .compile_custom_texture_shader(source, &[UniformName::new("y_offset", UniformType::_1f)])
}

fn draw_stream_texture(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    target: &mut smithay::backend::renderer::gles::GlesTarget<'_>,
    transform: Transform,
    program: &smithay::backend::renderer::gles::GlesTexProgram,
) -> Result<smithay::backend::renderer::sync::SyncPoint, String> {
    let source_size = texture.size();
    let size: Size<i32, Physical> = transform.transform_size((source_size.w, source_size.h).into());
    let mut frame = renderer
        .render(target, size, Transform::Normal)
        .map_err(|error| error.to_string())?;
    let rect = Rectangle::from_size(size);
    // Both stream shaders write alpha 1 over the full target. No clear is
    // needed; marking the entire destination opaque also disables blending.
    frame
        .render_texture_from_to(
            texture,
            Rectangle::from_size(source_size).to_f64(),
            rect,
            &[rect],
            &[rect],
            transform,
            1.0,
            Some(program),
            &[smithay::backend::renderer::gles::Uniform::new(
                "y_offset",
                if texture.is_y_inverted() {
                    1.0f32
                } else {
                    0.0f32
                },
            )],
        )
        .map_err(|error| error.to_string())?;
    frame.finish().map_err(|error| error.to_string())
}

pub(crate) fn clear_stream_capture_targets(renderer: &mut GlesRenderer) {
    if let Some(cache) = renderer.egl_context().user_data().get::<Arc<StreamCache>>() {
        let mut cache = cache.0.lock().expect("stream texture cache mutex poisoned");
        cache.textures.clear();
        cache.bytes = 0;
        cache.windows.clear();
        cache.window_bytes = 0;
    }
}

#[cfg(test)]
mod stream_gpu_tests {
    use super::*;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::ImportMem;

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn straight_capture_shader_matches_every_alpha_and_channel_value() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL rendering device");
        // SAFETY: the display retains the enumerated device for this test.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this thread exclusively owns the fresh context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let mut pixels = Vec::with_capacity(256 * 256 * 4);
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                pixels.extend_from_slice(&[
                    channel,
                    channel.wrapping_add(85),
                    channel.wrapping_add(170),
                    alpha,
                ]);
            }
        }
        let source = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (256, 256).into(), false)
            .unwrap();
        let program = compile_stream_program(&mut renderer, true).unwrap();
        let mut destination = Offscreen::<GlesTexture>::create_buffer(
            &mut renderer,
            Fourcc::Abgr8888,
            (256, 256).into(),
        )
        .unwrap();
        {
            let mut target = renderer.bind(&mut destination).unwrap();
            let _ = draw_stream_texture(
                &mut renderer,
                &source,
                &mut target,
                Transform::Normal,
                &program,
            )
            .unwrap();
        }
        let mapping = renderer
            .copy_texture(
                &destination,
                Rectangle::from_size((256, 256).into()),
                Fourcc::Argb8888,
            )
            .unwrap();
        let actual = renderer.map_texture(&mapping).unwrap();
        convert_window_pixels(&mut pixels);
        let actual_iter = actual
            .chunks_exact(4)
            .zip(pixels.chunks_exact(4))
            .enumerate();
        for (index, (actual, expected)) in actual_iter {
            assert_eq!(
                &actual[..3],
                &expected[..3],
                "alpha={} channel={}",
                index / 256,
                index % 256
            );
            assert_eq!(actual[3], 255, "BGRx capture must be opaque");
        }
    }
    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn stream_transfer_matches_output_orientation_for_all_transforms() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL rendering device");
        // SAFETY: the renderer retains the enumerated device's display.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this test exclusively owns the renderer's context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        use Transform::*;
        let cases = [
            (Normal, "abcdef"),
            (_90, "daebfc"),
            (_180, "fedcba"),
            (_270, "cfbead"),
            (Flipped, "cbafed"),
            (Flipped90, "adbecf"),
            (Flipped180, "defabc"),
            (Flipped270, "fcebda"),
        ];
        // Include transparent and translucent source pixels: the transfer must
        // replace every destination channel regardless of the source alpha.
        let pixels: Vec<_> = b"abcdef"
            .iter()
            .zip([0, 1, 64, 128, 254, 255])
            .flat_map(|(value, alpha)| [*value, value / 2, value / 3, alpha])
            .collect();
        for straight in [false, true] {
            let program = compile_stream_program(&mut renderer, straight).unwrap();
            let mut reference = pixels.clone();
            if straight {
                convert_window_pixels(&mut reference);
                for pixel in reference.chunks_exact_mut(4) {
                    pixel.swap(0, 2); // CPU conversion returns BGRA; read back RGBA.
                }
            }
            for pixel in reference.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            for flipped in [false, true] {
                let source = renderer
                    .import_memory(&pixels, Fourcc::Abgr8888, (3, 2).into(), flipped)
                    .unwrap();
                for (transform, order) in cases {
                    let size =
                        transform.transform_size(Size::<i32, smithay::utils::Buffer>::from((3, 2)));
                    let mut target = Offscreen::<GlesTexture>::create_buffer(
                        &mut renderer,
                        Fourcc::Abgr8888,
                        size,
                    )
                    .unwrap();
                    // Exercise reused targets containing unrelated pixels with
                    // both transparent and opaque destination alpha.
                    for alpha in [0.0, 1.0] {
                        {
                            let mut target = renderer.bind(&mut target).unwrap();
                            let mut frame = renderer
                                .render(&mut target, (size.w, size.h).into(), Transform::Normal)
                                .unwrap();
                            frame
                                .clear(
                                    Color32F::new(1.0, 0.0, 1.0, alpha),
                                    &[Rectangle::from_size((size.w, size.h).into())],
                                )
                                .unwrap();
                            let _ = frame.finish().unwrap();
                            let _ = draw_stream_texture(
                                &mut renderer,
                                &source,
                                &mut target,
                                transform,
                                &program,
                            )
                            .unwrap();
                        }
                        let mapping = renderer
                            .copy_texture(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
                            .unwrap();
                        let actual = renderer.map_texture(&mapping).unwrap();
                        // Reverse source rows before applying the output transform
                        // when the texture reports inverted storage.
                        let expected: Vec<_> = order
                            .bytes()
                            .flat_map(|value| {
                                let index = usize::from(value - b'a');
                                let index = if flipped { (index + 3) % 6 } else { index };
                                reference[index * 4..index * 4 + 4].iter().copied()
                            })
                            .collect();
                        assert_eq!(
                            actual, expected,
                            "{transform:?}, texture flipped={flipped}, straight={straight}, destination alpha={alpha}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn alternating_capture_sizes_reuse_their_gpu_allocations() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL rendering device");
        // SAFETY: the renderer retains the enumerated device's display.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this test exclusively owns the renderer's context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let mut cache = StreamResources::default();
        let sizes = [Size::from((64, 48)), Size::from((96, 64))];
        let mut allocations = [None, None];
        for _ in 0..100 {
            let size_iter = sizes.into_iter().enumerate();
            for (index, size) in size_iter {
                let texture = cache.take_texture(size).unwrap_or_else(|| {
                    Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, size)
                        .unwrap()
                });
                if let Some(previous) = allocations[index] {
                    assert_eq!(
                        texture.tex_id(),
                        previous,
                        "switching source sizes allocated another GPU target"
                    );
                } else {
                    allocations[index] = Some(texture.tex_id());
                }
                cache.put_texture(texture);
            }
        }
        assert_eq!(cache.textures.len(), 2);
        assert_eq!(cache.bytes, (64 * 48 + 96 * 64) * 4);
    }
}
