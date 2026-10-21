use std::path::Path;
use std::time::Duration;

use cosmic::iced::futures::{SinkExt, Stream};
use notify::{RecursiveMode, Watcher};

use crate::Message;
use crate::store::Snapshot;

pub fn changes(path: &Path) -> impl Stream<Item = Message> + use<> {
    let path = path.to_path_buf();

    cosmic::iced::stream::channel(1, async move |mut output| {
        let path = match std::path::absolute(path) {
            Ok(path) => path,
            Err(error) => {
                let _ = output
                    .send(Message::ExternalConfig(Err(error.to_string())))
                    .await;
                return;
            }
        };
        let (send, mut events) = tokio::sync::mpsc::channel(1);
        let target = path.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && !event.kind.is_access()
                && event.paths.iter().any(|p| p == &target)
            {
                let _ = send.try_send(());
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                let _ = output
                    .send(Message::ExternalConfig(Err(error.to_string())))
                    .await;
                return;
            }
        };

        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // On a fresh install the directory may not exist until the first save.
        // Install its watch now so later saves do not leave this subscription dead.
        let watch = std::fs::create_dir_all(directory)
            .map_err(notify::Error::io)
            .and_then(|()| watcher.watch(directory, RecursiveMode::NonRecursive));
        if let Err(error) = watch {
            let _ = output
                .send(Message::ExternalConfig(Err(error.to_string())))
                .await;
            return;
        }

        // Read once after installing the watch to cover the startup race.
        loop {
            let file = path.clone();
            let snapshot = tokio::task::spawn_blocking(move || Snapshot::read(&file))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            if output
                .send(Message::ExternalConfig(snapshot))
                .await
                .is_err()
            {
                return;
            }
            if events.recv().await.is_none() {
                return;
            }
            loop {
                match tokio::time::timeout(Duration::from_millis(150), events.recv()).await {
                    Ok(Some(())) => (),
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use cosmic::iced::futures::{StreamExt, pin_mut};

    use super::*;

    #[test]
    fn watches_first_save_when_the_config_directory_does_not_exist() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let directory = tempfile::tempdir_in(".").unwrap();
                let path = directory.path().join("ferese/config.kdl");
                let stream = changes(&path);
                pin_mut!(stream);
                assert!(matches!(
                    stream.next().await,
                    Some(Message::ExternalConfig(Ok(_)))
                ));
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let source = "animations { speed 0.75; }";
                std::fs::write(&path, source).unwrap();
                match tokio::time::timeout(Duration::from_secs(3), stream.next())
                    .await
                    .unwrap()
                    .unwrap()
                {
                    Message::ExternalConfig(Ok(snapshot)) => assert_eq!(snapshot.source, source),
                    _ => panic!("first save was not observed"),
                }
            });
    }

    #[test]
    fn watches_atomic_replacement_and_retains_subscription_after_bad_kdl() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("config.kdl");
                std::fs::write(&path, "animations {\n    speed 1\n}\n").unwrap();
                let stream = changes(&path);
                pin_mut!(stream);
                assert!(matches!(
                    stream.next().await,
                    Some(Message::ExternalConfig(Ok(_)))
                ));
                for source in ["bad [", "animations {\n    speed 0.75\n}\n"] {
                    let temporary = directory.path().join("replacement");
                    std::fs::write(&temporary, source).unwrap();
                    std::fs::rename(temporary, &path).unwrap();
                    let message = tokio::time::timeout(Duration::from_secs(3), stream.next())
                        .await
                        .unwrap()
                        .unwrap();
                    match message {
                        Message::ExternalConfig(Ok(snapshot)) => {
                            assert_eq!(snapshot.source, source)
                        }
                        Message::ExternalConfig(Err(_)) => assert_eq!(source, "bad ["),
                        _ => panic!("unexpected watcher message"),
                    }
                }
            });
    }
}
