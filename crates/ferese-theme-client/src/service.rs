use cosmic::iced::futures::SinkExt;
use ferese_ipc::theme::Snapshot;

pub fn current() -> Snapshot {
    ferese_ipc::Connection::connect()
        .ok()
        .and_then(|mut connection| connection.get(ferese_config::families::builtins).ok())
        .unwrap_or_else(fallback)
}

/// Standalone appearance; it does not represent compositor acceptance.
pub fn fallback() -> Snapshot {
    let theme = ferese_config::theme::default_theme();
    Snapshot {
        version: ferese_ipc::theme::SCHEMA_VERSION,
        revision: 0,
        mode: ferese_config::theme::Mode::Dark,
        presented: theme.clone(),
        theme,
        warnings: Vec::new(),
        error: None,
        families: ferese_config::families::builtins(),
        fallback_note: None,
    }
}

pub fn subscription() -> cosmic::iced::Subscription<Snapshot> {
    cosmic::iced::Subscription::run(stream)
}

fn stream() -> impl cosmic::iced::futures::Stream<Item = Snapshot> {
    cosmic::iced::stream::channel(1, async |mut output| {
        loop {
            let connection = tokio::task::spawn_blocking(ferese_ipc::Connection::connect).await;
            let Ok(Ok(mut connection)) = connection else {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            };
            let Ok(cancellation) = connection.cancellation() else {
                return;
            };
            let (send, mut receive) = tokio::sync::watch::channel(None);
            tokio::task::spawn_blocking(move || {
                let Ok(mut snapshot) = connection.get(ferese_config::families::builtins) else {
                    return;
                };
                loop {
                    let revision = snapshot.revision;
                    if send.send(Some(snapshot)).is_err() {
                        return;
                    }
                    match connection.watch(revision, ferese_config::families::builtins) {
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
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::futures::StreamExt;
    use ferese_ipc::{Request, Response, read_frame, write_frame};
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    #[test]
    fn subscription_reconnects_and_drop_cancels_a_pending_watch() {
        const CHILD: &str = "FERESE_THEME_SUBSCRIPTION_TEST";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "service::tests::subscription_reconnects_and_drop_cancels_a_pending_watch",
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
        // No compositor: return the same standalone values without connecting elsewhere.
        assert_eq!(current(), fallback());
        let path = ferese_ipc::socket_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(path).unwrap();
        let (watch_send, watch_receive) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for revision in [1, 2] {
                let (mut connection, _) = listener.accept().unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let get: Request = read_frame(&mut connection).unwrap();
                assert_eq!(get.command, "theme-get");
                let mut snapshot = fallback();
                snapshot.revision = revision;
                write_frame(
                    &mut connection,
                    &Response::success(get.id, serde_json::to_value(snapshot).unwrap()),
                )
                .unwrap();
                let watch: Request = read_frame(&mut connection).unwrap();
                assert_eq!(watch.command, "theme-watch");
                assert_eq!(watch.args["since"], revision);
                if revision == 2 {
                    watch_send.send(()).unwrap();
                    use std::io::Read;
                    // EOF, not the read deadline: dropping the stream must shut down the watch.
                    assert_eq!(connection.read(&mut [0]).unwrap(), 0);
                }
                // First connection closes while watched; the stream must reconnect.
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut subscription = Box::pin(stream());
            for revision in [1, 2] {
                let snapshot = tokio::time::timeout(Duration::from_secs(5), subscription.next())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(snapshot.revision, revision);
            }
            tokio::task::spawn_blocking(move || {
                watch_receive.recv_timeout(Duration::from_secs(5)).unwrap()
            })
            .await
            .unwrap();
            drop(subscription);
        });
        server.join().unwrap();
    }
}
