use super::*;
use crate::state::{ClientState, dispatch_test_client as dispatch, test_client_request as request};
use smithay::output::{Mode, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

fn fixture() -> (Ferese, EventLoop<'static, Ferese>) {
    let mut events = EventLoop::try_new().unwrap();
    let state = crate::startup_tests::state(&mut events);
    (state, events)
}

fn output(state: &mut Ferese, name: &str, x: i32) -> Output {
    let output = Output::new(
        name.into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (x, 0));
    state.register_output(&output, name.into());
    output
}

fn read(wire: &mut UnixStream) -> Vec<Vec<u32>> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    assert_eq!(
        wire.read_to_end(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    wire.set_nonblocking(false).unwrap();
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let len = (header >> 16) as usize;
        messages.push(
            bytes[offset..offset + len]
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
                .collect(),
        );
        offset += len;
    }
    messages
}

struct Client {
    client: smithay::reexports::wayland_server::Client,
    wire: UnixStream,
    compositor: WlCompositor,
    shell: ZwlrLayerShellV1,
}

impl Client {
    fn new(state: &mut Ferese) -> Self {
        let (server, mut wire) = UnixStream::pair().unwrap();
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
        let shell = client
            .create_resource::<ZwlrLayerShellV1, smithay::wayland::GlobalData, Ferese>(
                &state.display_handle,
                4,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        request(&mut wire, 1, 1, &[2], None); // Registry.
        request(&mut wire, compositor.id().protocol_id(), 0, &[3], None);
        // Keep client IDs contiguous: 5 is the optional wl_output, 6 the role.
        request(&mut wire, compositor.id().protocol_id(), 0, &[4], None);
        Self {
            client,
            wire,
            compositor,
            shell,
        }
    }

    fn bind_output(
        &mut self,
        state: &mut Ferese,
        events: &mut EventLoop<'static, Ferese>,
        output: &Output,
    ) -> smithay::reexports::wayland_server::backend::GlobalId {
        let global = output.create_global::<Ferese>(&state.display_handle);
        dispatch(events, state);
        let names = read(&mut self.wire)
            .into_iter()
            .filter(|message| {
                message[0] == 2
                    && message[1] & 0xffff == 0
                    && message[3] == 10
                    && message.get(4) == Some(&u32::from_ne_bytes(*b"wl_o"))
            })
            .map(|message| message[2])
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 1);
        request(
            &mut self.wire,
            2,
            0,
            &[
                names[0],
                10,
                u32::from_ne_bytes(*b"wl_o"),
                u32::from_ne_bytes(*b"utpu"),
                u32::from_ne_bytes([b't', 0, 0, 0]),
                4,
                5,
            ],
            None,
        );
        dispatch(events, state);
        read(&mut self.wire);
        global
    }

    fn create_layer(
        &mut self,
        state: &mut Ferese,
        events: &mut EventLoop<'static, Ferese>,
        output: u32,
    ) -> Vec<Vec<u32>> {
        if output == 0 {
            // Reserve the output slot with a spare surface for unspecified requests.
            request(
                &mut self.wire,
                self.compositor.id().protocol_id(),
                0,
                &[5],
                None,
            );
        }
        request(
            &mut self.wire,
            self.shell.id().protocol_id(),
            0,
            &[6, 3, output, 2, 4, u32::from_ne_bytes(*b"bar\0")],
            None,
        );
        request(&mut self.wire, 6, 0, &[64, 48], None);
        request(&mut self.wire, 3, 6, &[], None);
        dispatch(events, state);
        read(&mut self.wire)
    }

    fn mapped_on(&self, output: &Output) -> bool {
        layer_map_for_output(output).layers().any(|layer| {
            layer
                .wl_surface()
                .client()
                .is_some_and(|client| client.id() == self.client.id())
        })
    }
}

fn assert_closed_without_configure(messages: &[Vec<u32>]) {
    assert!(
        messages
            .iter()
            .any(|message| message[0] == 6 && message[1] & 0xffff == 1),
        "layer must receive closed"
    );
    assert!(
        !messages
            .iter()
            .any(|message| message[0] == 6 && message[1] & 0xffff == 0),
        "closed layers must not wait for configure"
    );
    assert!(
        !messages
            .iter()
            .any(|message| message[0] == 1 && message[1] & 0xffff == 0),
        "no client protocol error"
    );
}

#[test]
fn zero_outputs_close_new_layer_roles() {
    if !crate::startup_tests::private_runtime(
        "handlers::layer_shell::output_tests::zero_outputs_close_new_layer_roles",
    ) {
        return;
    }
    let (mut state, mut events) = fixture();
    let mut client = Client::new(&mut state);
    assert_closed_without_configure(&client.create_layer(&mut state, &mut events, 0));
}

#[test]
fn stale_explicit_output_closes_instead_of_retargeting() {
    if !crate::startup_tests::private_runtime(
        "handlers::layer_shell::output_tests::stale_explicit_output_closes_instead_of_retargeting",
    ) {
        return;
    }
    let (mut state, mut events) = fixture();
    let stale = output(&mut state, "stale", 0);
    let survivor = output(&mut state, "survivor", 800);
    let mut client = Client::new(&mut state);
    let global = client.bind_output(&mut state, &mut events, &stale);
    state.unregister_output(&stale);
    state.display_handle.remove_global::<Ferese>(global);
    dispatch(&mut events, &mut state);
    read(&mut client.wire);
    let messages = client.create_layer(&mut state, &mut events, 5);
    assert_closed_without_configure(&messages);
    assert!(!client.mapped_on(&stale));
    assert!(
        !client.mapped_on(&survivor),
        "explicit requests must not silently move to another output"
    );
}

#[test]
fn unspecified_output_uses_focused_registered_output() {
    if !crate::startup_tests::private_runtime(
        "handlers::layer_shell::output_tests::unspecified_output_uses_focused_registered_output",
    ) {
        return;
    }
    let (mut state, mut events) = fixture();
    let first = output(&mut state, "first", 0);
    let focused = output(&mut state, "focused", 800);
    state.focus_output_at((820., 20.).into());
    assert_eq!(state.focused_output(), Some(&focused));
    let mut client = Client::new(&mut state);
    let messages = client.create_layer(&mut state, &mut events, 0);
    assert!(client.mapped_on(&focused));
    assert!(!client.mapped_on(&first));
    assert!(
        messages
            .iter()
            .any(|message| message[0] == 6 && message[1] & 0xffff == 0),
        "valid layer receives configure"
    );
}

#[test]
fn valid_explicit_output_is_preserved() {
    if !crate::startup_tests::private_runtime(
        "handlers::layer_shell::output_tests::valid_explicit_output_is_preserved",
    ) {
        return;
    }
    let (mut state, mut events) = fixture();
    let explicit = output(&mut state, "explicit", 0);
    let focused = output(&mut state, "focused", 800);
    state.focus_output_at((820., 20.).into());
    let mut client = Client::new(&mut state);
    client.bind_output(&mut state, &mut events, &explicit);
    let messages = client.create_layer(&mut state, &mut events, 5);
    assert!(client.mapped_on(&explicit));
    assert!(!client.mapped_on(&focused));
    assert!(
        messages
            .iter()
            .any(|message| message[0] == 6 && message[1] & 0xffff == 0)
    );
}

#[test]
fn unregistered_output_is_not_a_creation_fallback() {
    if !crate::startup_tests::private_runtime(
        "handlers::layer_shell::output_tests::unregistered_output_is_not_a_creation_fallback",
    ) {
        return;
    }
    let (mut state, mut events) = fixture();
    let ghost = Output::new(
        "ghost".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    ghost.change_current_state(
        Some(Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&ghost, (0, 0));
    let mut client = Client::new(&mut state);
    assert_closed_without_configure(&client.create_layer(&mut state, &mut events, 0));
    assert!(!client.mapped_on(&ghost));
}
