//! One in-flight decode and one replaceable request across wallpaper states.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use super::pixels::Pixels;

struct Request {
    path: PathBuf,
    result: mpsc::Sender<Result<Pixels, String>>,
    wakeup: Option<smithay::reexports::calloop::LoopSignal>,
}

#[derive(Default)]
struct Queue {
    pending: Option<Request>,
    closed: bool,
}

pub(super) struct Decoder {
    shared: Arc<(Mutex<Queue>, Condvar)>,
}

impl Decoder {
    pub fn new(load: impl Fn(&Path) -> Result<Pixels, String> + Send + 'static) -> Arc<Self> {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("ferese-wallpaper".into())
            .spawn(move || {
                loop {
                    let (queue, ready) = &*worker;
                    let mut queue = ready
                        .wait_while(queue.lock().unwrap(), |queue| !queue.closed && queue.pending.is_none())
                        .unwrap();
                    if queue.closed {
                        break;
                    }

                    let request = queue.pending.take().unwrap();
                    drop(queue);
                    let _ = request.result.send(load(&request.path));
                    if let Some(wakeup) = request.wakeup {
                        wakeup.wakeup();
                    }
                }
            })
            .expect("start wallpaper decoder");
        Arc::new(Self { shared })
    }

    pub fn submit(
        &self,
        path: PathBuf,
        wakeup: Option<smithay::reexports::calloop::LoopSignal>,
    ) -> mpsc::Receiver<Result<Pixels, String>> {
        let (result, receiver) = mpsc::channel();
        let (queue, ready) = &*self.shared;
        queue.lock().unwrap().pending = Some(Request { path, result, wakeup });
        ready.notify_one();
        receiver
    }

    pub fn cancel_pending(&self) {
        self.shared.0.lock().unwrap().pending = None;
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let (queue, ready) = &*self.shared;
        let mut queue = queue.lock().unwrap();
        queue.closed = true;
        queue.pending = None;
        ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn replacements_share_one_decode_and_only_the_latest_pending_starts() {
        let (started, events) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let decoder = Decoder::new(move |path| {
            started.send(path.to_owned()).unwrap();
            if path == Path::new("first") {
                held.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            Err("test decode".into())
        });
        let first = decoder.submit("first".into(), None);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            PathBuf::from("first")
        );
        let dropped = decoder.submit("second".into(), None);
        let dropped_too = decoder.submit("third".into(), None);
        let latest = decoder.submit("latest".into(), None);
        assert!(events.try_recv().is_err());
        assert!(matches!(dropped.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
        assert!(matches!(dropped_too.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
        release.send(()).unwrap();
        assert!(first.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            PathBuf::from("latest")
        );
        assert!(latest.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
        assert!(events.try_recv().is_err());
    }
}
