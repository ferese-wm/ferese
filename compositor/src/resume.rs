//! System wake and listener recovery share reconciliation, but not idle activity.
use std::error::Error;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Poll, Waker};
use std::thread::JoinHandle;
use std::time::Duration;

use calloop::channel::{Event, channel};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use futures_lite::{StreamExt, future};

use crate::Ferese;

const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refresh {
    Resumed,
    Reconcile,
}

#[derive(Debug, PartialEq, Eq)]
enum MonitorExit {
    Stop,
    Reconnect,
}

#[derive(Default)]
struct StopState {
    requested: bool,
    waker: Option<Waker>,
}

#[derive(Default)]
struct Cancellation {
    state: Mutex<StopState>,
    changed: Condvar,
}

impl Cancellation {
    fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.requested = true;
        let waker = state.waker.take();
        self.changed.notify_all();
        drop(state);

        if let Some(waker) = waker {
            waker.wake();
        }
    }

    async fn cancelled(&self) {
        future::poll_fn(|context| {
            let mut state = self.state.lock().unwrap();
            if state.requested {
                return Poll::Ready(());
            }

            state.waker = Some(context.waker().clone());
            Poll::Pending
        })
        .await;
    }

    fn retry(&self, delay: Duration) -> bool {
        let (state, _) = self
            .changed
            .wait_timeout_while(self.state.lock().unwrap(), delay, |state| !state.requested)
            .unwrap();
        !state.requested
    }
}

pub(crate) struct Monitor {
    stop: Arc<Cancellation>,
    worker: Option<JoinHandle<()>>,
    source: Option<(LoopHandle<'static, Ferese>, RegistrationToken)>,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some((handle, token)) = self.source.take() {
            handle.remove(token);
        }

        // Cancellation wakes both the signal wait and retry delay. Do not join
        // a DBus worker on the compositor thread during shutdown.
        self.worker.take();
    }
}

fn refresh(state: &mut Ferese, event: Refresh) {
    match event {
        Refresh::Resumed => crate::backends::direct::system_resumed(state),
        Refresh::Reconcile => crate::backends::direct::reconcile_after_monitor_recovery(state),
    }

    state.refresh_theme();
}

pub(crate) fn init(event_loop: &mut EventLoop<'static, Ferese>) -> Result<Monitor, Box<dyn Error>> {
    let (sender, receiver) = channel();
    let handle = event_loop.handle();
    let token = handle.insert_source(receiver, |event, _, state| {
        if let Event::Msg(event) = event {
            refresh(state, event);
        }
    })?;

    match start(zbus::connection::Builder::system, RETRY_DELAY, move |event| {
        sender.send(event).is_ok()
    }) {
        Ok(mut monitor) => {
            monitor.source = Some((handle, token));
            Ok(monitor)
        }
        Err(error) => {
            handle.remove(token);
            Err(error.into())
        }
    }
}

#[derive(Default)]
struct Recovery {
    reconnecting: bool,
    warned: bool,
}

impl Recovery {
    fn unavailable(&mut self) -> bool {
        self.reconnecting = true;
        let warn = !self.warned;
        self.warned = true;
        warn
    }

    fn subscribed(&mut self) -> bool {
        self.warned = false;
        std::mem::take(&mut self.reconnecting)
    }
}

fn start(
    connect: impl Fn() -> zbus::Result<zbus::connection::Builder<'static>> + Send + 'static,
    retry_delay: Duration,
    mut send: impl FnMut(Refresh) -> bool + Send + 'static,
) -> std::io::Result<Monitor> {
    let stop = Arc::new(Cancellation::default());
    let worker_stop = stop.clone();
    let worker = std::thread::Builder::new()
        .name("ferese-system-resume".into())
        .spawn(move || {
            let mut recovery = Recovery::default();
            loop {
                let result = future::block_on(future::race(
                    async {
                        worker_stop.cancelled().await;
                        Ok(MonitorExit::Stop)
                    },
                    async {
                        let connection = connect()?.method_timeout(Duration::from_secs(2)).build().await?;
                        future::race(
                            async {
                                connection.closed().await;
                                Ok(MonitorExit::Reconnect)
                            },
                            listen(&connection, &mut recovery, &mut send),
                        )
                        .await
                    },
                ));

                match result {
                    Ok(MonitorExit::Stop) => break,
                    Ok(MonitorExit::Reconnect) => {
                        if recovery.unavailable() {
                            tracing::warn!("system resume monitoring interrupted; retrying connection");
                        }
                    }
                    Err(error) => {
                        if recovery.unavailable() {
                            tracing::warn!(%error, "system resume monitor unavailable; retrying connection");
                        }
                    }
                }

                if !worker_stop.retry(retry_delay) {
                    break;
                }
            }
        })?;

    Ok(Monitor {
        stop,
        worker: Some(worker),
        source: None,
    })
}

async fn listen(
    connection: &zbus::Connection,
    recovery: &mut Recovery,
    send: &mut impl FnMut(Refresh) -> bool,
) -> zbus::Result<MonitorExit> {
    let proxy = zbus::Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut owners = proxy.receive_owner_changed().await?;
    let mut signals = proxy.receive_signal("PrepareForSleep").await?;
    // AddMatch succeeds even when logind is absent. Only call the listener
    // recovered once the service is present, and retry if its owner changes.
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &("org.freedesktop.login1",),
        )
        .await?;

    // Subscribe first: a resume during reconciliation must remain queued.
    if recovery.subscribed() {
        tracing::debug!("system resume monitoring recovered; reconciling current state");
        if !send(Refresh::Reconcile) {
            return Ok(MonitorExit::Stop);
        }
    }

    future::race(
        async {
            owners.next().await;
            Ok(MonitorExit::Reconnect)
        },
        async {
            while let Some(message) = signals.next().await {
                let (sleeping,): (bool,) = message.body().deserialize()?;
                if !sleeping && !send(Refresh::Resumed) {
                    return Ok(MonitorExit::Stop);
                }
            }

            Ok(MonitorExit::Reconnect)
        },
    )
    .await
}

#[cfg(test)]
mod tests;
