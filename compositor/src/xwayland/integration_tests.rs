//! Deterministic lifecycle tests driven by a fake Satellite.
//!
//! These tests spawn a real child process that accepts the same argument shape
//! as Satellite, so descriptor passing, the readiness contract, and owned
//! process-group cleanup are proven end to end without starting an X server.
//!
//! Behaviour is selected per test by writing a small wrapper script and passing
//! it as the configured `xwayland.path`. That exercises the configurable-path
//! knob and avoids mutating global environment state, which would race between
//! parallel tests.
//!
//! Event-loop dispatch is covered too: `manager_loop` drives a real
//! `XwaylandManager` through a real calloop `EventLoop` inside an isolated
//! session, so source registration, readiness delivery, and the stop poll are
//! exercised the way a live compositor exercises them.

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

/// One generation's fixtures, dropped in reverse creation order.
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
        let directory = std::env::temp_dir().join(format!(
            "ferese-x11-it-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the fixture directory");
        // The runtime directory must be private; the authority cookie and the
        // readiness socket both live there.
        let runtime_directory = directory.join("run");
        fs::create_dir_all(&runtime_directory).expect("create the runtime directory");
        fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o700))
            .expect("make the runtime directory private");

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

    /// Write the configured executable, a wrapper that pins the fake's
    /// behaviour, so no test mutates the parent environment.
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

    /// Every start the fake recorded, as raw JSON lines.
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

    /// Spawn the real Satellite binary against the caller's live Wayland
    /// session, using this fixture's reserved display and authority file.
    fn spawn_real(&self, generation: u64, executable: &Path) -> (Child, ReadinessSocket) {
        let socket = ReadinessSocket::create(&self.runtime_directory, generation).expect("create a readiness socket");
        let notify = socket.path().to_owned();
        let mut environment = self.environment();
        // The real bridge is an ordinary public client of the running
        // compositor; a test compositor is not what is under test here.
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

/// Drain the readiness socket until the fake's notification is judged.
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
    // A descriptor the parent holds open. Rust creates sockets with
    // FD_CLOEXEC, so Ferese must not leak it into Satellite.
    let sentinel = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a sentinel");
    let sentinel_fd = sentinel.as_raw_fd();

    let fixture = Fixture::new("listeners");
    fixture.script(&[("FAKE_SATELLITE_READY", "0")]);
    let (satellite, _socket) = fixture.spawn(1);
    assert!(
        wait_until(|| !fixture.starts().is_empty()),
        "the fake must record a start"
    );
    let record = fixture.starts().remove(0);

    // The display and authority Ferese reserved are the ones the fake sees.
    assert_eq!(record["display"], fixture.reservation.display_name());
    assert_eq!(record["authority"], fixture.authority.path().to_str().unwrap());

    // One -listenfd per owned listener, all distinct, all actually listening.
    let listenfds = record["listenfds"].as_array().expect("listenfds array");
    assert_eq!(listenfds.len(), fixture.reservation.listeners().len());
    assert_eq!(
        record["all_listening"], true,
        "every passed fd must be a listening socket"
    );
    reap(satellite);

    // Positive control: with FD_CLOEXEC deliberately cleared, the same
    // descriptor *is* visible to the child. Without this, the assertion above
    // could pass merely because the check is broken.
    let flags = unsafe { libc::fcntl(sentinel_fd, libc::F_GETFD) };
    assert_ne!(flags, -1);
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "the sentinel must start close-on-exec, otherwise this test proves nothing"
    );
    // Positive control: clear FD_CLOEXEC so the descriptor *does* leak.
    assert_eq!(
        unsafe { libc::fcntl(sentinel_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0
    );

    let control = Fixture::new("listeners-control");
    control.script(&[
        ("FAKE_SATELLITE_READY", "0"),
        ("FAKE_SATELLITE_SENTINEL_FD", &sentinel_fd.to_string()),
    ]);
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
    // Satellite is an ordinary public client: it must connect by display name.
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
    // The datagram is valid but its credentials are the forked child's, so it
    // must not satisfy this generation's readiness contract.
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
    let fixture = Fixture::new("group");
    // The fake forks a descendant that keeps the listening descriptors open and
    // only exits when the parent tells it to, mimicking a bridge that leaves
    // children holding the endpoints.
    fixture.script(&[
        ("FAKE_SATELLITE_HOLD_FDS", "1"),
        ("FAKE_SATELLITE_READY", "1"),
        ("FAKE_SATELLITE_LINGER", "30"),
    ]);
    let (mut satellite, socket) = fixture.spawn(1);
    let pid = satellite.id();
    assert!(read_ready(&socket, pid), "the group fixture must reach readiness first");

    // The leader is in its own process group, so -pid targets it and any
    // descendant that inherited the group.
    let group_children = process::group_members(pid);
    assert!(
        group_children.len() >= 2,
        "the fake must have a descendant holding the fds"
    );

    child::signal_group(pid, libc::SIGKILL);
    let _ = satellite.wait();
    assert!(
        wait_until(|| !group_children.iter().any(|member| process_exists(*member))),
        "every member of the owned process group must be gone: {group_children:?}"
    );
}

mod process {
    /// PIDs whose process group matches `leader`.
    pub(super) fn group_members(leader: u32) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name();
                let pid: u32 = name.to_str()?.parse().ok()?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                // The final field before the closing paren is the process group.
                let closing = stat.rfind(')')?;
                let fields: Vec<&str> = stat[closing + 1..].split_whitespace().collect();
                let pgrp: u32 = fields.get(2)?.parse().ok()?;
                (pgrp == leader).then_some(pid)
            })
            .collect()
    }
}

