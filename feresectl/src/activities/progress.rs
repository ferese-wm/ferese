//! One replaceable progress update, independent of the child's stdout reader.
use super::{Id, OwnerScope, Publish, State, Value, publish_with_timeout};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

#[derive(Default)]
struct Pending {
    units: Option<u64>,
    end: Option<State>,
    closed: bool,
}

#[derive(Clone, Default)]
pub(super) struct Updates(Arc<(Mutex<Pending>, Condvar)>);
impl Updates {
    pub fn progress(&self, units: u64) {
        let (lock, wake) = &*self.0;
        let mut pending = lock.lock().unwrap();
        if !pending.closed {
            pending.units = Some(units);
            wake.notify_one();
        }
    }
}

pub(super) struct Publisher {
    pub updates: Updates,
    finished: mpsc::Receiver<()>,
}
impl Publisher {
    pub fn new(socket: PathBuf, local: String, handle: Id) -> Self {
        let updates = Updates::default();
        let worker = updates.clone();
        let (done, finished) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            loop {
                let (lock, wake) = &*worker.0;
                let mut pending = lock.lock().unwrap();
                while pending.units.is_none() && pending.end.is_none() {
                    pending = wake.wait(pending).unwrap();
                }
                let end = pending.end.take();
                let units = pending.units.take();
                drop(pending); // Never hold the reader's lock during socket I/O.
                let request = if let Some(result) = end {
                    Publish::End {
                        local: local.clone(),
                        handle: Some(handle.clone()),
                        result,
                        message: None,
                    }
                } else {
                    Publish::Update {
                        local: local.clone(),
                        handle: Some(handle.clone()),
                        value: Some(Value::Text(format!("{} units compiled", units.unwrap()))),
                        lease_ms: None,
                    }
                };
                let timeout = if end.is_some() {
                    Duration::from_secs(2)
                } else {
                    Duration::from_millis(200)
                };
                let _ = publish_with_timeout(&socket, OwnerScope::Publisher, request, timeout);
                if end.is_some() {
                    break;
                }
            }
            let _ = done.send(());
        });
        Self { updates, finished }
    }

    pub fn finish(self, result: State) {
        let (lock, wake) = &*self.updates.0;
        {
            let mut pending = lock.lock().unwrap();
            pending.closed = true;
            pending.units = None;
            pending.end = Some(result);
            wake.notify_one();
        }
        // Completion has a fixed budget even if the broker stops responding.
        let _ = self.finished.recv_timeout(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferese_ipc::{Request, Response, read_frame, write_frame};
    use std::os::unix::net::UnixListener;

    #[test]
    fn blocked_publication_coalesces_updates_and_completion_is_last() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("broker.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (seen, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            first
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let request: Request = read_frame(&mut first).unwrap();
            seen.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(1)).unwrap();
            // The progress timeout can already have elapsed on a loaded host.
            let _ = write_frame(
                &mut first,
                &Response::success(request.id, serde_json::json!({})),
            );
            let (mut latest, _) = listener.accept().unwrap();
            let request: Request = read_frame(&mut latest).unwrap();
            let wire: ferese_ipc::activities::Request =
                serde_json::from_value(request.args).unwrap();
            assert!(
                matches!(wire.publish, Publish::Update { value: Some(Value::Text(ref text)), .. } if text == "1000 units compiled")
            );
            write_frame(
                &mut latest,
                &Response::success(request.id, serde_json::json!({})),
            )
            .unwrap();
            seen.send(()).unwrap();
            let (mut end, _) = listener.accept().unwrap();
            let request: Request = read_frame(&mut end).unwrap();
            let wire: ferese_ipc::activities::Request =
                serde_json::from_value(request.args).unwrap();
            assert!(matches!(
                wire.publish,
                Publish::End {
                    result: State::Done,
                    ..
                }
            ));
            write_frame(
                &mut end,
                &Response::success(request.id, serde_json::json!({})),
            )
            .unwrap();
        });
        let publisher = Publisher::new(
            socket,
            "test".into(),
            Id {
                owner: ferese_activities::Owner { pid: 1, birth: 1 },
                local: "test".into(),
                session: 1,
                generation: 1,
            },
        );
        publisher.updates.progress(1);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        for units in 2..=1000 {
            publisher.updates.progress(units);
        }
        release.send(()).unwrap();
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        publisher.finish(State::Done);
        server.join().unwrap();
    }
}
