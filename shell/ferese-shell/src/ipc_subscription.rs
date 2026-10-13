use std::sync::Arc;
use std::time::Duration;

use cosmic::iced::futures::{SinkExt, Stream};
use serde::de::DeserializeOwned;
use serde_json::json;

/// Follow a versioned snapshot, clearing stale content when its connection ends.
pub(crate) fn stream<S>(get: &'static str, watch: &'static str, revision: fn(&S) -> u64) -> impl Stream<Item = Arc<S>>
where
    S: DeserializeOwned + Default + Send + Sync + 'static,
{
    cosmic::iced::stream::channel(1, async move |mut output| {
        loop {
            let connection = tokio::task::spawn_blocking(ferese_ipc::Connection::connect).await;
            let Ok(Ok(mut connection)) = connection else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            let Ok(cancellation) = connection.cancellation() else {
                return;
            };
            let (send, mut receive) = tokio::sync::watch::channel(None);

            tokio::task::spawn_blocking(move || {
                let Ok(value) = connection.call(get, json!({})) else {
                    return;
                };
                let Ok(mut snapshot) = serde_json::from_value::<S>(value) else {
                    return;
                };

                loop {
                    let since = revision(&snapshot);

                    if send.send(Some(Arc::new(snapshot))).is_err() {
                        return;
                    }

                    match connection
                        .wait(watch, json!({"since": since}))
                        .and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string()))
                    {
                        Ok(next) => snapshot = next,
                        Err(_) => return,
                    }
                }
            });

            while receive.changed().await.is_ok() {
                let snapshot = receive.borrow_and_update().clone();
                if let Some(snapshot) = snapshot
                    && output.send(snapshot).await.is_err()
                {
                    return;
                }
            }

            drop(cancellation);

            if output.send(Arc::new(S::default())).await.is_err() {
                return;
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::futures::StreamExt;
    use ferese_ipc::{Request, Response, read_frame, write_frame};
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    #[test]
    fn media_and_activities_reconnect_clear_stale_state_and_cancel_watches() {
        const CHILD: &str = "FERESE_SNAPSHOT_SUBSCRIPTION_TEST";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ipc_subscription::tests::media_and_activities_reconnect_clear_stale_state_and_cancel_watches",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("XDG_RUNTIME_DIR", root.path())
                .env_remove("FERESE_SOCKET")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        check_stream::<ferese_ipc::media::Snapshot>("media-get", "media-watch", |snapshot| snapshot.revision);
        check_stream::<ferese_ipc::activities::Snapshot>("activity-get", "activity-watch", |snapshot| {
            snapshot.revision
        });
    }

    fn check_stream<S>(get: &'static str, watch: &'static str, revision: fn(&S) -> u64)
    where
        S: DeserializeOwned + serde::Serialize + Default + Send + Sync + 'static,
    {
        let path = ferese_ipc::socket_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (consumed, consumed_receive) = std::sync::mpsc::channel();
        let (watch_send, watch_receive) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for initial in [1, 3] {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut connection = loop {
                    match listener.accept() {
                        Ok((connection, _)) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "subscription did not reconnect");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                connection.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
                connection.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
                let request: Request = read_frame(&mut connection).unwrap();
                assert_eq!(request.command, get);
                let mut snapshot = serde_json::to_value(S::default()).unwrap();
                snapshot["revision"] = json!(initial);
                write_frame(&mut connection, &Response::success(request.id, snapshot.clone())).unwrap();
                let request: Request = read_frame(&mut connection).unwrap();
                assert_eq!(request.command, watch);
                assert_eq!(request.args["since"], initial);
                if initial == 1 {
                    // Wait for delivery so watch-channel coalescing cannot skip this snapshot.
                    assert_eq!(consumed_receive.recv_timeout(Duration::from_secs(8)).unwrap(), 1);
                    snapshot["revision"] = json!(2);
                    write_frame(&mut connection, &Response::success(request.id, snapshot)).unwrap();
                    let next: Request = read_frame(&mut connection).unwrap();
                    assert_eq!(next.command, watch);
                    assert_eq!(next.args["since"], 2);
                    assert_ne!(request.id, next.id);
                    assert_eq!(consumed_receive.recv_timeout(Duration::from_secs(8)).unwrap(), 2);
                    // Dropping this connection must clear the old snapshot before retrying.
                } else {
                    watch_send.send(()).unwrap();
                    assert_eq!(
                        connection.read(&mut [0]).unwrap(),
                        0,
                        "dropped subscription kept its socket alive"
                    );
                }
            }
        });
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let mut subscription = Box::pin(stream::<S>(get, watch, revision));
                for expected in [1, 2, 0, 3] {
                    let snapshot = tokio::time::timeout(Duration::from_secs(8), subscription.next())
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(revision(&snapshot), expected);
                    if expected == 1 || expected == 2 {
                        consumed.send(expected).unwrap();
                    }
                }
                tokio::task::spawn_blocking(move || watch_receive.recv_timeout(Duration::from_secs(2)).unwrap())
                    .await
                    .unwrap();
                drop(subscription);
            });
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
