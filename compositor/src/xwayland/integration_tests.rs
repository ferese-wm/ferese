//! Child and notification helper tests; these do not drive the manager event loop.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::auth::AuthorityFile;
use super::child;
use super::readiness::ReadinessSocket;
use super::sockets::Reservation;
use crate::session_environment::{SessionEnvironment, X11Environment};

struct Fixture {
    directory: PathBuf,
    record: PathBuf,
    wrapper: PathBuf,
    reservation: Reservation,
    authority: AuthorityFile,
    runtime_directory: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with_allocator(name, false)
    }

    /// Production display-slot inputs: an X client resolves `DISPLAY` against `/tmp/.X11-unix`.
    fn new_live(name: &str) -> Self {
        Self::with_allocator(name, true)
    }

    fn with_allocator(name: &str, production_inputs: bool) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "ferese-x11-it-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the fixture directory");
        let runtime_directory = directory.join("run");
        fs::create_dir_all(&runtime_directory).expect("create the runtime directory");
        fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o700))
            .expect("make the runtime directory private");

        if production_inputs {
            crate::xwayland::test_hooks::allocator_root_in(PathBuf::from("/tmp/.X11-unix"), PathBuf::from("/tmp"));
        } else {
            // Owned inputs, so this fixture never allocates a real slot in `/tmp/.X11-unix`.
            let allocator = directory.join("x11");
            fs::create_dir_all(&allocator).expect("create the allocator directory");
            fs::set_permissions(&allocator, fs::Permissions::from_mode(0o700)).expect("make the allocator private");
            crate::xwayland::test_hooks::allocator_root(allocator);
        }

        let reservation = Reservation::allocate().expect("reserve a display");
        let authority =
            AuthorityFile::create(&runtime_directory, reservation.display_number()).expect("create an authority file");

        Self {
            record: directory.join("starts.jsonl"),
            wrapper: directory.join("fake-satellite"),
            reservation,
            authority,
            runtime_directory,
            directory,
        }
    }

    fn script(&self, assignments: &[(&str, &str)]) -> &Self {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/tests/fake-xwayland-satellite.py");
        assert!(fake.exists(), "the fake Satellite must exist at {}", fake.display());
        let record = self.record.display();
        let fake = fake.display();

        let mut body = String::from("#!/bin/sh\n");
        body.push_str(&format!(
            "FAKE_SATELLITE_RECORD='{record}'\nexport FAKE_SATELLITE_RECORD\n"
        ));
        for (key, value) in assignments {
            body.push_str(&format!("{key}='{value}'\nexport {key}\n"));
        }
        body.push_str(&format!("exec '{fake}' \"$@\"\n"));
        fs::write(&self.wrapper, body).expect("write the wrapper script");
        fs::set_permissions(&self.wrapper, fs::Permissions::from_mode(0o755)).expect("make the wrapper executable");
        self
    }

    fn environment(&self) -> SessionEnvironment {
        SessionEnvironment {
            wayland_display: std::ffi::OsString::from("wayland-it"),
            x11: Some(X11Environment {
                display: std::ffi::OsString::from(self.reservation.display_name()),
                authority: self.authority.path().to_owned(),
            }),
        }
    }

    fn starts(&self) -> Vec<serde_json::Value> {
        match fs::read_to_string(&self.record) {
            Ok(contents) => contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("each record must be valid JSON"))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("cannot read {}: {error}", self.record.display()),
        }
    }

    fn spawn(&self, generation: u64) -> (Child, ReadinessSocket) {
        let socket = ReadinessSocket::create(&self.runtime_directory, generation).expect("create a readiness socket");
        let notify = socket.path().to_owned();
        let spawned = child::spawn_satellite(
            &self.wrapper,
            &self.environment(),
            &self.reservation,
            self.authority.path(),
            Some(notify.as_path()),
        )
        .expect("spawn the fake Satellite");
        (spawned, socket)
    }

    fn spawn_real(&self, generation: u64, executable: &Path) -> (Child, ReadinessSocket) {
        let socket = ReadinessSocket::create(&self.runtime_directory, generation).expect("create a readiness socket");
        let notify = socket.path().to_owned();
        let mut environment = self.environment();

        environment.wayland_display = std::env::var_os("WAYLAND_DISPLAY").expect("a live Wayland session is required");
        let spawned = child::spawn_satellite(
            executable,
            &environment,
            &self.reservation,
            self.authority.path(),
            Some(notify.as_path()),
        )
        .expect("spawn the real Satellite");
        (spawned, socket)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Run `test_path` in a dedicated process and report whether this is that process.
///
/// Process-wide checks must not share a process with parallel tests that fork
/// children: those inherit every descriptor of this process, close-on-exec or
/// not, and would read as unrelated holders.
fn run_isolated(test_path: &str) -> bool {
    const CHILD: &str = "FERESE_XWAYLAND_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let directory = std::env::temp_dir().join(format!(
        "ferese-isolated-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("create the isolated session directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("make the session private");

    let status = Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", test_path, "--nocapture"])
        .env(CHILD, "1")
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .env_remove("FERESE_SOCKET")
        .output()
        .expect("re-exec the test");
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    false
}

fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn read_ready(socket: &ReadinessSocket, pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match socket.recv_ready(pid) {
            Ok(true) => return true,
            Ok(false) => return false,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("readiness read failed: {error}"),
        }
    }
    false
}

