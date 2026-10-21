//! Keep native snapshots until the owning service signals a change.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cosmic::iced::futures::channel::oneshot;
use cosmic::iced::futures::{FutureExt, StreamExt};
use zbus::blocking::{Connection, MessageIterator};

use super::{PollState, StatusBus, wake_status};

struct Signals {
    revision: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    _stop: oneshot::Sender<()>,
}

impl Signals {
    fn new(
        connection: &Connection,
        service: &str,
        wake: Option<Arc<PollState>>,
    ) -> zbus::Result<Self> {
        // Install both rules before the snapshot, including when the service is
        // currently absent. Owner changes invalidate paths after a restart.
        let changes = MessageIterator::for_match_rule(
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(service)?
                .build(),
            connection,
            Some(64),
        )?;
        let owners = MessageIterator::for_match_rule(
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .add_arg(service)?
                .build(),
            connection,
            Some(8),
        )?;
        let revision = Arc::new(AtomicU64::new(1));
        let alive = Arc::new(AtomicBool::new(true));
        let changed = revision.clone();
        let running = alive.clone();
        let (stop, stopped) = oneshot::channel::<()>();
        std::thread::spawn(move || {
            // Enter a runtime for zbus stream cleanup, including cancellation.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                // Dropping the blocking wrappers enters zbus' runtime, so do
                // this before entering our async runtime.
                let changes = changes.into_inner();
                let owners = owners.into_inner();
                let event_wake = wake.clone();
                runtime.block_on(async move {
                    let mut events = cosmic::iced::futures::stream::select(changes, owners);
                    let mut stopped = stopped.fuse();
                    loop {
                        cosmic::iced::futures::select! {
                            _ = stopped => break,
                            event = events.next() => {
                                changed.fetch_add(1, Ordering::Release);
                                if let Some(wake) = &event_wake { wake_status(wake); }
                                if !matches!(event, Some(Ok(_))) { break; }
                            }
                        }
                    }
                    let (changes, owners) = events.into_inner();
                    zbus::AsyncDrop::async_drop(changes).await;
                    zbus::AsyncDrop::async_drop(owners).await;
                });
            }
            running.store(false, Ordering::Release);
            if let Some(wake) = &wake {
                wake_status(wake);
            }
        });
        Ok(Self {
            revision,
            alive,
            _stop: stop,
        })
    }
}

pub(super) struct Cache<T> {
    bus: StatusBus,
    service: &'static str,
    signals: Option<Signals>,
    revision: u64,
    action: u64,
    value: Option<T>,
    retry_at: Instant,
    retry: bool,
    wake: Option<Arc<PollState>>,
}

impl<T: Clone> Cache<T> {
    pub(super) fn new(service: &'static str) -> Self {
        Self {
            bus: StatusBus::new(true),
            service,
            signals: None,
            revision: 0,
            action: u64::MAX,
            value: None,
            retry_at: Instant::now(),
            retry: false,
            wake: None,
        }
    }

    pub(super) fn with_wake(service: &'static str, wake: Arc<PollState>) -> Self {
        let mut cache = Self::new(service);
        cache.wake = Some(wake);
        cache
    }

    pub(super) fn next_retry(&self) -> Option<Instant> {
        (self.action != u64::MAX && (self.signals.is_none() || self.retry)).then_some(self.retry_at)
    }

