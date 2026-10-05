use std::sync::Arc;
use std::time::Duration;

use cosmic::iced::Subscription;
use cosmic::iced::futures::SinkExt;
use ferese_ipc::media::Snapshot;
use serde_json::json;

pub(super) fn subscription() -> Subscription<Arc<Snapshot>> {
    Subscription::run(stream)
}

fn stream() -> impl cosmic::iced::futures::Stream<Item = Arc<Snapshot>> {
    cosmic::iced::stream::channel(1, async |mut output| {
        loop {
            let connection = tokio::task::spawn_blocking(ferese_ipc::theme::Connection::connect).await;
            let Ok(Ok(mut connection)) = connection else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            let Ok(cancellation) = connection.cancellation() else {
                return;
            };
            let (send, mut receive) = tokio::sync::watch::channel(None);
            tokio::task::spawn_blocking(move || {
                let Ok(value) = connection.call("media-get", json!({})) else {
                    return;
                };
                let Ok(mut snapshot) = serde_json::from_value::<Snapshot>(value) else {
                    return;
                };
                loop {
                    let revision = snapshot.revision;
                    if send.send(Some(Arc::new(snapshot))).is_err() {
                        return;
                    }
                    match connection
                        .wait("media-watch", json!({"since":revision}))
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
            if output.send(Arc::new(Snapshot::default())).await.is_err() {
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
    use std::os::unix::net::UnixListener;

    #[test]
    fn reconnect_clears_ghost_media_and_drop_cancels_a_watch() {
        const CHILD: &str = "FERESE_MEDIA_SUBSCRIPTION_TEST";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "media::subscription::tests::reconnect_clears_ghost_media_and_drop_cancels_a_watch",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("XDG_RUNTIME_DIR", root.path())
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

        let path = ferese_ipc::theme::socket_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(path).unwrap();
        let (watch_send, watch_receive) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for revision in [1, 2] {
                let (mut connection, _) = listener.accept().unwrap();
                connection.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
                let get: Request = read_frame(&mut connection).unwrap();
                assert_eq!(get.command, "media-get");
                let snapshot = Snapshot {
                    revision,
                    ..Default::default()
                };
                write_frame(
                    &mut connection,
                    &Response::success(get.id, serde_json::to_value(snapshot).unwrap()),
                )
                .unwrap();
                let watch: Request = read_frame(&mut connection).unwrap();
                assert_eq!(watch.command, "media-watch");
                assert_eq!(watch.args["since"], revision);
                assert_ne!(get.id, watch.id);
                if revision == 2 {
                    watch_send.send(()).unwrap();
                    use std::io::Read;
                    assert_eq!(connection.read(&mut [0]).unwrap(), 0);
                }
            }
        });
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let mut subscription = Box::pin(stream());
                for revision in [1, 0, 2] {
                    let snapshot = tokio::time::timeout(Duration::from_secs(8), subscription.next())
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(snapshot.revision, revision);
                }

                tokio::task::spawn_blocking(move || watch_receive.recv_timeout(Duration::from_secs(2)).unwrap())
                    .await
                    .unwrap();
                drop(subscription);
            });
        server.join().unwrap();
    }
}
