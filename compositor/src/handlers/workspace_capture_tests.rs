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
    let source = WorkspaceSource::new(&state, workspace).unwrap();
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
    let source = WorkspaceSource::new(&state, workspace).unwrap();
    state.switch_workspace(2);
    let (second, _second_client) = crate::state::test_window(&mut state, &mut events, 0xff00ff00);
    for cursor in [false, true] {
        state.image_capture.workspaces.insert(
            (workspace, cursor),
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
                        WorkspaceSource::new(&state, id).unwrap(),
                        state.capture_epoch,
                    );
                    view.prepared = Some(view.content);
                    view.scene = Some(state.sample_workspace_frame(id, &output));
                    view.scene_dirty = false;
                    state.image_capture.workspaces.insert((id, cursor), view);
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
                let view = &state.image_capture.workspaces[&(workspace, cursor)];
                assert_ne!(
                    view.prepared,
                    Some(view.content),
                    "tab metadata must invalidate its capture (hidden={hidden}, title={title})"
                );
                assert!(
                    !view.scene_dirty,
                    "metadata must not force a layout resample"
                );
                let other = &state.image_capture.workspaces[&(other_workspace, cursor)];
                assert_eq!(other.prepared, Some(other.content));
            }
        }
    }
}
