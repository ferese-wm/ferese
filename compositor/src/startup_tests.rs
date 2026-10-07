use std::io::{Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::signals::{Signal, Signals};
use smithay::input::SeatHandler;
use smithay::input::pointer::{CursorImageStatus, MotionEvent};
use smithay::reexports::wayland_server::protocol::{wl_compositor::WlCompositor, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{Display, Resource};
use smithay::utils::SERIAL_COUNTER;

use crate::{Ferese, RedrawRequest, after_dispatch, after_dispatch_with_redraw};

// Each state needs its own sockets and environment. A subprocess also bounds
// the cursor deadlock test without changing environment variables across tests.
pub(crate) fn private_runtime(test: &str) -> bool {
    const CHILD: &str = "FERESE_STARTUP_TEST_CHILD";
    if std::env::var_os(CHILD).is_some_and(|value| value == test) {
        return true;
    }

    let directory = tempfile::tempdir().unwrap();
    let log_path = directory.path().join("test.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--include-ignored", "--nocapture"])
        .env(CHILD, test)
        .env("XDG_RUNTIME_DIR", directory.path())
        .env("XDG_CONFIG_HOME", directory.path())
        .env("XDG_STATE_HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path())
        .env_remove("FERESE_SOCKET")
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }

        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{test} did not complete; possible startup deadlock");
        }

        std::thread::sleep(Duration::from_millis(10));
    };

    assert!(status.success(), "{}", std::fs::read_to_string(log_path).unwrap());
    false
}

pub(crate) fn state(events: &mut EventLoop<'static, Ferese>) -> Ferese {
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.wallpaper.path = None;

    Ferese::new(events, Display::new().unwrap(), config).unwrap()
}

#[test]
fn nested_instances_share_a_runtime_and_route_launched_clients_to_their_own_ipc() {
    use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
    if !private_runtime("startup_tests::nested_instances_share_a_runtime_and_route_launched_clients_to_their_own_ipc") {
        return;
    }
    let mut host_events = EventLoop::try_new().unwrap();
    let host = state(&mut host_events);
    let host_socket = host.session_environment.control_socket.clone();
    let mut first_events = EventLoop::try_new().unwrap();
    let mut second_events = EventLoop::try_new().unwrap();
    let config = || {
        let mut config = crate::config::Config::default().runtime_config().unwrap();
        config.wallpaper.path = None;
        config
    };
    let mut first = Ferese::new_with_session(
        &mut first_events,
        Display::new().unwrap(),
        config(),
        crate::ipc::Endpoint::instance().unwrap(),
        crate::SessionPolicy::Embedded,
    )
    .unwrap();
    let mut second = Ferese::new_with_session(
        &mut second_events,
        Display::new().unwrap(),
        config(),
        crate::ipc::Endpoint::instance().unwrap(),
        crate::SessionPolicy::Embedded,
    )
    .unwrap();
    let first_socket = first.session_environment.control_socket.clone();
    let second_socket = second.session_environment.control_socket.clone();
    assert_ne!(first_socket, host_socket);
    assert_ne!(second_socket, host_socket);
    assert_ne!(first_socket, second_socket);
    assert_ne!(first.socket_name, second.socket_name);
    let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    assert_eq!(host_socket, runtime.join("ferese/control.sock"));
    for (events, state, name) in [
        (&mut first_events, &mut first, "first"),
        (&mut second_events, &mut second, "second"),
    ] {
        let output = Output::new(
            name.into(),
            PhysicalProperties {
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
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, name.into());
        let report = runtime.join(format!("{name}.json"));
        let script = r#"import json, os, socket, struct, sys
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ['FERESE_SOCKET'])
s.settimeout(2)
payload = json.dumps({'version':1, 'id':1, 'type':'command', 'command':'outputs', 'args':{}}).encode()
s.sendall(struct.pack('>I', len(payload)) + payload)
def read_exact(n):
    data = b''
    while len(data) < n:
        part = s.recv(n - len(data))
        if not part: raise RuntimeError('IPC disconnected')
        data += part
    return data
response = json.loads(read_exact(struct.unpack('>I', read_exact(4))[0]))
with open(sys.argv[1], 'w') as report:
    json.dump({'socket':os.environ['FERESE_SOCKET'], 'response':response}, report)
"#;
        let mut child = crate::process::spawn_client(
            state,
            [
                std::ffi::OsStr::new("python3"),
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new(script),
                report.as_os_str(),
            ],
            crate::private_client::ClientCapabilities::default(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "nested IPC client timed out");
            events.dispatch(Duration::from_millis(10), state).unwrap();
        }
        let result: serde_json::Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(
            result["socket"],
            state.session_environment.control_socket.to_str().unwrap()
        );
        assert_eq!(result["response"]["result"][0]["name"], name);
        assert!(result["response"]["error"].is_null());
    }
    let first_directory = first_socket.parent().unwrap().to_owned();
    drop(first);
    assert!(!first_directory.exists());
    assert!(host_socket.symlink_metadata().unwrap().file_type().is_socket());
    assert!(second_socket.symlink_metadata().unwrap().file_type().is_socket());
    let second_directory = second_socket.parent().unwrap().to_owned();
    drop(second);
    assert!(!second_directory.exists());
    assert!(host_socket.symlink_metadata().unwrap().file_type().is_socket());
}

