use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use calloop::EventLoop;
use smithay::reexports::wayland_server::Client;
use smithay::wayland::security_context::SecurityContext;

use crate::Ferese;
use crate::private_client::ClientCapabilities;
use crate::state::ClientState;

struct Registry {
    wire: UnixStream,
    client: Client,
    globals: HashMap<String, u32>,
}

fn request(wire: &mut UnixStream, object: u32, opcode: u32, args: &[u32]) {
    for word in [object, ((args.len() as u32 + 2) * 4) << 16 | opcode]
        .into_iter()
        .chain(args.iter().copied())
    {
        wire.write_all(&word.to_ne_bytes()).unwrap();
    }
}

fn dispatch(events: &mut EventLoop<'static, Ferese>, state: &mut Ferese) {
    events.dispatch(Duration::from_millis(5), state).unwrap();
    crate::after_dispatch(state);
}

impl Registry {
    fn new(events: &mut EventLoop<'static, Ferese>, state: &mut Ferese, data: ClientState) -> Self {
        let (server, mut wire) = UnixStream::pair().unwrap();
        let client = state
            .display_handle
            .insert_client(server, Arc::new(data))
            .unwrap();
        request(&mut wire, 1, 1, &[2]);
        dispatch(events, state);
        wire.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        let _ = wire.read_to_end(&mut bytes);
        wire.set_nonblocking(false).unwrap();
        wire.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut globals = HashMap::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let word = |index| {
                u32::from_ne_bytes(
                    bytes[offset + index..offset + index + 4]
                        .try_into()
                        .unwrap(),
                )
            };
            let header = word(4);
            if word(0) == 2 && header & 0xffff == 0 {
                let len = word(12) as usize;
                let name =
                    String::from_utf8(bytes[offset + 16..offset + 16 + len - 1].to_vec()).unwrap();
                globals.insert(name, word(8));
            }
            offset += (header >> 16) as usize;
        }
        Self {
            wire,
            client,
            globals,
        }
    }

    fn bind(
        &mut self,
        events: &mut EventLoop<'static, Ferese>,
        state: &mut Ferese,
        name: &str,
        global: u32,
        allowed: bool,
    ) {
        let mut bytes = name.as_bytes().to_vec();
        bytes.push(0);
        let len = bytes.len();
        bytes.resize(len.next_multiple_of(4), 0);
        let mut args = vec![global, len as u32];
        args.extend(
            bytes
                .chunks_exact(4)
                .map(|b| u32::from_ne_bytes(b.try_into().unwrap())),
        );
        args.extend([1, 3]);
        request(&mut self.wire, 2, 0, &args);
        request(&mut self.wire, 1, 0, &[4]);
        dispatch(events, state);
        loop {
            let mut header = [0; 8];
            self.wire.read_exact(&mut header).unwrap();
            let object = u32::from_ne_bytes(header[..4].try_into().unwrap());
            let header = u32::from_ne_bytes(header[4..].try_into().unwrap());
            let mut body = vec![0; (header >> 16) as usize - 8];
            self.wire.read_exact(&mut body).unwrap();
            // Some managers send their current state before the sync callback.
            if object == 1 || object == 4 {
                assert_eq!(
                    (object, header & 0xffff),
                    (if allowed { 4 } else { 1 }, 0),
                    "{name}"
                );
                break;
            }
            assert!(allowed, "forbidden bind emitted {name} state");
        }
    }
}

#[test]
fn privileged_globals_enforce_registry_admission() {
    if !crate::startup_tests::private_runtime_with_env(
        "handlers::security_context::tests::privileged_globals_enforce_registry_admission",
        &[
            ("FERESE_ENABLE_SCREENCOPY", "1"),
            ("FERESE_ENABLE_INPUT_METHOD", "1"),
        ],
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let host = Registry::new(&mut events, &mut state, ClientState::default());
    for name in [
        "zwlr_screencopy_manager_v1",
        "zwlr_layer_shell_v1",
        "zwlr_gamma_control_manager_v1",
        "zwp_input_method_manager_v2",
        "ext_session_lock_manager_v1",
        "wp_security_context_manager_v1",
        "zwlr_data_control_manager_v1",
        "ext_data_control_manager_v1",
        "ext_foreign_toplevel_list_v1",
        "ext_workspace_manager_v1",
        "zwlr_output_manager_v1",
    ] {
        let global = *host
            .globals
            .get(name)
            .unwrap_or_else(|| panic!("missing host global {name}"));
        let mut restricted = Registry::new(
            &mut events,
            &mut state,
            ClientState {
                security_context: Some(SecurityContext {
                    sandbox_engine: Some("test-sandbox".into()),
                    app_id: Some("test.restricted".into()),
                    instance_id: None,
                    creator_client_id: host.client.id(),
                }),
                ..Default::default()
            },
        );
        assert!(
            !restricted.globals.contains_key(name),
            "restricted discovery of {name}"
        );
        restricted.bind(&mut events, &mut state, name, global, false);
        {
            let mut ordinary = Registry::new(&mut events, &mut state, ClientState::default());
            ordinary.bind(&mut events, &mut state, name, global, true);
        }
    }
    for (name, required) in [
        ("ferese_effects_manager_v1", ClientCapabilities::EFFECTS),
        ("ferese_shell_manager_v1", ClientCapabilities::SHELL_CONTROL),
        (
            "ferese_window_capture_manager_v1",
            ClientCapabilities::WINDOW_CAPTURE,
        ),
        (
            "ext_image_copy_capture_manager_v1",
            ClientCapabilities::WINDOW_CAPTURE,
        ),
        (
            "ext_output_image_capture_source_manager_v1",
            ClientCapabilities::WINDOW_CAPTURE,
        ),
    ] {
        let owner = Registry::new(
            &mut events,
            &mut state,
            ClientState {
                capabilities: required,
                ..Default::default()
            },
        );
        let global = *owner
            .globals
            .get(name)
            .unwrap_or_else(|| panic!("missing capability global {name}"));
        for capability in [
            ClientCapabilities::default(),
            ClientCapabilities::EFFECTS,
            ClientCapabilities::SHELL_CONTROL,
            ClientCapabilities::WINDOW_CAPTURE,
        ] {
            let allowed = capability.contains(required);
            let mut peer = Registry::new(
                &mut events,
                &mut state,
                ClientState {
                    capabilities: capability,
                    ..Default::default()
                },
            );
            assert_eq!(peer.globals.contains_key(name), allowed, "{name}");
            peer.bind(&mut events, &mut state, name, global, allowed);
        }
    }
}
