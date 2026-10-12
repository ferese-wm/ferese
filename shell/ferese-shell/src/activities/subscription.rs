use std::sync::Arc;
use std::time::Duration;

use cosmic::iced::Subscription;
use cosmic::iced::futures::SinkExt;
use ferese_ipc::activities::Snapshot;
use serde_json::json;

pub(crate) fn subscription() -> Subscription<Arc<Snapshot>> {
    Subscription::run(stream)
}

fn stream() -> impl cosmic::iced::futures::Stream<Item = Arc<Snapshot>> {
    cosmic::iced::stream::channel(1, async |mut output| {
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
                let Ok(value) = connection.call("activity-get", json!({})) else {
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
                        .wait("activity-watch", json!({"since":revision}))
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