fn reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[test]
fn satellite_receives_exactly_the_owned_listeners_and_no_extra_descriptors() {
    let sentinel = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a sentinel");
    let sentinel_fd = sentinel.as_raw_fd();

    let fixture = Fixture::new("listeners");
    let sentinel_target = std::fs::read_link(format!("/proc/self/fd/{sentinel_fd}"))
        .expect("the sentinel descriptor is open")
        .to_string_lossy()
        .into_owned();
    fixture.script(&[
        ("FAKE_SATELLITE_READY", "0"),
        ("FAKE_SATELLITE_SENTINEL_FD", &sentinel_fd.to_string()),
        ("FAKE_SATELLITE_SENTINEL_TARGET", &sentinel_target),
    ]);
    let (satellite, _socket) = fixture.spawn(1);
    assert!(
        wait_until(|| !fixture.starts().is_empty()),
        "the fake must record a start"
    );
    let record = fixture.starts().remove(0);

    assert_eq!(record["display"], fixture.reservation.display_name());
    assert_eq!(record["authority"], fixture.authority.path().to_str().unwrap());

    let listenfds = record["listenfds"].as_array().expect("listenfds array");
    assert_eq!(listenfds.len(), fixture.reservation.listeners().len());
    assert_eq!(
        record["all_listening"], true,
        "every passed fd must be a listening socket"
    );
    assert_eq!(
        record["sentinel_leaked"], false,
        "the ordinary spawn must prove the parent's own descriptor never leaked"
    );
    reap(satellite);

    // Positive control: with the flag cleared in the forked child, the same descriptor is visible.
    let flags = unsafe { libc::fcntl(sentinel_fd, libc::F_GETFD) };
    assert_ne!(flags, -1);
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "the sentinel must start close-on-exec, otherwise this test proves nothing"
    );

    let control = Fixture::new("listeners-control");
    control.script(&[
        ("FAKE_SATELLITE_READY", "0"),
        ("FAKE_SATELLITE_SENTINEL_FD", &sentinel_fd.to_string()),
        ("FAKE_SATELLITE_SENTINEL_TARGET", &sentinel_target),
    ]);
    child::test_hook::set_in_child(move || {
        if unsafe { libc::fcntl(sentinel_fd, libc::F_SETFD, 0) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    });
    let (satellite, _socket) = control.spawn(1);
    assert!(
        wait_until(|| !control.starts().is_empty()),
        "the control must record a start"
    );
    let control_record = control.starts().remove(0);
    assert_eq!(
        control_record["sentinel_leaked"], true,
        "the control proves the descriptor check can detect an inherited fd"
    );
    reap(satellite);

    let flags = unsafe { libc::fcntl(sentinel_fd, libc::F_GETFD) };
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "the parent's descriptor flags must be unchanged after the control"
    );
}

#[test]
fn satellite_gets_the_session_environment_and_never_a_private_socket() {
    let fixture = Fixture::new("environment");
    fixture.script(&[("FAKE_SATELLITE_READY", "0")]);
    let (satellite, _socket) = fixture.spawn(1);
    assert!(wait_until(|| !fixture.starts().is_empty()));
    let record = fixture.starts().remove(0);
    let environment = &record["env"];

    assert_eq!(environment["DISPLAY"], fixture.reservation.display_name());
    assert_eq!(environment["XAUTHORITY"], fixture.authority.path().to_str().unwrap());
    assert_eq!(environment["WAYLAND_DISPLAY"], "wayland-it");

    assert_eq!(environment["WAYLAND_SOCKET"], serde_json::Value::Null);
    assert_eq!(environment["FERESE_SHELL_CONTROL_SOCKET"], serde_json::Value::Null);

    reap(satellite);
}

#[test]
fn a_valid_notification_from_the_service_pid_satisfies_readiness() {
    let fixture = Fixture::new("ready");
    fixture.script(&[("FAKE_SATELLITE_READY", "1")]);
    let (satellite, socket) = fixture.spawn(1);
    let pid = satellite.id();
    assert!(
        read_ready(&socket, pid),
        "a READY=1 from the service PID must be accepted"
    );
    reap(satellite);
}

#[test]
fn a_delayed_notification_is_still_accepted() {
    let fixture = Fixture::new("ready-delay");
    fixture.script(&[("FAKE_SATELLITE_READY", "1"), ("FAKE_SATELLITE_READY_DELAY", "0.3")]);
    let (satellite, socket) = fixture.spawn(1);
    let pid = satellite.id();
    assert!(read_ready(&socket, pid), "a delayed READY=1 must be accepted");
    reap(satellite);
}

#[test]
fn a_notification_from_a_child_pid_is_rejected() {
    let fixture = Fixture::new("ready-child");
    fixture.script(&[("FAKE_SATELLITE_FORK_NOTIFY", "1")]);
    let (satellite, socket) = fixture.spawn(1);
    let pid = satellite.id();

    assert!(!read_ready(&socket, pid), "a foreign PID's READY=1 must be rejected");
    reap(satellite);
}

