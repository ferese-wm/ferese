use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use super::*;

#[test]
fn warning_suppression_resets_at_subscription_not_at_next_resume() {
    let mut recovery = Recovery::default();
    assert!(!recovery.subscribed(), "startup is not a recovery or a resume");
    assert!(recovery.unavailable());
    assert!(!recovery.unavailable(), "one warning per outage");
    assert!(recovery.subscribed());
    assert!(!recovery.subscribed(), "one reconciliation per recovery");
    assert!(recovery.unavailable(), "another outage must warn even without a resume");
    assert!(!recovery.unavailable());
}

fn stop_and_join(mut monitor: Monitor) {
    let worker = monitor.worker.take().unwrap();
    drop(monitor);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !worker.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }

    assert!(worker.is_finished(), "cancellation did not stop the worker promptly");
    worker.join().unwrap();
}

#[test]
fn shutdown_interrupts_retry_delay() {
    let (attempt, attempted) = mpsc::channel();
    let monitor = start(
        move || {
            attempt.send(()).unwrap();
            Err(zbus::Error::Failure("test connection failure".into()))
        },
        RETRY_DELAY,
        |_| panic!("no subscription succeeded"),
    )
    .unwrap();
    attempted.recv_timeout(Duration::from_secs(1)).unwrap();
    stop_and_join(monitor);
    assert!(
        attempted.try_recv().is_err(),
        "shutdown must not start another connection"
    );
}

struct Bus {
    process: Child,
    address: String,
}

impl Bus {
    fn new() -> Self {
        let mut process = crate::process::command("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            process,
            address: address.trim().to_owned(),
        }
    }

    fn stop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }

    fn manager(&self) -> zbus::blocking::Connection {
        struct Manager;
        #[zbus::interface(name = "org.freedesktop.login1.Manager")]
        impl Manager {
            #[zbus(property)]
            fn lid_closed(&self) -> bool {
                false
            }
        }

        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/org/freedesktop/login1", Manager)
            .unwrap()
            .build()
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        self.stop();
    }
}

fn signal(manager: &zbus::blocking::Connection, sleeping: bool) {
    manager
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
            &(sleeping,),
        )
        .unwrap();
}

fn confirm_subscription(manager: &zbus::blocking::Connection, events: &Receiver<Refresh>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        signal(manager, false);
        if let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            assert_eq!(event, Refresh::Resumed);
            break;
        }

        assert!(Instant::now() < deadline, "listener did not subscribe");
    }
}

#[test]
fn reconnect_reconciles_a_resume_missed_during_the_outage() {
    if !crate::startup_tests::private_runtime("resume::tests::reconnect_reconciles_a_resume_missed_during_the_outage") {
        return;
    }

    let mut bus = Bus::new();
    let manager = bus.manager();
    let address = Arc::new(Mutex::new(bus.address.clone()));
    let worker_address = address.clone();
    let (sender, events) = mpsc::channel();
    let (attempt, attempts) = mpsc::channel();
    let monitor = start(
        move || {
            attempt.send(()).unwrap();
            zbus::connection::Builder::address(worker_address.lock().unwrap().as_str())
        },
        Duration::from_millis(40),
        move |event| sender.send(event).is_ok(),
    )
    .unwrap();
    confirm_subscription(&manager, &events);
    attempts.recv_timeout(Duration::from_secs(1)).unwrap();
    *address.lock().unwrap() = "unix:path=/nonexistent/ferese-test-bus".into();
    bus.stop();
    attempts.recv_timeout(Duration::from_secs(1)).unwrap();

    let replacement = Bus::new();
    let manager = replacement.manager();
    signal(&manager, false); // No listener can receive this resume.
    *address.lock().unwrap() = replacement.address.clone();
    assert_eq!(events.recv_timeout(Duration::from_secs(2)).unwrap(), Refresh::Reconcile);
    assert!(events.recv_timeout(Duration::from_millis(80)).is_err());
    signal(&manager, true);
    assert!(events.recv_timeout(Duration::from_millis(40)).is_err());
    signal(&manager, false);
    assert_eq!(events.recv_timeout(Duration::from_secs(1)).unwrap(), Refresh::Resumed);
    stop_and_join(monitor);
}

#[test]
fn shutdown_interrupts_a_healthy_signal_wait() {
    if !crate::startup_tests::private_runtime("resume::tests::shutdown_interrupts_a_healthy_signal_wait") {
        return;
    }

    let bus = Bus::new();
    let manager = bus.manager();
    let address = bus.address.clone();
    let (sender, events) = mpsc::channel();
    let monitor = start(
        move || zbus::connection::Builder::address(address.as_str()),
        RETRY_DELAY,
        move |event| sender.send(event).is_ok(),
    )
    .unwrap();
    confirm_subscription(&manager, &events);
    stop_and_join(monitor);
}

#[test]
fn logind_replacement_reconciles_without_a_bus_disconnect() {
    if !crate::startup_tests::private_runtime("resume::tests::logind_replacement_reconciles_without_a_bus_disconnect") {
        return;
    }

    let bus = Bus::new();
    let manager = bus.manager();
    let address = bus.address.clone();
    let (sender, events) = mpsc::channel();
    let monitor = start(
        move || zbus::connection::Builder::address(address.as_str()),
        Duration::from_millis(40),
        move |event| sender.send(event).is_ok(),
    )
    .unwrap();
    confirm_subscription(&manager, &events);
    manager.release_name("org.freedesktop.login1").unwrap();
    assert!(
        events.recv_timeout(Duration::from_millis(100)).is_err(),
        "absent logind is not recovery"
    );
    let replacement = bus.manager();
    assert_eq!(events.recv_timeout(Duration::from_secs(2)).unwrap(), Refresh::Reconcile);
    signal(&replacement, false);
    assert_eq!(events.recv_timeout(Duration::from_secs(1)).unwrap(), Refresh::Resumed);
    stop_and_join(monitor);
}

#[test]
fn shutdown_interrupts_connection_authentication() {
    if !crate::startup_tests::private_runtime("resume::tests::shutdown_interrupts_connection_authentication") {
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("silent-bus");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("unix:path={}", path.display());
    let monitor = start(
        move || zbus::connection::Builder::address(address.as_str()),
        RETRY_DELAY,
        |_| panic!("authentication cannot complete"),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let _connection = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("{error}"),
        }
    };
    stop_and_join(monitor);
}
