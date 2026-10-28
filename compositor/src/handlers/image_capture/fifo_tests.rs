use super::*;
use crate::presentation_constraints::protocol_tests::{
    bind, damage_commit, fixture, messages, pump,
};
use crate::state::{dispatch_test_client as dispatch, test_client_request as request};
use smithay::backend::renderer::element::{
    Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::GlobalData;
use std::os::unix::net::UnixStream;

struct Waiting {
    state: Ferese,
    events: calloop::EventLoop<'static, Ferese>,
    output: Output,
    root: WlSurface,
    child: WlSurface,
    cursor: WlSurface,
    other: WlSurface,
    _root_wire: UnixStream,
    _other_wire: UnixStream,
}

fn waiting() -> Waiting {
    let (mut state, mut events, root, mut root_wire) = fixture();
    let (window, mut other_wire) =
        crate::state::test_window_with_app_id(&mut state, &mut events, 0xff445566, "capture.other");
    let other = window.toplevel().unwrap().wl_surface().clone();
    request(&mut other_wire, 1, 1, &[7], None);
    dispatch(&mut events, &mut state);
    let globals = messages(&mut other_wire);
    bind(&mut other_wire, &globals, "wp_fifo_manager_v1", 8);
    request(&mut other_wire, 8, 1, &[9, 2], None);
    let client = root.client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, GlobalData, Ferese>(&state.display_handle, 6, GlobalData)
        .unwrap();
    let subcompositor = client
        .create_resource::<WlSubcompositor, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap();
    request(
        &mut root_wire,
        compositor.id().protocol_id(),
        0,
        &[12],
        None,
    );
    request(
        &mut root_wire,
        subcompositor.id().protocol_id(),
        1,
        &[13, 12, 2],
        None,
    );
    request(&mut root_wire, 13, 1, &[100, 100], None);
    request(&mut root_wire, 13, 5, &[], None);
    request(&mut root_wire, 10, 1, &[14, 12], None);
    request(
        &mut root_wire,
        compositor.id().protocol_id(),
        0,
        &[15],
        None,
    );
    request(&mut root_wire, 10, 1, &[16, 15], None);
    for (fifo, surface) in [(11, 2), (14, 12), (16, 15)] {
        request(&mut root_wire, fifo, 0, &[], None);
        surface_commit(&mut root_wire, surface);
    }
    request(&mut root_wire, 2, 6, &[], None);
    request(&mut other_wire, 9, 0, &[], None);
    damage_commit(&mut other_wire);
    dispatch(&mut events, &mut state);
    let child = client
        .object_from_protocol_id::<WlSurface>(&state.display_handle, 12)
        .unwrap();
    let cursor = client
        .object_from_protocol_id::<WlSurface>(&state.display_handle, 15)
        .unwrap();
    state.cursor_status = smithay::input::pointer::CursorImageStatus::Surface(cursor.clone());
    let output = state.space.outputs().next().unwrap().clone();
    let mut displayed = RenderElementStates::default();
    for surface in [&root, &child, &cursor, &other] {
        displayed.states.insert(
            Id::from(surface),
            RenderElementState {
                needs_capture: false,
                visible_area: 1,
                presentation_state: RenderElementPresentationState::ZeroCopy,
            },
        );
    }
    state.display_presentation.queued(&output, &displayed);
    state.display_presentation.presented(&output);
    let before = [&root, &child, &cursor, &other].map(commit);
    for (fifo, surface) in [(11, 2), (14, 12), (16, 15)] {
        request(&mut root_wire, fifo, 1, &[], None);
        surface_commit(&mut root_wire, surface);
    }
    request(&mut other_wire, 9, 1, &[], None);
    damage_commit(&mut other_wire);
    dispatch(&mut events, &mut state);
    pump(&mut events, &mut state, Duration::from_millis(25));
    assert_eq!(
        [&root, &child, &cursor, &other].map(commit),
        before,
        "ordinary commits must preserve the FIFO barrier until observed progress"
    );
    Waiting {
        state,
        events,
        output,
        root,
        child,
        cursor,
        other,
        _root_wire: root_wire,
        _other_wire: other_wire,
    }
}

fn surface_commit(wire: &mut UnixStream, surface: u32) {
    request(wire, surface, 1, &[6, 0, 0], None);
    request(wire, surface, 2, &[0, 0, 1, 1], None);
    request(wire, surface, 6, &[], None);
}

fn commit(surface: &WlSurface) -> CommitCounter {
    with_renderer_surface_state(surface, |data| data.current_commit()).unwrap()
}

#[test]
fn captured_window_fifo_scope_excludes_cropped_children_and_other_clients() {
    if !crate::startup_tests::private_runtime(
        "handlers::image_capture::fifo_tests::captured_window_fifo_scope_excludes_cropped_children_and_other_clients",
    ) {
        return;
    }
    let mut fixture = waiting();
    let before = [
        commit(&fixture.root),
        commit(&fixture.child),
        commit(&fixture.cursor),
        commit(&fixture.other),
    ];
    let crop = Rectangle::from_size((64, 48).into());
    let mut stamp = WindowStamp {
        output: fixture.output.clone(),
        topology: 0,
        surfaces: Arc::new(surface_stamp(&fixture.root, Some(crop))),
        geometry: crop,
        scale: 1.0,
        epoch: fixture.state.capture_epoch,
        appearance: WindowAppearance::new(&fixture.state),
        cursor: Some(CursorStamp {
            location: (100.0, 100.0).into(),
            geometry: crop,
            image: CursorImageStamp::Surface(surface_stamp(&fixture.cursor, None), (0, 0).into()),
        }),
    };
    assert!(
        stamp
            .surfaces
            .iter()
            .any(|surface| surface.id == fixture.root.id())
    );
    assert!(
        !stamp
            .surfaces
            .iter()
            .any(|surface| surface.id == fixture.child.id())
    );
    release_captured_window_fifo(&mut fixture.state, &stamp);
    fixture.state.poll_presentation_constraints();
    assert_ne!(commit(&fixture.root), before[0]);
    assert_eq!(commit(&fixture.child), before[1]);
    assert_eq!(commit(&fixture.cursor), before[2]);
    assert_eq!(commit(&fixture.other), before[3]);
    stamp.cursor.as_mut().unwrap().location = (2.0, 2.0).into();
    release_captured_window_fifo(&mut fixture.state, &stamp);
    fixture.state.poll_presentation_constraints();
    assert_ne!(commit(&fixture.cursor), before[2]);
    assert_eq!(commit(&fixture.child), before[1]);
    assert_eq!(commit(&fixture.other), before[3]);
    pump(
        &mut fixture.events,
        &mut fixture.state,
        Duration::from_millis(5),
    );
}

#[test]
fn captured_workspace_fifo_respects_send_deadline_target_visibility_and_privacy() {
    if !crate::startup_tests::private_runtime(
        "handlers::image_capture::fifo_tests::captured_workspace_fifo_respects_send_deadline_target_visibility_and_privacy",
    ) {
        return;
    }
    let mut fixture = waiting();
    let before = [
        commit(&fixture.root),
        commit(&fixture.child),
        commit(&fixture.cursor),
        commit(&fixture.other),
    ];
    let windows = [
        fixture.state.windows.id_for_surface(&fixture.root).unwrap(),
        fixture
            .state
            .windows
            .id_for_surface(&fixture.other)
            .unwrap(),
    ];
    fixture.state.window_rules = crate::config::Config::parse_source(
        "window-rule app-id=\"capture.other\" block-out-from-screencasts=#true\n",
    )
    .unwrap()
    .window_rules()
    .unwrap();
    let root = fixture.root.clone();
    let other = fixture.other.clone();
    let visible = |surface: &WlSurface| surface == &root || surface == &other;
    assert!(
        super::super::workspace_capture::callbacks_for_windows(
            &mut fixture.state,
            &fixture.output,
            windows,
            visible,
            false,
        ),
        "FIFO-only capture clients must rearm the pacing deadline even without frame callbacks"
    );
    fixture.state.poll_presentation_constraints();
    assert_eq!(commit(&fixture.root), before[0]);
    super::super::workspace_capture::callbacks_for_windows(
        &mut fixture.state,
        &fixture.output,
        windows,
        visible,
        true,
    );
    fixture.state.poll_presentation_constraints();
    assert_ne!(commit(&fixture.root), before[0]);
    assert_eq!(commit(&fixture.child), before[1]);
    assert_eq!(commit(&fixture.cursor), before[2]);
    assert_eq!(commit(&fixture.other), before[3]);
    assert!(
        fixture
            .state
            .capture_protected(fixture.state.windows.window(windows[1]).unwrap())
    );
}