#[test]
fn malformed_and_truncated_notifications_are_rejected() {
    for payload in ["garbage", "truncated"] {
        let fixture = Fixture::new(&format!("ready-{payload}"));
        fixture.script(&[("FAKE_SATELLITE_READY", payload)]);
        let (satellite, socket) = fixture.spawn(1);
        let pid = satellite.id();
        assert!(!read_ready(&socket, pid), "{payload} must not satisfy readiness");
        reap(satellite);
    }
}

#[test]
fn a_service_that_exits_before_readiness_leaves_no_running_process() {
    let fixture = Fixture::new("exit-early");
    fixture.script(&[
        ("FAKE_SATELLITE_EXIT_BEFORE_READY", "1"),
        ("FAKE_SATELLITE_EXIT_CODE", "17"),
    ]);
    let (mut satellite, socket) = fixture.spawn(1);
    let pid = satellite.id();
    assert!(!read_ready(&socket, pid), "an early exit must not satisfy readiness");
    let status = wait_until_child(&mut satellite);
    assert_eq!(status.code(), Some(17));
    assert!(
        wait_until(|| !process_exists(pid)),
        "the early-exiting service must be gone"
    );
}

fn wait_until_child(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll the child") {
            return status;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the child did not exit in time");
}

#[test]
fn shutdown_cleans_the_whole_owned_process_group() {
    const PATH: &str = "xwayland::integration_tests::shutdown_cleans_the_whole_owned_process_group";
    if !run_isolated(PATH) {
        return;
    }

    let fixture = Fixture::new("group");

    fixture.script(&[
        ("FAKE_SATELLITE_HOLD_FDS", "1"),
        ("FAKE_SATELLITE_READY", "1"),
        ("FAKE_SATELLITE_LINGER", "30"),
    ]);
    let (satellite, socket) = fixture.spawn(1);
    let pid = satellite.id();
    assert!(read_ready(&socket, pid), "the group fixture must reach readiness first");

    let group_children = process::group_members(pid);
    assert!(
        group_children.len() >= 2,
        "the fake must have a descendant holding the fds"
    );

    // Ownership, not liveness: the group must hold the endpoints it was handed.
    let inodes = listener_inodes(&fixture.reservation);
    assert!(!inodes.is_empty(), "the reserved listeners must be identifiable");
    let holders_before = process::socket_holders(&inodes);
    assert!(
        holders_before.iter().any(|holder| *holder != std::process::id()),
        "a service descendant must hold the X11 listeners before shutdown: {holders_before:?}"
    );

    let mut group = child::OwnedProcessGroup::adopt(satellite);
    let status = group
        .terminate(Duration::from_secs(5))
        .expect("the owned process group must terminate");
    assert!(status.is_some(), "terminate() must reap the service leader");

    assert!(
        wait_until(|| group_children.iter().all(|member| !process_exists(*member))),
        "every member of the owned process group must be gone: {group_children:?}"
    );
    assert!(group.group_is_empty(), "the group must report itself empty");

    let holders_after = process::socket_holders(&inodes);
    assert_eq!(
        holders_after,
        vec![std::process::id()],
        "no service process may still hold the listeners after shutdown: {holders_after:?}"
    );
}

/// Socket inodes behind the listeners this process reserved.
fn listener_inodes(reservation: &Reservation) -> Vec<String> {
    reservation
        .listeners()
        .iter()
        .filter_map(|listener| {
            let target = std::fs::read_link(format!("/proc/self/fd/{}", listener.as_raw_fd())).ok()?;
            let text = target.to_string_lossy().into_owned();
            text.strip_prefix("socket:[")
                .and_then(|rest| rest.strip_suffix(']'))
                .map(str::to_owned)
        })
        .collect()
}

mod process {
    /// PIDs that currently hold any of `inodes`.
    pub(super) fn socket_holders(inodes: &[String]) -> Vec<u32> {
        let mut holders = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return holders;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(pid): Result<u32, _> = name.parse() else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                let text = target.to_string_lossy();
                let Some(number) = text.strip_prefix("socket:[").and_then(|rest| rest.strip_suffix(']')) else {
                    continue;
                };
                if inodes.iter().any(|inode| inode == number) {
                    holders.push(pid);
                    break;
                }
            }
        }
        holders.sort_unstable();
        holders.dedup();
        holders
    }

    pub(super) fn group_members(leader: u32) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };

        entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name();
                let pid: u32 = name.to_str()?.parse().ok()?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

                let closing = stat.rfind(')')?;
                let fields: Vec<&str> = stat[closing + 1..].split_whitespace().collect();
                let pgrp: u32 = fields.get(2)?.parse().ok()?;
                (pgrp == leader).then_some(pid)
            })
            .collect()
    }
}