#[test]
fn launched_clients_unblock_termination_signals_and_keep_private_sockets() {
    if !private_runtime("startup_tests::launched_clients_unblock_termination_signals_and_keep_private_sockets") {
        return;
    }

    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM, Signal::SIGUSR1]).unwrap();
    let mut private = crate::private_client::ClientCapabilities::EFFECTS;
    private.insert(crate::private_client::ClientCapabilities::SHELL_CONTROL);
    for capabilities in [crate::private_client::ClientCapabilities::default(), private] {
        let mut child = crate::process::spawn_client(&mut state, ["/bin/sleep", "30"], capabilities).unwrap();
        let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
        let blocked = status.lines().find(|line| line.starts_with("SigBlk:")).unwrap();
        let blocked = u64::from_str_radix(blocked.split_whitespace().nth(1).unwrap(), 16).unwrap();
        assert_eq!(blocked & ((1 << (libc::SIGINT - 1)) | (1 << (libc::SIGTERM - 1))), 0);
        assert_ne!(blocked & (1 << (libc::SIGUSR1 - 1)), 0, "unrelated mask was lost");
        if !capabilities.is_empty() {
            let environment = std::fs::read(format!("/proc/{}/environ", child.id())).unwrap();
            for name in ["WAYLAND_SOCKET=", "FERESE_SHELL_CONTROL_SOCKET="] {
                let variable = environment
                    .split(|byte| *byte == 0)
                    .filter_map(|value| std::str::from_utf8(value).ok())
                    .find_map(|value| value.strip_prefix(name))
                    .unwrap();
                let fd = std::fs::read_link(format!("/proc/{}/fd/{variable}", child.id())).unwrap();
                assert!(fd.to_string_lossy().starts_with("socket:["));
            }
        }

        crate::process::terminate_child(&mut child);
        assert_eq!(
            child.wait().unwrap().signal(),
            Some(libc::SIGTERM),
            "child needed SIGKILL"
        );
    }

    // Child setup must not unblock the compositor's own signalfd mask.
    let mut mask = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut mask) },
        0
    );
    assert_eq!(unsafe { libc::sigismember(&mask, libc::SIGINT) }, 1);
    assert_eq!(unsafe { libc::sigismember(&mask, libc::SIGTERM) }, 1);
    drop(signals);
}

#[test]
fn explicit_client_exit_is_reaped_without_stopping_the_compositor() {
    if !private_runtime("startup_tests::explicit_client_exit_is_reaped_without_stopping_the_compositor") {
        return;
    }

    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let child = crate::process::spawn_client(
        &mut state,
        ["/bin/sh", "-c", "exit 7"],
        crate::private_client::ClientCapabilities::default(),
    )
    .unwrap();
    let pid = child.id();
    let watched = crate::process::watch_client_exit(&events.handle(), child);
    let deadline = Instant::now() + Duration::from_secs(2);
    while watched.borrow().is_some() {
        assert!(Instant::now() < deadline, "client was not reaped");
        events.dispatch(Duration::from_millis(10), &mut state).unwrap();
        after_dispatch(&mut state);
    }

    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) }, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    // Exit watching removes itself; subsequent event-loop dispatch remains usable.
    events.dispatch(Duration::ZERO, &mut state).unwrap();
}

