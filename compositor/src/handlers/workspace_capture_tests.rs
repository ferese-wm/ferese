use super::super::workspace_capture::{WorkspaceCapture, WorkspaceSource};
use super::*;
use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};

fn fixture() -> (
    smithay::reexports::calloop::EventLoop<'static, Ferese>,
    Ferese,
    Output,
) {
    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.animations_enabled = false;
    config.wallpaper.path = None;
    let mut state = Ferese::new(
        &mut events,
        smithay::reexports::wayland_server::Display::new().unwrap(),
        config,
    )
    .unwrap();
    let output = Output::new(
        "workspace-test".into(),
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
            size: (1600, 1200).into(),
            refresh: 60_000,
        }),
        None,
        Some(Scale::Fractional(2.0)),
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "workspace-test".into());
    (events, state, output)
}

#[test]
fn workspace_capture_samples_hidden_layout_without_switching_or_configuring_clients() {
    if !crate::startup_tests::private_runtime(
        "handlers::image_capture::workspace_tests::workspace_capture_samples_hidden_layout_without_switching_or_configuring_clients",
    ) {
        return;
    }
    let (mut events, mut state, output) = fixture();
    let (window, _client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
    let id = state.windows.ids()[&window];
    let workspace = state.workspaces.workspace_for_window(id).unwrap();
    let source = WorkspaceSource::new(&state, workspace, false).unwrap();
    state.switch_workspace(2);
    let (private, _private_client) = crate::state::test_window(&mut state, &mut events, 0xff00ff00);
    let private_id = state.windows.ids()[&private];
    let focused = state.focused_window;
    let active = state.workspaces.active_id();
    let geometry = *state.windows.geometry(&id).unwrap();
    let sample = state.sample_workspace_frame(workspace, &output);
    assert!(sample.windows.contains_key(&id));
    assert!(!sample.windows.contains_key(&private_id));
    assert!(!sample.overview.is_presenting());
    assert!(sample.capture_tabs.is_some());
    assert_eq!(state.workspaces.active_id(), active);
    assert_eq!(state.focused_window, focused);
    assert_eq!(*state.windows.geometry(&id).unwrap(), geometry);
    assert!(state.space.element_location(&window).is_none());
    assert!(source.valid(&state));
    // The descriptor is never part of the desktop or advertised to clients.
    assert!(state.output_id(&source.render_output).is_none());
    assert!(state.space.output_geometry(&source.render_output).is_none());
    // A newly mapped hidden client need not have acquired a display sample.
    state.windows.record_mut(id).unwrap().geometry = None;
    assert!(
        state
            .sample_workspace_frame(workspace, &output)
            .windows
            .contains_key(&id)
    );
    assert!(state.windows.record(id).unwrap().geometry.is_none());
    output.change_current_state(None, None, Some(Scale::Fractional(1.5)), None);
    assert!(!source.valid(&state));
}

#[test]
fn workspace_commits_invalidate_only_subscribed_source_and_allocate_no_targets() {
    if !crate::startup_tests::private_runtime(
        "handlers::image_capture::workspace_tests::workspace_commits_invalidate_only_subscribed_source_and_allocate_no_targets",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (first, _first_client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
    let first_id = state.windows.ids()[&first];
    let workspace = state.workspaces.workspace_for_window(first_id).unwrap();
    let source = WorkspaceSource::new(&state, workspace, false).unwrap();
    state.switch_workspace(2);
    let (second, _second_client) = crate::state::test_window(&mut state, &mut events, 0xff00ff00);
    for cursor in [false, true] {
        state.image_capture.workspaces.insert(
            (workspace, cursor, false),
            WorkspaceCapture::new(source.clone(), state.capture_epoch),
        );
    }
    crate::backends::direct::render_surface(&mut state, second.toplevel().unwrap().wl_surface());
    for view in state.image_capture.workspaces.values() {
        assert_eq!(view.content, 0);
        assert!(view.target.is_none());
    }
    crate::backends::direct::render_surface(&mut state, first.toplevel().unwrap().wl_surface());
    for view in state.image_capture.workspaces.values() {
        assert_eq!(view.content, 1);
    }
    state
        .image_capture
        .workspace_callbacks
        .insert(workspace, Instant::now());
    state.refresh_capture_sessions();
    assert!(state.image_capture.workspaces.is_empty());
    assert!(state.image_capture.workspace_callbacks.is_empty());
    assert!(state.image_capture.callback_timer.is_none());
}

#[test]
fn capture_admission_accounts_for_the_pool_and_transfer_targets() {
    assert_eq!(stream_bytes((1920, 1080).into()), 8192 * 1080 * 10);
    assert!(stream_bytes((3840, 2160).into()) < MAX_STREAM_BYTES);
    assert!(stream_bytes((i32::MAX, i32::MAX).into()) > MAX_STREAM_BYTES);
}

#[test]
fn workspace_tab_metadata_invalidates_only_owning_capture() {
    if !crate::startup_tests::private_runtime(
        "handlers::image_capture::workspace_tests::workspace_tab_metadata_invalidates_only_owning_capture",
    ) {
        return;
    }
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::shell::xdg::{XdgShellHandler, XdgToplevelSurfaceData};

    for hidden in [false, true] {
        for title in [false, true] {
            let (mut events, mut state, output) = fixture();
            let (first, _first_client) =
                crate::state::test_window(&mut state, &mut events, 0xffff0000);
            state.toggle_column_tabbed();
            let (_second, _second_client) =
                crate::state::test_window(&mut state, &mut events, 0xff00ff00);
            state.consume_focused_window();
            let workspace = state.workspaces.active_id();
            let scene = state.sample_workspace_frame(workspace, &output);
            assert!(!scene.capture_tabs.as_ref().unwrap().is_empty());
            assert!(state.space.element_location(&first).is_none());

            state.switch_workspace(2);
            let (_other, _other_client) =
                crate::state::test_window(&mut state, &mut events, 0xff0000ff);
            let other_workspace = state.workspaces.active_id();
            if !hidden {
                state.switch_workspace(1);
            }
            for id in [workspace, other_workspace] {
                for cursor in [false, true] {
                    let mut view = WorkspaceCapture::new(
                        WorkspaceSource::new(&state, id, false).unwrap(),
                        state.capture_epoch,
                    );
                    view.prepared = Some(view.content);
                    view.scene = Some(state.sample_workspace_frame(id, &output));
                    view.scene_dirty = false;
                    state
                        .image_capture
                        .workspaces
                        .insert((id, cursor, false), view);
                }
            }
            let epoch = state.capture_epoch;
            let top = first.toplevel().unwrap();
            with_states(top.wl_surface(), |states| {
                let mut metadata = states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap();
                if title {
                    metadata.title = Some("Updated tab title".into());
                } else {
                    metadata.title = None;
                    metadata.app_id = Some("org.test.UpdatedApp".into());
                }
            });
            if title {
                state.title_changed(top.clone());
            } else {
                state.app_id_changed(top.clone());
            }
            assert_eq!(state.capture_epoch, epoch);
            for cursor in [false, true] {
                let view = &state.image_capture.workspaces[&(workspace, cursor, false)];
                assert_ne!(
                    view.prepared,
                    Some(view.content),
                    "tab metadata must invalidate its capture (hidden={hidden}, title={title})"
                );
                assert!(
                    !view.scene_dirty,
                    "metadata must not force a layout resample"
                );
                let other = &state.image_capture.workspaces[&(other_workspace, cursor, false)];
                assert_eq!(other.prepared, Some(other.content));
            }
        }
    }
}

#[test]
#[ignore = "requires private Wayland sockets and an offscreen EGL device"]
fn workspace_shell_pixels_follow_consent_and_visibility() {
    if !crate::startup_tests::private_runtime(
        "handlers::image_capture::workspace_tests::workspace_shell_pixels_follow_consent_and_visibility",
    ) {
        return;
    }
    use crate::state::{dispatch_test_client as dispatch, test_client_request as request};
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::gles::GlesRenderer;
    use smithay::backend::renderer::{Bind, ExportMem};
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1;
    use smithay::reexports::wayland_server::protocol::{
        wl_compositor::WlCompositor, wl_shm::WlShm,
    };
    use smithay::utils::Transform;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    let (mut events, mut state, output) = fixture();
    let (_window, _window_client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
    let workspace = state.workspaces.active_id();
    let (mut wire, server) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(crate::state::ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, _, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let layers = client
        .create_resource::<ZwlrLayerShellV1, _, Ferese>(
            &state.display_handle,
            4,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let shm = client
        .create_resource::<WlShm, _, Ferese>(&state.display_handle, 1, smithay::wayland::GlobalData)
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[2], None);
    // A 64x48 overlay at the top-left, with no exclusive zone.
    request(
        &mut wire,
        layers.id().protocol_id(),
        0,
        &[3, 2, 0, 3, 4, u32::from_ne_bytes(*b"tst\0")],
        None,
    );
    request(&mut wire, 3, 0, &[64, 48], None);
    request(&mut wire, 3, 1, &[1 | 4], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    wire.set_nonblocking(true).unwrap();
    let mut messages = Vec::new();
    let _ = wire.read_to_end(&mut messages);
    wire.set_nonblocking(false).unwrap();
    let mut offset = 0;
    let mut serial = None;
    while offset < messages.len() {
        let object = u32::from_ne_bytes(messages[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(messages[offset + 4..offset + 8].try_into().unwrap());
        if object == 3 && header & 0xffff == 0 {
            serial = Some(u32::from_ne_bytes(
                messages[offset + 8..offset + 12].try_into().unwrap(),
            ));
        }
        offset += (header >> 16) as usize;
    }
    request(&mut wire, 3, 6, &[serial.expect("layer configure")], None);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&0xffff00ffu32.to_ne_bytes().repeat(64 * 48))
        .unwrap();
    request(
        &mut wire,
        shm.id().protocol_id(),
        0,
        &[4, 64 * 48 * 4],
        Some(file.as_raw_fd()),
    );
    request(&mut wire, 4, 0, &[5, 0, 64, 48, 64 * 4, 0], None);
    request(&mut wire, 2, 1, &[5, 0, 0], None);
    request(&mut wire, 2, 2, &[0, 0, 64, 48], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);

    let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
    // SAFETY: The enumerated device is retained by the EGL display and context.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    let context = EGLContext::new(&display).unwrap();
    // SAFETY: This test exclusively owns the context and renderer.
    let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
    let has_shell =
        |view: &mut WorkspaceCapture, state: &mut Ferese, renderer: &mut GlesRenderer| {
            let (mut texture, _) = view.render(state, renderer, false).unwrap();
            let target = renderer.bind(&mut texture).unwrap();
            let size = (view.source.size.w, view.source.size.h).into();
            let mapping = renderer
                .copy_framebuffer(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
                .unwrap();
            renderer
                .map_texture(&mapping)
                .unwrap()
                .chunks_exact(4)
                .any(|pixel| pixel == [255, 0, 255, 255])
        };
    // Exercise the separate layer-map and render-output coordinate systems.
    for (scale, transform) in [(2.0, Transform::Normal), (1.25, Transform::_90)] {
        output.change_current_state(None, Some(transform), Some(Scale::Fractional(scale)), None);
        state.relayout();
        let mut plain = WorkspaceCapture::new(
            WorkspaceSource::new(&state, workspace, false).unwrap(),
            state.capture_epoch,
        );
        let mut shell = WorkspaceCapture::new(
            WorkspaceSource::new(&state, workspace, true).unwrap(),
            state.capture_epoch,
        );
        assert!(!plain.source.shell_visible(&state));
        assert!(shell.source.shell_visible(&state));
        assert!(!has_shell(&mut plain, &mut state, &mut renderer));
        assert!(
            has_shell(&mut shell, &mut state, &mut renderer),
            "missing shell at scale {scale}, transform {transform:?}, layers {:?}",
            smithay::desktop::layer_map_for_output(&output)
                .layers()
                .map(|layer| layer.bbox())
                .collect::<Vec<_>>()
        );
        assert!(
            !has_shell(&mut plain, &mut state, &mut renderer),
            "a concurrent plain capture must remain isolated"
        );
        state.toggle_overview();
        assert!(!shell.source.shell_visible(&state));
        assert!(!has_shell(&mut shell, &mut state, &mut renderer));
        state.toggle_overview();
        assert!(shell.source.shell_visible(&state));
        state.switch_workspace(2);
        assert!(!shell.source.shell_visible(&state));
        shell.invalidate();
        assert!(
            !has_shell(&mut shell, &mut state, &mut renderer),
            "switching away must erase retained shell pixels"
        );
        state.switch_workspace(1);
        shell.invalidate();
        assert!(has_shell(&mut shell, &mut state, &mut renderer));
        shell.release_idle_target();
        assert!(
            has_shell(&mut shell, &mut state, &mut renderer),
            "resume must rebuild the shell view"
        );
        if transform == Transform::_90 {
            // Unmapping the panel must erase its pixels without a window change.
            request(&mut wire, 2, 1, &[0, 0, 0], None);
            request(&mut wire, 2, 6, &[], None);
            dispatch(&mut events, &mut state);
            assert!(!has_shell(&mut shell, &mut state, &mut renderer));
        }
    }
}
