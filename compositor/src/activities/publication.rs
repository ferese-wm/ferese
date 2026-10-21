//! Bounded serialization and delivery outside the compositor event loop.
use ferese_ipc::Response;
use ferese_ipc::activities::Snapshot;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, RwLock};

type Recipient = (u64, SyncSender<Response>);
struct Job {
    epoch: u64,
    snapshot: Arc<Snapshot>,
    recipients: Vec<Recipient>,
}
pub(super) struct Publisher(SyncSender<Job>, Arc<RwLock<u64>>);
impl Publisher {
    pub fn new() -> std::io::Result<Self> {
        let (send, receive) = mpsc::sync_channel::<Job>(8);
        let epoch = Arc::new(RwLock::new(0));
        let worker_epoch = epoch.clone();
        std::thread::Builder::new()
            .name("ferese-activity-snapshots".into())
            .spawn(move || {
                let mut cached: Option<(Arc<Snapshot>, serde_json::Value)> = None;
                while let Ok(job) = receive.recv() {
                    if !cached
                        .as_ref()
                        .is_some_and(|(snapshot, _)| Arc::ptr_eq(snapshot, &job.snapshot))
                    {
                        cached = Some((
                            job.snapshot.clone(),
                            serde_json::json!(job.snapshot.as_ref()),
                        ));
                    }
                    let value = &cached.as_ref().unwrap().1;
                    // Lock acquisition revokes queued/in-flight serialization.
                    // Hold the guard through delivery to order sends before revocation.
                    deliver(job.epoch, &worker_epoch, job.recipients, value);
                }
            })?;
        Ok(Self(send, epoch))
    }

    pub fn revoke(&self) {
        let mut epoch = self.1.write().expect("activity publication epoch");
        *epoch = epoch.wrapping_add(1);
    }

    pub fn send(&self, snapshot: Arc<Snapshot>, recipients: Vec<Recipient>) {
        let epoch = *self.1.read().expect("activity publication epoch");
        if let Err(error) = self.0.try_send(Job {
            epoch,
            snapshot,
            recipients,
        }) {
            let (mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job)) = error;
            unavailable(job.recipients);
        }
    }
}
fn deliver(
    epoch: u64,
    current: &RwLock<u64>,
    recipients: Vec<Recipient>,
    value: &serde_json::Value,
) {
    let current = current.read().expect("activity publication epoch");
    if epoch != *current {
        locked(recipients);
        return;
    }
    for (id, sender) in recipients {
        // A slow/disconnected client cannot block another subscriber.
        let _ = sender.try_send(Response::success(id, value.clone()));
    }
}
pub(super) fn locked(recipients: Vec<Recipient>) {
    for (id, sender) in recipients {
        let _ = sender.try_send(Response::error(id, "session_locked", "Session is locked"));
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
    fn lock_revokes_serialized_jobs_even_after_unlock() {
        let (send, _receive) = mpsc::sync_channel(1);
        let publisher = Publisher(send, Arc::new(RwLock::new(0)));
        let job_epoch = *publisher.1.read().unwrap();
        publisher.revoke();
        let (reply, receive) = mpsc::sync_channel(1);
        deliver(
            job_epoch,
            &publisher.1,
            vec![(9, reply)],
            &serde_json::json!({"title": "private"}),
        );
        let response = receive.recv().unwrap();
        assert_eq!(response.id, 9);
        assert!(response.result.is_none());
        assert_eq!(response.error.unwrap().code, "session_locked");
    }

    #[test]
    fn saturated_queue_rejects_without_blocking_and_slow_clients_do_not_block_others() {
        let (send, _receive) = mpsc::sync_channel(0);
        let publisher = Publisher(send, Arc::new(RwLock::new(0)));
        let (reply, receive) = mpsc::sync_channel(1);
        publisher.send(Arc::new(Snapshot::default()), vec![(7, reply)]);
        let response = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(response.id, 7);
        assert_eq!(response.error.unwrap().code, "activity_busy");

        let publisher = Publisher::new().unwrap();
        let (slow, _slow_receive) = mpsc::sync_channel(1);
        slow.send(Response::success(1, serde_json::json!({})))
            .unwrap();
        let (fast, fast_receive) = mpsc::sync_channel(1);
        publisher.send(Arc::new(Snapshot::default()), vec![(2, slow), (3, fast)]);
        assert_eq!(
            fast_receive
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .id,
            3
        );
    }
}