#[test]
fn check_config_rejects_an_otherwise_valid_oversized_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.kdl");
    let valid = format!("//{}\n", "x".repeat(60 * 1024 - 3));
    std::fs::write(&path, &valid).unwrap();
    crate::check_config(&path).unwrap();
    assert!(crate::theme::prepare(&(valid.clone() + "\n"), directory.path()).is_ok());
    std::fs::write(&path, valid + "\n").unwrap();
    let error = crate::check_config(&path).unwrap_err();
    assert!(error.contains("exceeds 60 KiB"), "{error}");
    assert_eq!(crate::theme::read_source(&path).unwrap_err(), error);
}

#[test]
fn deferred_cursor_redraw_runs_after_input_lock_is_released() {
    if !private_runtime("startup_tests::deferred_cursor_redraw_runs_after_input_lock_is_released") {
        return;
    }

    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let (server, mut client_wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(crate::state::ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, (), Ferese>(&state.display_handle, 6, ())
        .unwrap();
    // wl_compositor.create_surface(new_id=2).
    for word in [compositor.id().protocol_id(), 12u32 << 16, 2] {
        client_wire.write_all(&word.to_ne_bytes()).unwrap();
    }

    events.dispatch(Duration::from_millis(10), &mut state).unwrap();
    let surface = client
        .object_from_protocol_id::<WlSurface>(&state.display_handle, 2)
        .unwrap();
    let pointer = state.seat.get_pointer().unwrap();
    let seat = state.seat.clone();
    state.cursor_image(&seat, CursorImageStatus::Hidden);
    after_dispatch_with_redraw(&mut state, |_, _| {});
    events.handle().insert_idle(move |state| {
        for _ in 0..2 {
            let event = MotionEvent {
                location: (10.0, 20.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: 0,
            };
            pointer.motion(state, Some((surface.clone(), (0.0, 0.0).into())), &event);
            // Smithay invokes Ferese::cursor_image while holding its real pointer lock.
            pointer.motion(state, None, &event);
            assert!(state.cursor_redraw_pending);
        }
    });
    let signal = events.get_signal();
    let mut redraws = 0;
    events
        .run(Some(Duration::ZERO), &mut state, |state| {
            after_dispatch_with_redraw(state, |state, request| {
                assert!(matches!(request, RedrawRequest::Cursor));
                assert_eq!(
                    state.seat.get_pointer().unwrap().current_location(),
                    (10.0, 20.0).into()
                );
                redraws += 1;
            });
            assert!(!state.cursor_redraw_pending);
            signal.stop();
        })
        .unwrap();
    assert_eq!(redraws, 1);
    after_dispatch_with_redraw(&mut state, |_, _| panic!("redraw was not consumed"));
}

#[test]
fn wayland_roundtrip_completes_without_a_rendered_frame() {
    if !private_runtime("startup_tests::wayland_roundtrip_completes_without_a_rendered_frame") {
        return;
    }

    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let (server, mut client) = UnixStream::pair().unwrap();
    state
        .display_handle
        .insert_client(server, Arc::new(crate::state::ClientState::default()))
        .unwrap();
    // wl_display.sync(new_id=2), using native-endian Wayland wire format.
    for word in [1u32, 12u32 << 16, 2u32] {
        client.write_all(&word.to_ne_bytes()).unwrap();
    }

    events.dispatch(Duration::from_millis(10), &mut state).unwrap();
    client.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let mut reply = [0u8; 12];
    assert!(
        client.read_exact(&mut reply).is_err(),
        "dispatch flushed replies before after_dispatch"
    );
    after_dispatch(&mut state);
    client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    client.read_exact(&mut reply).unwrap();
    assert_eq!(u32::from_ne_bytes(reply[0..4].try_into().unwrap()), 2);
    assert_eq!(u32::from_ne_bytes(reply[4..8].try_into().unwrap()) & 0xffff, 0);
}
