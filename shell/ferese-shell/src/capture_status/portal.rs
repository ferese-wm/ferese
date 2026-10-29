use std::time::Duration;

use cosmic::iced::futures::{SinkExt, Stream, StreamExt};
use ferese_ipc::capture::{INTERFACE, PATH, SERVICE, Session};

use super::Event;

async fn snapshot(proxy: &zbus::Proxy<'_>) -> zbus::Result<Vec<Session>> {
    let reply = tokio::time::timeout(
        Duration::from_secs(3),
        proxy.call_with_flags::<_, _, String>(
            "GetSnapshot",
            zbus::proxy::MethodFlags::NoAutoStart.into(),
            &(),
        ),
    )
    .await;
    let text = match reply {
        Ok(Ok(Some(text))) => text,
        Ok(Err(zbus::Error::MethodError(name, _, _)))
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    | "org.freedesktop.DBus.Error.UnknownMethod"
                    | "org.freedesktop.DBus.Error.UnknownInterface"
            ) =>
        {
            return Ok(vec![]);
        }
        Ok(Err(error)) => return Err(error),
        _ => return Err(zbus::Error::Failure("Capture status timed out".into())),
    };
    if text.len() > 64 * 1024 {
        return Err(zbus::Error::Failure(
            "Capture status exceeds its limit".into(),
        ));
    }
    serde_json::from_str::<Vec<Session>>(&text)
        .ok()
        .filter(|sessions| sessions.len() <= 8 && sessions.iter().all(|s| s.sources.len() <= 8))
        .ok_or_else(|| zbus::Error::Failure("Invalid capture status".into()))
}

pub(super) fn stream() -> impl Stream<Item = Event> {
    stream_at(None)
}

fn stream_at(address: Option<String>) -> impl Stream<Item = Event> {
    cosmic::iced::stream::channel(1, async move |mut output| {
        loop {
            let result: Result<(), zbus::Error> = async {
                let connection = if let Some(address) = &address {
                    zbus::connection::Builder::address(address.as_str())?
                        .build()
                        .await?
                } else {
                    zbus::Connection::session().await?
                };
                let proxy = zbus::Proxy::new(&connection, SERVICE, PATH, INTERFACE).await?;
                let mut owners = proxy.receive_owner_changed().await?;
                let mut changes = proxy.receive_signal("Changed").await?;
                let mut last = snapshot(&proxy).await?;
                if output.send(Event::Sharing(last.clone())).await.is_err() {
                    return Ok(());
                }
                loop {
                    tokio::select! {
                        owner = owners.next() => {
                            let Some(owner) = owner else { break; };
                            // Forget the previous service before reading its replacement.
                            last.clear();
                            if output.send(Event::Sharing(vec![])).await.is_err() { return Ok(()); }
                            if owner.is_none() { continue; }
                        }
                        change = changes.next() => {
                            if change.is_none() { break; }
                        }
                    }
                    let next = snapshot(&proxy).await?;
                    if next != last {
                        last = next;
                        if output.send(Event::Sharing(last.clone())).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Ok(())
            }
            .await;
            let _ = result;
            if output.send(Event::Sharing(vec![])).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Status(Arc<AtomicUsize>);
    #[zbus::interface(name = "org.ferese.CaptureStatus")]
    impl Status {
        fn get_snapshot(&self) -> String {
            self.0.fetch_add(1, Ordering::Relaxed);
            serde_json::to_string(&vec![Session {
                app: "test".into(),
                sources: vec!["Workspace 2".into()],
            }])
            .unwrap()
        }
        #[zbus(signal)]
        async fn changed(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
    }

    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[tokio::test]
    #[ignore = "requires dbus-daemon; uses a private test bus"]
    async fn portal_owner_changes_recover_without_polling_or_activation() {
        use std::io::BufRead;
        let mut bus = Bus(std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        let events = stream_at(Some(address.clone()));
        tokio::pin!(events);
        let Some(Event::Sharing(sessions)) =
            tokio::time::timeout(Duration::from_secs(2), events.next())
                .await
                .unwrap()
        else {
            panic!("missing initial state");
        };
        assert!(sessions.is_empty());
        let reads = Arc::new(AtomicUsize::new(0));
        let server = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(PATH, Status(reads.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(Event::Sharing(sessions)) = events.next().await
                    && sessions.len() == 1
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let before = reads.load(Ordering::Relaxed);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), events.next())
                .await
                .is_err()
        );
        assert_eq!(
            reads.load(Ordering::Relaxed),
            before,
            "idle subscriber polled the portal"
        );
        let emitter = zbus::object_server::SignalEmitter::new(&server, PATH).unwrap();
        Status::changed(&emitter).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), events.next())
                .await
                .is_err(),
            "unchanged snapshot was republished"
        );
        assert_eq!(reads.load(Ordering::Relaxed), before + 1);
        server.release_name(SERVICE).await.unwrap();
        let Some(Event::Sharing(sessions)) =
            tokio::time::timeout(Duration::from_secs(2), events.next())
                .await
                .unwrap()
        else {
            panic!("missing owner loss");
        };
        assert!(sessions.is_empty());
        server.request_name(SERVICE).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(Event::Sharing(sessions)) = events.next().await
                    && sessions.len() == 1
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
}