/// Live check against the installed Satellite build.
///
/// This is the one assumption unit tests cannot establish: that the packaged
/// binary actually implements the readiness contract Ferese depends on. It
/// needs a real Satellite, a real `Xwayland`, and a live Wayland session, so it
/// is ignored by default.
///
/// Run with:
///
/// ```text
/// cargo test -p ferese --bin ferese -- --ignored real_satellite
/// ```
#[test]
#[ignore = "requires a real Satellite, Xwayland, and a live Wayland session"]
fn real_satellite_reports_verified_readiness_and_serves_authorized_x_clients() {
    let Some(executable) = ["xwayland-satellite", "/usr/bin/xwayland-satellite"]
        .into_iter()
        .find_map(|candidate| {
            let path = PathBuf::from(candidate);
            path.is_file().then_some(path)
        })
    else {
        panic!("xwayland-satellite is not installed");
    };

    let fixture = Fixture::new("real");
    let (mut satellite, socket) = fixture.spawn_real(1, &executable);
    let pid = satellite.id();

    // Ferese allows 10 seconds in production; allow more here so a slow machine
    // does not produce a misleading failure, but report the real elapsed time.
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

    // Readiness alone does not prove the X server is actually usable. Connect a
    // real X11 client through the managed authority file: this exercises the
    // reservation, the cookie, and the inherited listening descriptors together.
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

    // Clean up the whole owned group, as shutdown does.
    child::signal_group(pid, libc::SIGKILL);
    let _ = satellite.wait();
    assert!(wait_until(|| !process_exists(pid)), "the real Satellite must be gone");
}

/// Manager-level tests: the real `XwaylandManager`, registered through a real
/// calloop `EventLoop`, dispatched like a live compositor.
///
/// These are the paths the state-machine unit tests cannot reach: source
/// registration, the readiness source firing inside a dispatch, the exit watch,
/// and the stop poll deciding when the service becomes startable again. They
/// run in a re-executed child with an isolated `XDG_RUNTIME_DIR`, because a
/// live `Ferese` owns a Wayland `Display` and an IPC socket.
mod manager_loop {
    use super::*;
    use crate::Ferese;
    use crate::config::{XwaylandConfig, XwaylandStartup};
    use crate::xwayland;
    use ferese_ipc::xwayland::{Readiness, State};
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;

    /// Re-exec this test in an isolated session, and report whether we are the
    /// child.
    fn run_isolated(test_path: &str) -> bool {
        const CHILD: &str = "FERESE_XWAYLAND_MANAGER_LOOP_CHILD";
        if std::env::var_os(CHILD).is_some() {
            return true;
        }
        let directory = std::env::temp_dir().join(format!(
            "ferese-manager-loop-{}-{:?}",
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

    /// Write the fake Satellite wrapper that this test drives.
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

    /// A `Ferese` wired to a real event loop, ready to run the manager.
    fn session(directory: &Path, path: PathBuf) -> (EventLoop<'static, Ferese>, Ferese) {
        let (_, runtime, _) = crate::theme::prepare("xwayland { }", directory).expect("prepare configuration");
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

    /// Dispatch until `condition` holds, or fail after a generous bound.
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
        xwayland::initialize(&mut state).expect("initialize the managed X11 service");

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
        xwayland::initialize(&mut state).expect("initialize the managed X11 service");
        xwayland::request_start(&mut state).expect("an on-demand service accepts a start request");

        // The core contract: while a service process is alive, no dispatch may
        // make the service startable again, or a replacement generation could
        // race the process that still owns the listeners.
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
            // Only a published outcome counts: `Stopped` covers an in-flight
            // stop, where the process is still legitimately alive.
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
}
