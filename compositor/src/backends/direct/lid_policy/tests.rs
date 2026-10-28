use std::io::BufReader;
use std::os::fd::OwnedFd as NativeOwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder as ConnectionBuilder;
use zbus::zvariant::OwnedFd;

use super::LidPolicy;

struct Bus {
    child: Child,
    address: String,
}
impl Bus {
    fn new() -> Self {
        use std::io::BufRead;
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private dbus-daemon must start");
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("daemon stdout is piped"))
            .read_line(&mut address)
            .expect("private bus must publish its address");
        Self {
            child,
            address: address.trim().into(),
        }
    }

    fn serve(&self, manager: Arc<Manager>) -> Connection {
        ConnectionBuilder::address(self.address.as_str())
            .expect("daemon must publish a valid bus address")
            .name("org.freedesktop.login1")
            .expect("logind service name is valid")
            .serve_at("/org/freedesktop/login1", Mock(manager))
            .expect("mock manager must register at the logind path")
            .build()
            .expect("mock logind must connect to the private bus")
    }

    fn policy(&self) -> LidPolicy {
        let address = self.address.clone();
        LidPolicy::try_new_with_connector(
            move || connect(&address),
            Duration::from_millis(20),
            Duration::from_millis(80),
        )
        .expect("lid policy worker must start")
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect(address: &str) -> zbus::Result<Connection> {
    ConnectionBuilder::address(address)?
        .method_timeout(Duration::from_secs(1))
        .build()
}

#[derive(Default)]
struct Manager {
    inhibit_calls: AtomicUsize,
    inhibit_failures: AtomicUsize,
    suspend_calls: AtomicUsize,
    suspend_failures: AtomicUsize,
    suspend_successes: AtomicUsize,
    sleep_blocked: AtomicBool,
    hold_inhibit: Mutex<bool>,
    release_inhibit: Condvar,
    peers: Mutex<Vec<UnixStream>>,
}

struct Mock(Arc<Manager>);
#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Mock {
    #[zbus(property)]
    fn lid_closed(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn block_inhibited(&self) -> &str {
        if self.0.sleep_blocked.load(Ordering::SeqCst) {
            "sleep"
        } else {
            "handle-lid-switch"
        }
    }

    fn inhibit(
        &self,
        what: &str,
        _who: &str,
        _why: &str,
        mode: &str,
    ) -> zbus::fdo::Result<OwnedFd> {
        assert_eq!((what, mode), ("handle-lid-switch", "block"));
        self.0.inhibit_calls.fetch_add(1, Ordering::SeqCst);
        if take_failure(&self.0.inhibit_failures) {
            return Err(zbus::fdo::Error::Failed(
                "temporary inhibitor failure".into(),
            ));
        }
        drop(
            self.0
                .release_inhibit
                .wait_while(
                    self.0
                        .hold_inhibit
                        .lock()
                        .expect("mock state must remain consistent"),
                    |held| *held,
                )
                .expect("inhibitor gate must remain consistent"),
        );
        let (peer, descriptor) = UnixStream::pair().expect("inhibitor test needs a socket pair");
        peer.set_nonblocking(true)
            .expect("lease checks must not block");
        self.0
            .peers
            .lock()
            .expect("mock state must remain consistent")
            .push(peer);
        Ok(NativeOwnedFd::from(descriptor).into())
    }

    fn suspend(&self, interactive: bool) -> zbus::fdo::Result<()> {
        assert!(!interactive);
        self.0.suspend_calls.fetch_add(1, Ordering::SeqCst);
        if take_failure(&self.0.suspend_failures) {
            return Err(zbus::fdo::Error::Failed("temporary suspend failure".into()));
        }
        self.0.suspend_successes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn take_failure(count: &AtomicUsize) -> bool {
    count
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "lid policy worker did not reach expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[must_use]
fn acquired(policy: &LidPolicy) -> bool {
    policy
        .lease
        .lock()
        .expect("mock state must remain consistent")
        .descriptor
        .is_some()
}

#[must_use]
fn released(manager: &Manager) -> bool {
    use std::io::Read;
    let mut peers = manager
        .peers
        .lock()
        .expect("mock state must remain consistent");
    !peers.is_empty()
        && peers
            .iter_mut()
            .all(|peer| matches!(peer.read(&mut [0]), Ok(0)))
}

#[test]
#[ignore = "requires dbus-daemon and private test sockets"]
fn initial_bus_failure_recovers_without_another_activation() {
    let bus = Bus::new();
    let manager = Arc::new(Manager::default());
    let _server = bus.serve(manager.clone());
    let unavailable = Arc::new(AtomicBool::new(true));
    let attempts = Arc::new(AtomicUsize::new(0));
    let ready = unavailable.clone();
    let calls = attempts.clone();
    let address = bus.address.clone();
    let missing = tempfile::tempdir().expect("missing bus address needs a private directory");
    let absent_address = format!("unix:path={}/missing", missing.path().display());
    let mut policy = LidPolicy::try_new_with_connector(
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            connect(if ready.load(Ordering::SeqCst) {
                &absent_address
            } else {
                &address
            })
        },
        Duration::from_millis(20),
        Duration::from_millis(80),
    )
    .expect("lid policy worker must start");
    policy.set_active(true);
    wait(|| attempts.load(Ordering::SeqCst) >= 2);
    assert!(!acquired(&policy));
    unavailable.store(false, Ordering::SeqCst);
    wait(|| acquired(&policy));
    let connected = attempts.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        connected,
        "healthy idle workers must not poll"
    );
    assert_eq!(manager.inhibit_calls.load(Ordering::SeqCst), 1);
}

#[test]
#[ignore = "requires dbus-daemon and private test sockets"]
fn inhibitor_failure_retries_and_pause_releases_the_lease() {
    let bus = Bus::new();
    let manager = Arc::new(Manager::default());
    manager.inhibit_failures.store(1, Ordering::SeqCst);
    let _server = bus.serve(manager.clone());
    let mut policy = bus.policy();
    policy.set_active(true);
    wait(|| acquired(&policy));
    assert_eq!(manager.inhibit_calls.load(Ordering::SeqCst), 2);
    policy.set_active(false);
    assert!(!acquired(&policy));
    wait(|| released(&manager));
    std::thread::sleep(Duration::from_millis(160));
    assert_eq!(manager.inhibit_calls.load(Ordering::SeqCst), 2);
}

#[test]
#[ignore = "requires dbus-daemon and private test sockets"]
fn pause_cancels_retries_and_rejects_delayed_acquisitions() {
    let bus = Bus::new();
    let manager = Arc::new(Manager::default());
    manager.inhibit_failures.store(usize::MAX, Ordering::SeqCst);
    let _server = bus.serve(manager.clone());
    let mut policy = bus.policy();
    policy.set_active(true);
    wait(|| manager.inhibit_calls.load(Ordering::SeqCst) >= 2);
    policy.set_active(false);
    std::thread::sleep(Duration::from_millis(100));
    let stopped = manager.inhibit_calls.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(160));
    assert_eq!(manager.inhibit_calls.load(Ordering::SeqCst), stopped);
    manager.inhibit_failures.store(0, Ordering::SeqCst);
    *manager
        .hold_inhibit
        .lock()
        .expect("mock state must remain consistent") = true;
    policy.set_active(true);
    wait(|| manager.inhibit_calls.load(Ordering::SeqCst) > stopped);
    policy.set_active(false);
    *manager
        .hold_inhibit
        .lock()
        .expect("mock state must remain consistent") = false;
    manager.release_inhibit.notify_all();
    wait(|| released(&manager));
    assert!(
        !acquired(&policy),
        "late descriptor must not escape its activation generation"
    );
    policy.set_active(true);
    wait(|| acquired(&policy));
    drop(policy);
    wait(|| released(&manager));
}

#[test]
#[ignore = "requires dbus-daemon and private test sockets"]
fn recovered_owner_replaces_the_previous_inhibitor() {
    let bus = Bus::new();
    let old = Arc::new(Manager::default());
    let server = bus.serve(old.clone());
    let mut policy = bus.policy();
    policy.set_active(true);
    wait(|| acquired(&policy));
    server
        .release_name("org.freedesktop.login1")
        .expect("old logind owner must release its name");
    let new = Arc::new(Manager::default());
    let _replacement = bus.serve(new.clone());
    // The resume monitor invokes request_reconcile on owner/bus recovery.
    policy.reconnect();
    wait(|| new.inhibit_calls.load(Ordering::SeqCst) == 1 && acquired(&policy));
    wait(|| released(&old));
    assert_eq!(old.inhibit_calls.load(Ordering::SeqCst), 1);
}

#[test]
#[ignore = "requires dbus-daemon and private test sockets"]
fn suspend_failure_retries_once_current_request_then_stays_idle() {
    let bus = Bus::new();
    let manager = Arc::new(Manager::default());
    manager.suspend_failures.store(1, Ordering::SeqCst);
    let _server = bus.serve(manager.clone());
    let mut policy = bus.policy();
    policy.set_active(true);
    wait(|| acquired(&policy));
    policy.request_suspend(true);
    wait(|| manager.suspend_successes.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(160));
    policy.request_suspend(true);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(manager.suspend_calls.load(Ordering::SeqCst), 2);
    policy.request_suspend(false);
    manager.sleep_blocked.store(true, Ordering::SeqCst);
    policy.request_suspend(true);
    wait(|| {
        !policy
            .lease
            .lock()
            .expect("mock state must remain consistent")
            .suspend
    });
    assert_eq!(
        manager.suspend_calls.load(Ordering::SeqCst),
        2,
        "sleep inhibitors must still win"
    );
}

#[test]
#[ignore = "requires dbus-daemon and private test sockets"]
fn cancelling_suspend_prevents_later_retry() {
    let bus = Bus::new();
    let manager = Arc::new(Manager::default());
    manager.suspend_failures.store(usize::MAX, Ordering::SeqCst);
    let _server = bus.serve(manager.clone());
    let mut policy = bus.policy();
    policy.set_active(true);
    wait(|| acquired(&policy));
    policy.request_suspend(true);
    wait(|| manager.suspend_calls.load(Ordering::SeqCst) >= 1);
    policy.request_suspend(false);
    std::thread::sleep(Duration::from_millis(100));
    let stopped = manager.suspend_calls.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(160));
    assert_eq!(manager.suspend_calls.load(Ordering::SeqCst), stopped);
    assert!(acquired(&policy));
}