#[test]
#[ignore = "requires a real Satellite, Xwayland, and a live Wayland session"]
fn real_satellite_reports_verified_readiness_and_serves_authorized_x_clients() {
    let executable = live_dependencies();

    let fixture = Fixture::new_live("real");
    let (mut satellite, socket) = fixture.spawn_real(1, &executable);
    let pid = satellite.id();

    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    let mut accepted = false;
    while Instant::now() < deadline {
        match socket.recv_ready(pid) {
            Ok(true) => {
                accepted = true;
                break;
            }
            Ok(false) => panic!("the real Satellite sent a notification that was rejected"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if satellite.try_wait().expect("poll Satellite").is_some() {
                    panic!("the real Satellite exited before reporting readiness");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("readiness read failed: {error}"),
        }
    }
    let elapsed = started.elapsed();
    assert!(accepted, "the installed Satellite never sent a usable READY=1");
    println!("real Satellite reported readiness in {elapsed:?} (production budget 10s)");
    assert!(
        elapsed <= Duration::from_secs(10),
        "readiness took {elapsed:?}, which exceeds the production startup budget"
    );

    let output = Command::new("xdpyinfo")
        .arg("-display")
        .arg(fixture.reservation.display_name())
        .env("XAUTHORITY", fixture.authority.path())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                text.contains("X.Org"),
                "an authorized X client connected, but this is not an X.Org server:\n{text}"
            );
        }
        Ok(output) => panic!(
            "an authorized X11 client could not use the managed display: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ),
        Err(error) => panic!("xdpyinfo is unavailable, so the endpoint is unverified: {error}"),
    }

    let mut group = child::OwnedProcessGroup::adopt(satellite);
    let status = group
        .terminate(Duration::from_secs(10))
        .expect("the real service group must terminate");
    assert!(status.is_some(), "terminate() must reap the real Satellite");
    assert!(
        group.group_is_empty(),
        "the real service group must be empty after shutdown"
    );
    assert!(wait_until(|| !process_exists(pid)), "the real Satellite must be gone");
}

/// Everything the live smoke test cannot supply for itself, declared up front.
fn live_dependencies() -> PathBuf {
    let Some(executable) = ["xwayland-satellite", "/usr/bin/xwayland-satellite"]
        .into_iter()
        .find_map(|candidate| {
            let path = PathBuf::from(candidate);
            path.is_file().then_some(path)
        })
    else {
        panic!("dependency missing: xwayland-satellite is not installed");
    };

    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        panic!("dependency missing: a live Wayland session (WAYLAND_DISPLAY is unset)");
    }

    let on_path = |program: &str| {
        std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|directory| directory.join(program).is_file()))
    };
    if !on_path("xdpyinfo") {
        panic!("dependency missing: xdpyinfo (x11-utils) is required to prove an X client can connect");
    }

    match fs::symlink_metadata("/tmp/.X11-unix") {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => panic!("dependency missing: /tmp/.X11-unix exists but is not a directory"),
        Err(error) => panic!("dependency missing: the shared X11 socket directory /tmp/.X11-unix: {error}"),
    }

    executable
}

mod manager_loop {
    use super::*;
    use crate::Ferese;
    use crate::config::{XwaylandConfig, XwaylandStartup};
    use crate::xwayland;
    use crate::xwayland::test_hooks;
    use ferese_ipc::xwayland::{Readiness, State};
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;

    fn wrapper(directory: &Path, assignments: &[(&str, &str)]) -> PathBuf {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/tests/fake-xwayland-satellite.py");
        let script = directory.join("fake-satellite");
        let mut body = String::from("#!/bin/sh\n");
        for (key, value) in assignments {
            body.push_str(&format!("{key}='{value}'\nexport {key}\n"));
        }
        body.push_str(&format!("exec '{}' \"$@\"\n", fake.display()));
        fs::write(&script, body).expect("write the wrapper");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make the wrapper executable");
        script
    }

    fn x11_socket_path(directory: &Path, display: &str) -> PathBuf {
        directory
            .join("x11")
            .join(format!("X{}", display.trim_start_matches(':')))
    }

    /// A `Ferese` wired to a real event loop, ready to run the manager.
    fn session(directory: &Path, path: PathBuf) -> (EventLoop<'static, Ferese>, Ferese) {
        let (_, runtime, _) = crate::theme::prepare("xwayland { }", directory).expect("prepare configuration");
        // Owned inputs, so this test never competes for a real display slot.
        let allocator = directory.join("x11");
        fs::create_dir_all(&allocator).expect("create the allocator directory");
        fs::set_permissions(&allocator, fs::Permissions::from_mode(0o700)).expect("make the allocator private");
        test_hooks::allocator_root(allocator);
        let mut event_loop = EventLoop::try_new().expect("create the event loop");
        let display = Display::new().expect("create the Wayland display");
        let mut state = Ferese::new(&mut event_loop, display, runtime).expect("create the session");
        state.xwayland_config = XwaylandConfig {
            enabled: true,
            startup: XwaylandStartup::OnDemand,
            path,
        };
        (event_loop, state)
    }

