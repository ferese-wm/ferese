use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_protocols::xdg::toplevel_icon::v1::server::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
use smithay::wayland::GlobalData;
use smithay::wayland::shell::xdg::XdgWmBaseUserData;

use crate::Ferese;
use crate::state::{ClientState, dispatch_test_client, test_client_request as request};

// Replay the native JetBrains lifecycle: assign an icon, destroy the icon
// object, then free its SHM buffer. The next sync must still reach the client.
fn icon_cleanup_messages(destroy_icon: bool) -> Vec<(u32, u16, Vec<u32>)> {
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, GlobalData, Ferese>(&state.display_handle, 6, GlobalData)
        .unwrap();
    let shell = client
        .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(
            &state.display_handle,
            6,
            Default::default(),
        )
        .unwrap();
    let shm = client
        .create_resource::<WlShm, GlobalData, Ferese>(&state.display_handle, 1, GlobalData)
        .unwrap();
    let manager = client
        .create_resource::<XdgToplevelIconManagerV1, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[2], None);
    request(&mut wire, shell.id().protocol_id(), 2, &[3, 2], None);
    request(&mut wire, 3, 1, &[4], None);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&0xff112233u32.to_ne_bytes().repeat(32 * 32))
        .unwrap();
    request(
        &mut wire,
        shm.id().protocol_id(),
        0,
        &[5, 32 * 32 * 4],
        Some(file.as_raw_fd()),
    );
    request(&mut wire, 5, 0, &[6, 0, 32, 32, 128, 0], None);
    request(&mut wire, manager.id().protocol_id(), 1, &[7], None);
    request(&mut wire, 7, 2, &[6, 1], None);
    request(&mut wire, manager.id().protocol_id(), 2, &[4, 7], None);
    if destroy_icon {
        request(&mut wire, 7, 0, &[], None);
    }
    request(&mut wire, 6, 0, &[], None);
    request(&mut wire, 1, 0, &[8], None);
    dispatch_test_client(&mut events, &mut state);
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        let arguments = bytes[offset + 8..offset + size]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect();
        messages.push((object, header as u16, arguments));
        offset += size;
    }
    messages
}

#[test]
fn destroying_an_assigned_icon_before_its_buffer_keeps_the_client_connected() {
    if !crate::startup_tests::private_runtime(
        "handlers::toplevel_icon_tests::destroying_an_assigned_icon_before_its_buffer_keeps_the_client_connected",
    ) {
        return;
    }
    let messages = icon_cleanup_messages(true);
    assert!(
        !messages
            .iter()
            .any(|(object, opcode, _)| *object == 1 && *opcode == 0)
    );
    assert!(
        messages
            .iter()
            .any(|(object, opcode, _)| *object == 8 && *opcode == 0),
        "valid icon cleanup disconnected the client before its sync reply: {messages:?}"
    );
}

#[test]
fn destroying_a_buffer_while_its_icon_is_alive_still_reports_no_buffer() {
    if !crate::startup_tests::private_runtime(
        "handlers::toplevel_icon_tests::destroying_a_buffer_while_its_icon_is_alive_still_reports_no_buffer",
    ) {
        return;
    }
    let messages = icon_cleanup_messages(false);
    assert!(
        messages.iter().any(|(object, opcode, args)| {
            *object == 1 && *opcode == 0 && args.first() == Some(&7) && args.get(1) == Some(&3)
        }),
        "destroying a live icon's buffer must report the protocol error: {messages:?}"
    );
}
