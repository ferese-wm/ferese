//! Keeps the lid-policy inhibitor alive while logind handles suspend requests.
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::Builder as ThreadBuilder;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder as ConnectionBuilder;
use zbus::zvariant::{OwnedFd, OwnedValue};

const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Lease {
    active: bool,
    generation: u64,
    descriptor: Option<OwnedFd>,
    suspend: bool,
}

impl Lease {
    fn activate(&mut self, active: bool) {
        self.generation = self.generation.wrapping_add(1);
        self.active = active;
        self.suspend = false;
        // Release immediately, even while the worker is inside a method call.
        self.descriptor = None;
    }

    #[must_use]
    fn current(&self, generation: u64) -> bool {
        self.active && self.generation == generation
    }
}

enum Request {
    Changed,
    Reconnect,
}

pub(super) struct LidPolicy {
    requests: Sender<Request>,
    lease: Arc<Mutex<Lease>>,
    suspend_requested: bool,
}

impl Drop for LidPolicy {
    fn drop(&mut self) {
        self.set_active(false);
        // Dropping the sender also wakes a worker blocked on its retry delay.
    }
}

impl LidPolicy {
    /// Starts the worker, returning an error if its thread cannot be spawned.
    pub(super) fn try_new() -> io::Result<Self> {
        Self::try_new_with_connector(
            || {
                ConnectionBuilder::system()?
                    .method_timeout(Duration::from_secs(5))
                    .build()
            },
            RETRY_MIN,
            RETRY_MAX,
        )
    }

    fn try_new_with_connector(
        connect: impl FnMut() -> zbus::Result<Connection> + Send + 'static,
        retry_min: Duration,
        retry_max: Duration,
    ) -> io::Result<Self> {
        let (requests, receiver) = mpsc::channel();
        let lease = Arc::new(Mutex::new(Lease::default()));
        let worker_lease = lease.clone();
        ThreadBuilder::new()
            .name("ferese-lid-policy".into())
            .spawn(move || run(receiver, worker_lease, connect, retry_min, retry_max))?;
        Ok(Self {
            requests,
            lease,
            suspend_requested: false,
        })
    }

    /// Releases the inhibitor immediately; replies from the previous activation are discarded.
    pub(super) fn set_active(&mut self, active: bool) {
        self.lease
            .lock()
            .expect("lid policy lease must remain consistent across threads")
            .activate(active);
        self.suspend_requested = false;
        let _ = self.requests.send(Request::Changed);
    }

    /// Coalesces requests and cancels pending retries when suspend is no longer wanted.
    pub(super) fn request_suspend(&mut self, requested: bool) {
        if self.suspend_requested == requested {
            return;
        }
        self.suspend_requested = requested;
        let mut lease = self
            .lease
            .lock()
            .expect("lid policy lease must remain consistent across threads");
        lease.generation = lease.generation.wrapping_add(1);
        lease.suspend = requested;
        drop(lease);
        let _ = self.requests.send(Request::Changed);
    }

    /// Replaces the inhibitor after bus or logind recovery without activating a paused session.
    pub(super) fn reconnect(&mut self) {
        let mut lease = self
            .lease
            .lock()
            .expect("lid policy lease must remain consistent across threads");
        lease.generation = lease.generation.wrapping_add(1);
        lease.descriptor = None;
        drop(lease);
        let _ = self.requests.send(Request::Reconnect);
    }
}

fn run(
    requests: Receiver<Request>,
    lease: Arc<Mutex<Lease>>,
    mut connect: impl FnMut() -> zbus::Result<Connection>,
    retry_min: Duration,
    retry_max: Duration,
) {
    let mut connection = None;
    let mut retry = None;
    let mut delay = retry_min;
    loop {
        let request = match retry.take() {
            Some(delay) => match requests.recv_timeout(delay) {
                Ok(request) => Some(request),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match requests.recv() {
                Ok(request) => Some(request),
                Err(_) => break,
            },
        };
        let pending_requests = request.into_iter().chain(requests.try_iter());
        for request in pending_requests {
            if matches!(request, Request::Reconnect) {
                connection = None;
                delay = retry_min;
            }
        }
        let (active, generation, acquire, suspend) = {
            let current = lease
                .lock()
                .expect("lid policy lease must remain consistent across threads");
            (
                current.active,
                current.generation,
                current.descriptor.is_none(),
                current.suspend,
            )
        };
        if !active {
            connection = None;
            delay = retry_min;
            continue;
        }
        let result = (|| {
            let connection = match connection.as_ref() {
                Some(connection) => connection,
                None => connection.insert(connect()?),
            };
            if acquire {
                let descriptor: OwnedFd = connection
                    .call_method(
                        Some("org.freedesktop.login1"),
                        "/org/freedesktop/login1",
                        Some("org.freedesktop.login1.Manager"),
                        "Inhibit",
                        &(
                            "handle-lid-switch",
                            "Ferese",
                            "Reconcile monitors before lid action",
                            "block",
                        ),
                    )?
                    .body()
                    .deserialize()?;
                let mut current = lease
                    .lock()
                    .expect("lid policy lease must remain consistent across threads");
                if !current.current(generation) {
                    return Ok(());
                }
                current.descriptor = Some(descriptor);
            }
            if suspend {
                suspend_if_current(connection, &lease, generation)?;
            }
            Ok::<_, zbus::Error>(())
        })();
        match result {
            Ok(()) => delay = retry_min,
            Err(error) => {
                connection = None;
                let mut current = lease
                    .lock()
                    .expect("lid policy lease must remain consistent across threads");
                if current.current(generation) {
                    current.descriptor = None;
                    if delay == retry_min {
                        tracing::warn!(%error, "logind lid policy unavailable; retrying");
                    }
                    retry = Some(delay);
                    delay = (delay * 2).min(retry_max);
                }
            }
        }
    }
}

fn suspend_if_current(
    connection: &Connection,
    lease: &Mutex<Lease>,
    generation: u64,
) -> zbus::Result<()> {
    if !lease
        .lock()
        .expect("lid policy lease must remain consistent across threads")
        .current(generation)
    {
        return Ok(());
    }
    let closed = super::lid::read_logind(connection)?;
    let inhibitors: OwnedValue = connection
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Manager", "BlockInhibited"),
        )?
        .body()
        .deserialize()?;
    let blocked = String::try_from(inhibitors)?
        .split(':')
        .any(|kind| kind == "sleep");
    let current = lease
        .lock()
        .expect("lid policy lease must remain consistent across threads");
    if !current.current(generation) || !current.suspend {
        return Ok(());
    }
    drop(current);
    if closed && !blocked {
        connection.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Suspend",
            &(false,),
        )?;
    }
    let mut current = lease
        .lock()
        .expect("lid policy lease must remain consistent across threads");
    if current.current(generation) {
        // A successful request, open lid or sleep inhibitor ends this request.
        // A later topology change can submit a new one; transient errors retry.
        current.suspend = false;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
