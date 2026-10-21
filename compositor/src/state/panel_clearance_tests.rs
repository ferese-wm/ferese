use super::*;
use crate::state::ClientState;
use crate::state::DesktopOutput;
use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::utils::Transform;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

fn request(wire: &mut UnixStream, object: u32, opcode: u32, args: &[u32]) {
    let words = [object, (((args.len() + 2) * 4) as u32) << 16 | opcode];
    wire.write_all(
        &words
            .into_iter()
            .chain(args.iter().copied())
            .flat_map(u32::to_ne_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
}

#[test]
fn every_panel_style_owns_clearance_at_its_reserved_edge() {
    if !crate::startup_tests::private_runtime(
        "state::layout::panel_clearance_tests::every_panel_style_owns_clearance_at_its_reserved_edge",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "clearance".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    state.begin_desktop_transition();
    let changes = state
        .publish_desktop(vec![DesktopOutput {
            output: output.clone(),
            identity: "clearance".into(),
            mode: Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            },
            transform: Transform::Normal,
            scale: Scale::Fractional(1.0),
            position: (0, 0).into(),
        }])
        .unwrap();
    state.finish_desktop_transition(changes);
    let (mut wire, server) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let layers = client
        .create_resource::<ZwlrLayerShellV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            4,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[2]);
    let mut namespace = b"ferese-shell-top-bar\0".to_vec();
    let length = namespace.len() as u32;
    namespace.resize(namespace.len().div_ceil(4) * 4, 0);
    let args = [3, 2, 0, 2, length]
        .into_iter()
        .chain(
            namespace
                .chunks_exact(4)
                .map(|b| u32::from_ne_bytes(b.try_into().unwrap())),
        )
        .collect::<Vec<_>>();
    request(&mut wire, layers.id().protocol_id(), 0, &args);
    request(&mut wire, 3, 0, &[800, 36]);
    request(&mut wire, 3, 1, &[1 | 4 | 8]); // Top, left and right.
    for style in ferese_panel::PanelStyle::ALL {
        state.panel_style = style;
        for clearance in [0, 5, 24] {
            let mut panel = ferese_panel::Panel::from_preset(style);
            panel.geometry.height = Some(36.);
            panel.geometry.window_clearance = Some(clearance);
            panel.geometry.edge_margin = Some(8);
            let geometry = panel.resolved_geometry();
            request(&mut wire, 3, 2, &[panel.exclusive_zone(false) as u32]);
            request(&mut wire, 3, 3, &[geometry.edge_margin as u32, 0, 0, 0]);
            request(&mut wire, 2, 6, &[]);
            events
                .dispatch(Duration::from_millis(1), &mut state)
                .unwrap();
            let bounds = state.output_bounds_for(&output).unwrap();
            let edge = f64::from(geometry.edge_margin + 36 + clearance);
            assert_eq!(bounds.y, edge, "{style:?}, clearance={clearance}");
            for outer in [0., 12.] {
                let adjusted = state.panel_layout_bounds(bounds, outer);
                let window = maximized_rect(adjusted, outer);
                assert_eq!(
                    window.y, edge,
                    "{style:?}, clearance={clearance}, outer={outer}"
                );
                assert_eq!(window.x, bounds.x + outer);
                assert_eq!(window.y + window.height, bounds.y + bounds.height - outer);
            }
        }
    }
    request(&mut wire, 3, 2, &[0]);
    request(&mut wire, 2, 6, &[]);
    events
        .dispatch(Duration::from_millis(1), &mut state)
        .unwrap();
    let bounds = state.output_bounds_for(&output).unwrap();
    assert_eq!(
        state.panel_layout_bounds(bounds, 12.),
        bounds,
        "an unreserved panel must not remove gaps"
    );
}