    fn dispatch_until(
        event_loop: &mut EventLoop<'static, Ferese>,
        state: &mut Ferese,
        mut condition: impl FnMut(&Ferese) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), state)
                .expect("dispatch the event loop");
            if condition(state) {
                return;
            }
        }
        panic!(
            "the condition was never met; last status: {:?}",
            xwayland::status_snapshot(state)
        );
    }

    #[test]
    fn the_manager_reaches_running_and_verifies_readiness_through_the_loop() {
        if !run_isolated(
            "xwayland::integration_tests::manager_loop::the_manager_reaches_running_and_verifies_readiness_through_the_loop",
        ) {
            return;
        }
        let directory = std::env::temp_dir().join(format!("ferese-manager-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(&directory, wrapper(&directory, &[("FAKE_SATELLITE_LINGER", "60")]));
        xwayland::initialize(&mut state);

        let generation = xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");
        assert!(generation > 0);

        dispatch_until(&mut event_loop, &mut state, |state| {
            xwayland::status_snapshot(state).state == State::Running
        });

        let status = xwayland::status_snapshot(&state);
        assert_eq!(
            status.readiness,
            Readiness::Verified,
            "readiness is earned, not assumed"
        );
        assert_eq!(status.state, State::Running);
        assert!(!status.restart_required, "the live service matches the configured one");
        let pid = status.satellite_pid.expect("a running service reports its pid");
        assert!(process_exists(pid), "the service must be alive");

        xwayland::shutdown_after_loop(&mut state);
        assert!(!process_exists(pid), "shutdown must stop and reap the service");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_crashed_service_is_stopped_before_anything_can_start_again() {
        if !run_isolated(
            "xwayland::integration_tests::manager_loop::a_crashed_service_is_stopped_before_anything_can_start_again",
        ) {
            return;
        }
        let directory = std::env::temp_dir().join(format!("ferese-manager-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(
            &directory,
            wrapper(
                &directory,
                &[("FAKE_SATELLITE_EXIT_AFTER_READY", "1"), ("FAKE_SATELLITE_LINGER", "0")],
            ),
        );
        xwayland::initialize(&mut state);
        xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        // While a service process is alive, no dispatch may make it startable again.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
            let status = xwayland::status_snapshot(&state);
            if let Some(pid) = status.satellite_pid
                && process_exists(pid)
            {
                assert!(
                    xwayland::request_start(&mut state).is_none(),
                    "a live service must never be startable again; status: {status:?}"
                );
            }
            // `Stopped` also covers an in-flight stop, where the process is still alive.
            if matches!(status.state, State::Backoff | State::Failed | State::Idle) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the crashed service never reached a stopped phase; status: {status:?}"
            );
        }

        let status = xwayland::status_snapshot(&state);
        assert!(
            matches!(status.state, State::Backoff | State::Stopped | State::Failed),
            "a crash must land in a stopped phase, not {:?}",
            status.state
        );
        if let Some(pid) = status.satellite_pid {
            assert!(
                !process_exists(pid),
                "the crashed service must be reaped before publishing"
            );
        } else {
            // The owner was cleared by cleanup, which is also proof it ran.
            assert!(
                xwayland::request_start(&mut state).is_none(),
                "a fresh start must wait for the retry window"
            );
        }
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_readiness_timeout_stops_the_live_service_before_anything_starts_again() {
        const PATH: &str = "xwayland::integration_tests::manager_loop::a_readiness_timeout_stops_the_live_service_before_anything_starts_again";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();
        test_hooks::STARTUP_DEADLINE_MS.store(600, std::sync::atomic::Ordering::SeqCst);

        let directory = std::env::temp_dir().join(format!("ferese-manager-timeout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");
        let record = directory.join("starts.jsonl");
        let record = record.display().to_string();

        let (mut event_loop, mut state) = session(
            &directory,
            wrapper(
                &directory,
                &[
                    ("FAKE_SATELLITE_RECORD", record.as_str()),
                    // Never reports readiness; a descendant keeps the listeners open.
                    ("FAKE_SATELLITE_READY", "0"),
                    ("FAKE_SATELLITE_HOLD_FDS", "1"),
                    ("FAKE_SATELLITE_LINGER", "60"),
                ],
            ),
        );
        xwayland::initialize(&mut state);
        let first_generation =
            xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        let display = xwayland::status_snapshot(&state)
            .display
            .expect("a display is reserved");
        let socket_path = x11_socket_path(&directory, &display);
        // The kernel queues the connection, so a re-enabled listener could claim it.
        std::os::unix::net::UnixStream::connect(&socket_path).expect("the reserved socket accepts a queued connection");

        let mut first_pid = None;
        let mut observed_pids = Vec::new();
        let mut highest_generation = first_generation;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
            let status = xwayland::status_snapshot(&state);
            highest_generation = highest_generation.max(status.generation.unwrap_or(0));
            if let Some(pid) = status.satellite_pid {
                if first_pid.is_none() {
                    first_pid = Some(pid);
                }
                observed_pids.push(pid);
                assert!(
                    xwayland::request_start(&mut state).is_none(),
                    "a live service must never be startable again; status: {status:?}"
                );
            }
            if status.state == State::Failed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the timeout never opened the circuit; last status: {status:?}"
            );
        }

        let status = xwayland::status_snapshot(&state);
        assert_eq!(status.state, State::Failed, "three silent starts must open the circuit");
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("reported no readiness")),
            "the timeout must be reported as a broken readiness contract, got {status:?}"
        );

        let first_pid = first_pid.expect("a service was spawned");
        assert!(
            wait_until(|| !process_exists(first_pid)),
            "the timed-out service leader must be reaped, pid {first_pid} still exists"
        );
        assert!(
            observed_pids.iter().all(|pid| wait_until(|| !process_exists(*pid))),
            "every generation started by the queued connection must be reaped"
        );

        let holder = fs::read_to_string(&record)
            .expect("the fake recorded its start")
            .lines()
            .next()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid JSON")["holder_pid"].clone())
            .and_then(|value| value.as_u64())
            .map(|pid| pid as u32)
            .expect("the fake reports the descriptor holder");
        assert!(
            wait_until(|| !process_exists(holder)),
            "the descendant holding the listeners must be killed, pid {holder} still exists"
        );

        assert!(
            !socket_path.exists(),
            "the socket path must be released on terminal failure"
        );
        assert!(
            xwayland::request_start(&mut state).is_none(),
            "an open circuit must not accept a start request"
        );
        assert!(
            highest_generation >= 2,
            "the queued connection must have driven retries after cleanup, saw generation {highest_generation}"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_unavailable_pidfd_falls_back_to_the_bounded_exit_poll() {
        const PATH: &str =
            "xwayland::integration_tests::manager_loop::an_unavailable_pidfd_falls_back_to_the_bounded_exit_poll";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();
        test_hooks::PIDFD_UNAVAILABLE.store(true, std::sync::atomic::Ordering::SeqCst);

        let directory = std::env::temp_dir().join(format!("ferese-manager-poll-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(
            &directory,
            wrapper(
                &directory,
                &[
                    ("FAKE_SATELLITE_EXIT_AFTER_READY", "1"),
                    ("FAKE_SATELLITE_LINGER", "60"),
                ],
            ),
        );
        xwayland::initialize(&mut state);
        xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        let mut pid = None;
        let mut verified_readiness = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
            let status = xwayland::status_snapshot(&state);
            if let Some(observed) = status.satellite_pid {
                pid = Some(observed);
            }
            if status.readiness == Readiness::Verified {
                verified_readiness = true;
            }
            if pid.is_some() && status.state == State::Backoff {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the fallback watcher never noticed the exit; last status: {status:?}"
            );
        }

        let pid = pid.expect("a service was spawned");
        assert!(
            wait_until(|| !process_exists(pid)),
            "the observed service must be reaped without a pidfd"
        );
        assert!(verified_readiness, "the service reported readiness before it exited");
        let status = xwayland::status_snapshot(&state);
        assert_eq!(status.satellite_pid, None, "cleanup released the process owner");
        test_hooks::reset();
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_stale_exit_callback_cannot_stop_a_replacement_service() {
        const PATH: &str =
            "xwayland::integration_tests::manager_loop::a_stale_exit_callback_cannot_stop_a_replacement_service";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();

        let directory = std::env::temp_dir().join(format!("ferese-manager-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(
            &directory,
            wrapper(&directory, &[("FAKE_SATELLITE_EXIT_BEFORE_READY", "1")]),
        );
        xwayland::initialize(&mut state);
        let first_generation =
            xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        let mut retired_pid = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
            let status = xwayland::status_snapshot(&state);
            if let Some(pid) = status.satellite_pid {
                retired_pid = Some(pid);
            }
            if status.state == State::Failed {
                break;
            }
            assert!(Instant::now() < deadline, "the early failure never settled: {status:?}");
        }
        let retired_pid = retired_pid.expect("the first generation was spawned");
        let retired_status = xwayland::status_snapshot(&state);
        assert!(
            !retired_status.state.is_inactive(),
            "an early failure must leave the circuit open: {retired_status:?}"
        );

        wrapper(&directory, &[("FAKE_SATELLITE_LINGER", "60")]);
        xwayland::retry(&mut state).expect("an explicit retry rebuilds an unavailable service");
        dispatch_until(&mut event_loop, &mut state, |state| {
            xwayland::status_snapshot(state).state == State::Running
        });
        let live = xwayland::status_snapshot(&state);
        let live_pid = live.satellite_pid.expect("the replacement reports its pid");
        let live_generation = live.generation.expect("the replacement reports its generation");
        assert_ne!(live_pid, retired_pid, "the replacement must be a different process");

        let handle = state.loop_handle.clone();
        let stale_generation = first_generation;
        {
            let manager = state.xwayland.as_mut().expect("the service exists");
            assert!(
                !manager.owns(stale_generation, retired_pid),
                "the retired process is no longer owned"
            );
            manager.handle_exit(&handle, stale_generation, retired_pid);
            manager.fail(&handle, stale_generation, "stale failure", true);
        }

        let after = xwayland::status_snapshot(&state);
        assert_eq!(
            after.state,
            State::Running,
            "a stale callback must not move the live service"
        );
        assert_eq!(after.satellite_pid, Some(live_pid), "the live process must stay owned");
        assert_eq!(
            after.generation,
            Some(live_generation),
            "the live generation must survive"
        );
        assert!(
            process_exists(live_pid),
            "the live service must not be signalled by a stale callback"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn shutdown_while_starting_stops_the_service_it_owns() {
        const PATH: &str =
            "xwayland::integration_tests::manager_loop::shutdown_while_starting_stops_the_service_it_owns";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();

        let directory = std::env::temp_dir().join(format!("ferese-manager-shutdown-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");
        let (mut event_loop, mut state) =
            session(&directory, wrapper(&directory, &[("FAKE_SATELLITE_READY_DELAY", "30")]));
        xwayland::initialize(&mut state);
        xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        for _ in 0..8 {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
        }
        let status = xwayland::status_snapshot(&state);
        let display = status.display.clone().expect("a display is reserved");
        let pid = status.satellite_pid.expect("the service was spawned");
        assert_eq!(
            status.state,
            State::Starting,
            "shutdown must happen mid-start: {status:?}"
        );

        xwayland::shutdown_after_loop(&mut state);
        assert!(state.xwayland.is_none(), "shutdown releases the manager");
        assert!(
            wait_until(|| !process_exists(pid)),
            "the starting service must be stopped and reaped, pid {pid} still exists"
        );

        event_loop
            .dispatch(Some(Duration::from_millis(50)), &mut state)
            .expect("dispatch after shutdown");
        assert!(
            state.xwayland.is_none(),
            "a stale callback must not resurrect the manager"
        );
        assert!(
            !x11_socket_path(&directory, &display).exists(),
            "shutdown must release the socket path"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_failed_rebind_is_reported_and_the_next_retry_starts_a_service() {
        const PATH: &str = "xwayland::integration_tests::manager_loop::a_failed_rebind_is_reported_and_the_next_retry_starts_a_service";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();

        let directory = std::env::temp_dir().join(format!("ferese-manager-rebind-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(
            &directory,
            wrapper(&directory, &[("FAKE_SATELLITE_EXIT_BEFORE_READY", "1")]),
        );
        xwayland::initialize(&mut state);
        let first_generation =
            xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
            if xwayland::status_snapshot(&state).state == State::Failed {
                break;
            }
            assert!(Instant::now() < deadline, "the service never became failed");
        }

        test_hooks::FAIL_REBIND_AFTER.store(0, std::sync::atomic::Ordering::SeqCst);
        let error = xwayland::retry(&mut state).expect_err("an injected rebind failure must be reported");
        assert!(
            error.contains("rebind X11 display"),
            "the reported error must name the rebind step: {error}"
        );
        let failed = xwayland::status_snapshot(&state);
        assert_eq!(
            failed.state,
            State::Failed,
            "a failed rebind must stay retryable: {failed:?}"
        );
        assert!(
            xwayland::request_start(&mut state).is_none(),
            "the circuit is still open"
        );

        test_hooks::FAIL_REBIND_AFTER.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        wrapper(&directory, &[("FAKE_SATELLITE_LINGER", "60")]);
        xwayland::retry(&mut state).expect("a retry after a failed rebind must succeed");
        dispatch_until(&mut event_loop, &mut state, |state| {
            xwayland::status_snapshot(state).state == State::Running
                && xwayland::status_snapshot(state)
                    .generation
                    .is_some_and(|generation| generation > first_generation)
        });
        let status = xwayland::status_snapshot(&state);
        assert!(
            status
                .generation
                .is_some_and(|generation| generation > first_generation),
            "the successful retry must start a real generation: {status:?}"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_listener_registration_failure_leaves_a_retryable_endpoint() {
        const PATH: &str =
            "xwayland::integration_tests::manager_loop::a_listener_registration_failure_leaves_a_retryable_endpoint";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();

        let directory = std::env::temp_dir().join(format!("ferese-manager-register-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(
            &directory,
            wrapper(&directory, &[("FAKE_SATELLITE_EXIT_BEFORE_READY", "1")]),
        );
        // Fail the second listener source: the first is already registered.
        test_hooks::FAIL_LISTENER_REGISTRATION_AFTER.store(1, std::sync::atomic::Ordering::SeqCst);
        xwayland::initialize(&mut state);
        let diagnostic = state.x11_diagnostic.clone().expect("the failure must be visible");
        assert!(
            diagnostic.contains("injected X11 listener registration failure"),
            "the diagnostic must name the failure: {diagnostic}"
        );
        assert!(
            state.xwayland.is_none(),
            "a half-registered endpoint must not be published"
        );

        test_hooks::FAIL_LISTENER_REGISTRATION_AFTER.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        xwayland::retry(&mut state).expect("the service becomes available once registration works");

        let mut first_generation = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            event_loop
                .dispatch(Some(Duration::from_millis(25)), &mut state)
                .expect("dispatch the event loop");
            let status = xwayland::status_snapshot(&state);
            if status.generation.is_some() {
                first_generation = status.generation;
            }
            if status.state == State::Failed {
                break;
            }
            assert!(Instant::now() < deadline, "the service never became failed: {status:?}");
        }
        let first_generation = first_generation.expect("a generation was started");

        test_hooks::FAIL_LISTENER_REGISTRATION_AFTER.store(1, std::sync::atomic::Ordering::SeqCst);
        let error = xwayland::retry(&mut state).expect_err("registration failure must be reported");
        assert!(
            error.contains("injected X11 listener registration failure"),
            "the reported error must name the failure: {error}"
        );
        let failed = xwayland::status_snapshot(&state);
        assert_eq!(
            failed.state,
            State::Failed,
            "the endpoint must stay in a retryable failed state: {failed:?}"
        );
        assert!(
            xwayland::request_start(&mut state).is_none(),
            "the circuit is still open"
        );
        let display = failed.display.expect("the display is still reserved");
        assert!(
            !x11_socket_path(&directory, &display).exists(),
            "the half-registered listeners must be closed"
        );

        test_hooks::FAIL_LISTENER_REGISTRATION_AFTER.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        wrapper(&directory, &[("FAKE_SATELLITE_LINGER", "60")]);
        xwayland::retry(&mut state).expect("the endpoint recovers once registration works");
        dispatch_until(&mut event_loop, &mut state, |state| {
            xwayland::status_snapshot(state).state == State::Running
                && xwayland::status_snapshot(state)
                    .generation
                    .is_some_and(|generation| generation > first_generation)
        });
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_eager_service_that_could_not_start_retries_exactly_once() {
        const PATH: &str =
            "xwayland::integration_tests::manager_loop::an_eager_service_that_could_not_start_retries_exactly_once";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();

        let directory = std::env::temp_dir().join(format!("ferese-manager-eager-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(&directory, wrapper(&directory, &[("FAKE_SATELLITE_LINGER", "60")]));
        state.xwayland_config.startup = XwaylandStartup::Eager;

        test_hooks::FAIL_LISTENER_REGISTRATION_AFTER.store(0, std::sync::atomic::Ordering::SeqCst);
        xwayland::initialize(&mut state);
        assert!(state.xwayland.is_none(), "an unavailable service publishes no endpoint");
        assert!(
            state.x11_diagnostic.is_some(),
            "the unavailable start must leave a diagnostic"
        );
        test_hooks::FAIL_LISTENER_REGISTRATION_AFTER.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);

        // Eager startup must still issue exactly one start request.
        xwayland::retry(&mut state).expect("an eager retry of an unavailable service must succeed");
        assert!(
            state.x11_diagnostic.is_none(),
            "a successful retry clears the diagnostic"
        );

        dispatch_until(&mut event_loop, &mut state, |state| {
            xwayland::status_snapshot(state).state == State::Running
        });
        let status = xwayland::status_snapshot(&state);
        assert_eq!(
            status.generation,
            Some(1),
            "exactly one generation was requested: {status:?}"
        );
        assert_eq!(status.readiness, Readiness::Verified, "readiness is earned: {status:?}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn restart_required_tracks_desired_configuration_only() {
        const PATH: &str =
            "xwayland::integration_tests::manager_loop::restart_required_tracks_desired_configuration_only";
        if !run_isolated(PATH) {
            return;
        }
        test_hooks::reset();

        let directory = std::env::temp_dir().join(format!("ferese-manager-reload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the test directory");

        let (mut event_loop, mut state) = session(&directory, wrapper(&directory, &[("FAKE_SATELLITE_LINGER", "60")]));
        xwayland::initialize(&mut state);
        xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");
        dispatch_until(&mut event_loop, &mut state, |state| {
            xwayland::status_snapshot(state).state == State::Running
        });

        let effective = xwayland::status_snapshot(&state);
        let display = effective.display.clone().expect("a display is reserved");
        let pid = effective.satellite_pid.expect("a running service reports its pid");
        let authority = state
            .xwayland
            .as_ref()
            .and_then(|manager| manager.x11_environment().authority.to_str().map(str::to_owned))
            .expect("an authority path is published");
        assert!(
            !effective.restart_required,
            "the live service matches the session configuration"
        );

        // Every restart-only field, changed one reload at a time.
        let changes = [
            "xwayland { enabled #true; startup \"eager\"; }",
            "xwayland { enabled #true; path \"/nonexistent/satellite\"; }",
            "xwayland { enabled #false; }",
        ];
        for source in changes {
            state
                .reload_config_source(source.into())
                .unwrap_or_else(|error| panic!("the reload must be accepted: {error}"));
            let status = xwayland::status_snapshot(&state);
            assert!(
                status.restart_required,
                "changing the configuration must request a restart: {source} => {status:?}"
            );
            assert_eq!(
                status.state,
                State::Running,
                "a reload must not disturb the live service"
            );
            assert_eq!(
                status.display.as_deref(),
                Some(display.as_str()),
                "the display must not move"
            );
            assert_eq!(status.satellite_pid, Some(pid), "the service must not be restarted");
            let authority_now = state
                .xwayland
                .as_ref()
                .and_then(|manager| manager.x11_environment().authority.to_str().map(str::to_owned))
                .expect("an authority path is published");
            assert_eq!(authority_now, authority, "the authority file must not be replaced");
        }

        // Reverting to the session configuration clears the indication.
        state
            .reload_config_source(
                "xwayland { enabled #true; startup \"on-demand\"; path \"".to_owned()
                    + &wrapper(&directory, &[]).display().to_string()
                    + "\"; }",
            )
            .expect("the reload must be accepted");
        let reverted = xwayland::status_snapshot(&state);
        assert!(
            !reverted.restart_required,
            "reverting to the effective configuration must clear restart_required: {reverted:?}"
        );

        // A rejected reload changes nothing.
        let error = state
            .reload_config_source("xwayland { enabled #true; startup \"whenever\"; }".into())
            .expect_err("an invalid startup mode must be rejected");
        assert!(!error.is_empty(), "the rejection is explained");
        let unchanged = xwayland::status_snapshot(&state);
        assert!(
            !unchanged.restart_required,
            "a rejected reload must not change desired state: {unchanged:?}"
        );
        assert_eq!(
            unchanged.satellite_pid,
            Some(pid),
            "a rejected reload must not touch the service"
        );
        let _ = fs::remove_dir_all(&directory);
    }
}
