//! Bounded serialization and delivery outside the compositor event loop.
use ferese_ipc::{Response, activities::Snapshot};
use std::sync::{
    Arc,
    mpsc::{self, SyncSender},
};

type Recipient = (u64, SyncSender<Response>);
struct Job {
    snapshot: Arc<Snapshot>,
    recipients: Vec<Recipient>,
}
pub(super) struct Publisher(SyncSender<Job>);
impl Publisher {
    pub fn new() -> std::io::Result<Self> {
        let (send, receive) = mpsc::sync_channel::<Job>(8);
        std::thread::Builder::new()
            .name("ferese-activity-snapshots".into())
            .spawn(move || {
                let mut cached: Option<(Arc<Snapshot>, serde_json::Value)> = None;
                while let Ok(job) = receive.recv() {
                    if !cached
                        .as_ref()
                        .is_some_and(|(snapshot, _)| Arc::ptr_eq(snapshot, &job.snapshot))
                    {
                        cached = Some((job.snapshot.clone(), serde_json::json!(job.snapshot.as_ref())));
                    }
                    let value = &cached.as_ref().unwrap().1;
                    for (id, sender) in job.recipients {
                        // Client queues are bounded too. A slow/disconnected client
                        // cannot block serialization or another subscriber.
                        let _ = sender.try_send(Response::success(id, value.clone()));
                    }
                }
            })?;
        Ok(Self(send))
    }

    pub fn send(&self, snapshot: Arc<Snapshot>, recipients: Vec<Recipient>) {
        if let Err(error) = self.0.try_send(Job { snapshot, recipients }) {
            let (mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job)) = error;
            unavailable(job.recipients);
        }
    }
}
pub(super) fn unavailable(recipients: Vec<Recipient>) {
    for (id, sender) in recipients {
        let _ = sender.try_send(Response::error(
            id,
            "activity_busy",
            "Activity snapshot delivery unavailable; retry",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn saturated_queue_rejects_without_blocking_and_slow_clients_do_not_block_others() {
        let (send, _receive) = mpsc::sync_channel(0);
        let publisher = Publisher(send);
        let (reply, receive) = mpsc::sync_channel(1);
        publisher.send(Arc::new(Snapshot::default()), vec![(7, reply)]);
        let response = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(response.id, 7);
        assert_eq!(response.error.unwrap().code, "activity_busy");

        let publisher = Publisher::new().unwrap();
        let (slow, _slow_receive) = mpsc::sync_channel(1);
        slow.send(Response::success(1, serde_json::json!({}))).unwrap();
        let (fast, fast_receive) = mpsc::sync_channel(1);
        publisher.send(Arc::new(Snapshot::default()), vec![(2, slow), (3, fast)]);
        assert_eq!(fast_receive.recv_timeout(Duration::from_secs(1)).unwrap().id, 3);
    }
}