    pub(super) fn read(
        &mut self,
        action: u64,
        read: fn(&Connection) -> zbus::Result<Option<T>>,
    ) -> Option<T> {
        if self
            .signals
            .as_ref()
            .is_some_and(|s| !s.alive.load(Ordering::Acquire))
        {
            self.signals = None;
            self.bus.connection = None;
            self.value = None;
            self.retry_at = Instant::now();
        }
        if self.signals.is_none() && Instant::now() >= self.retry_at {
            self.signals = self
                .bus
                .query(|connection| Signals::new(connection, self.service, self.wake.clone()));
            self.revision = 0;
        }
        // If subscription setup fails, read fresh rather than trusting a cache
        // with no invalidation. Retry setup at the bounded recovery deadline.
        let revision = self
            .signals
            .as_ref()
            .map(|s| s.revision.load(Ordering::Acquire));
        if (revision.is_none() && Instant::now() >= self.retry_at)
            || revision.is_some_and(|revision| revision != self.revision)
            || action != self.action
            || (self.retry && Instant::now() >= self.retry_at)
        {
            // Keep signal subscriptions across service errors. A dead bus is
            // detected by the listener; a transient query failure retries later.
            let result = self.bus.query(|connection| {
                let owned = connection
                    .call_method(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        Some("org.freedesktop.DBus"),
                        "NameHasOwner",
                        &(self.service,),
                    )?
                    .body()
                    .deserialize::<bool>()?;
                if owned { read(connection) } else { Ok(None) }
            });
            self.retry = result.is_none();
            self.value = result.flatten();
            self.retry_at = Instant::now() + Duration::from_secs(5);
            self.action = action;
            self.revision = revision.unwrap_or(0);
        }
        self.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    struct Counter(Arc<AtomicU64>);
    #[zbus::interface(name = "org.ferese.StatusCounter")]
    impl Counter {
        fn read(&self) -> u64 {
            self.0.fetch_add(1, Ordering::Relaxed) + 1
        }
    }
    fn read_counter(connection: &Connection) -> zbus::Result<Option<u64>> {
        connection
            .call_method(
                Some("org.ferese.StatusCounter"),
                "/test",
                Some("org.ferese.StatusCounter"),
                "Read",
                &(),
            )?
            .body()
            .deserialize()
            .map(Some)
    }
    #[test]
    #[ignore = "requires dbus-daemon; uses a private test bus"]
    fn cache_skips_idle_reads_but_refreshes_after_actions_signals_and_failures() {
        let bus = crate::status::tests::TestBus::new();
        let count = Arc::new(AtomicU64::new(0));
        let server = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.ferese.StatusCounter")
            .unwrap()
            .serve_at("/test", Counter(count.clone()))
            .unwrap()
            .build()
            .unwrap();
        let mut cache = Cache::new("org.ferese.StatusCounter");
        cache.bus.connection = Some(bus.connect());
        assert_eq!(cache.read(0, read_counter), Some(1));
        for _ in 0..10 {
            assert_eq!(cache.read(0, read_counter), Some(1));
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(
            cache.next_retry().is_none(),
            "healthy subscriptions must not poll"
        );
        assert_eq!(cache.read(1, read_counter), Some(2));
        server
            .emit_signal(
                None::<&str>,
                "/test",
                "org.ferese.StatusCounter",
                "Changed",
                &(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while cache.read(1, read_counter) != Some(3) {
            assert!(Instant::now() < deadline, "signal did not invalidate cache");
            std::thread::sleep(Duration::from_millis(1));
        }
        server.release_name("org.ferese.StatusCounter").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while cache.read(1, read_counter).is_some() {
            assert!(Instant::now() < deadline, "owner loss kept stale data");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            cache.next_retry().is_none(),
            "absent services wait for owner changes"
        );
        server.request_name("org.ferese.StatusCounter").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while cache.read(1, read_counter).is_none() {
            assert!(Instant::now() < deadline, "owner return did not recover");
            std::thread::sleep(Duration::from_millis(1));
        }
        // A transient method error may have no subsequent signal. It must
        // clear the snapshot immediately and retry after the bounded backoff.
        assert!(
            cache
                .read(2, |_| Err(zbus::Error::Failure("temporary".into())))
                .is_none()
        );
        assert!(cache.read(2, read_counter).is_none());
        // The production cache reconnects to the system bus after an error;
        // keep this recovery attempt on the isolated test bus instead.
        cache.bus.connection = Some(bus.connect());
        cache.retry_at = Instant::now();
        assert!(cache.read(2, read_counter).is_some());
    }

    #[test]
    #[ignore = "requires dbus-daemon; uses a private test bus"]
    fn signals_invalidate_and_owner_loss_is_observed() {
        let bus = crate::status::tests::TestBus::new();
        let client = bus.connect();
        let server = bus.connect();
        server.request_name("org.ferese.StatusTest").unwrap();
        let wake = Arc::new((
            std::sync::Mutex::new((0, false, None, false, 0)),
            std::sync::Condvar::new(),
        ));
        let signals = Signals::new(&client, "org.ferese.StatusTest", Some(wake.clone())).unwrap();
        let wait = |before| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while signals.revision.load(Ordering::Acquire) == before && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(signals.revision.load(Ordering::Acquire) > before);
        };
        let before = signals.revision.load(Ordering::Acquire);
        server
            .emit_signal(
                None::<&str>,
                "/test",
                "org.ferese.StatusTest",
                "Changed",
                &(),
            )
            .unwrap();
        let (state, timeout) = wake
            .1
            .wait_timeout_while(wake.0.lock().unwrap(), Duration::from_secs(2), |state| {
                state.4 == 0
            })
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "service signal must wake the status worker"
        );
        assert!(state.4 > 0);
        drop(state);
        wait(before);
        let before = signals.revision.load(Ordering::Acquire);
        server.release_name("org.ferese.StatusTest").unwrap();
        wait(before);
        let alive = signals.alive.clone();
        drop(signals);
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !alive.load(Ordering::Acquire),
            "dropping the service must stop its listener"
        );
    }
}
