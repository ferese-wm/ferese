use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use base64::Engine;
use cosmic::iced::Subscription;
use cosmic::iced::futures::SinkExt;
use cosmic::widget::image;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const CACHE_SIZE: usize = 16;
const SIDE: u32 = 128;

#[derive(Clone, Debug)]
pub(crate) struct Artwork {
    key: Key,
    pub handle: Option<image::Handle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    uri: String,
    owner: String,
    track_id: Option<String>,
    title: String,
    artist: String,
}

impl Key {
    fn from_player(player: &ferese_ipc::media::Player) -> Option<Self> {
        Some(Self {
            uri: player.art_url.clone()?,
            owner: player.owner.clone(),
            track_id: player.track_id.clone(),
            title: player.title.clone(),
            artist: player.artist.clone(),
        })
    }
}

#[derive(Default)]
struct Queue {
    pending: Option<Key>,
    stopped: bool,
}

pub(crate) struct Service {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    receive: Updates,
    requested: Option<Key>,
}

impl Service {
    pub fn new() -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (send, receive) = tokio::sync::watch::channel(None);
        let worker_queue = queue.clone();
        let _ = std::thread::Builder::new()
            .name("ferese-media-art".into())
            .spawn(move || {
                let agent = ureq::AgentBuilder::new()
                    .redirects(0)
                    .timeout(Duration::from_secs(5))
                    .build();
                work(worker_queue, send, |uri| load(&agent, uri).ok());
            });
        Self {
            queue,
            receive: Updates(Arc::new(receive)),
            requested: None,
        }
    }

    pub fn request(&mut self, player: Option<&ferese_ipc::media::Player>) {
        let key = player.and_then(Key::from_player);
        if self.requested == key {
            return;
        }
        self.requested = key.clone();
        self.queue.0.lock().unwrap().pending = key;
        self.queue.1.notify_one();
    }

    pub fn accepts(&self, art: &Artwork) -> bool {
        self.requested.as_ref() == Some(&art.key)
    }

    pub fn subscription(&self) -> Subscription<Artwork> {
        Subscription::run_with(self.receive.clone(), stream)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().stopped = true;
        self.queue.1.notify_one();
    }
}

fn work(
    queue: Arc<(Mutex<Queue>, Condvar)>,
    send: tokio::sync::watch::Sender<Option<Artwork>>,
    mut load: impl FnMut(&str) -> Option<image::Handle>,
) {
    let mut cache: VecDeque<Artwork> = VecDeque::new();
    loop {
        let (lock, changed) = &*queue;
        let mut state = lock.lock().unwrap();
        while state.pending.is_none() && !state.stopped {
            state = changed.wait(state).unwrap();
        }

        if state.stopped {
            return;
        }
        let key = state.pending.take().unwrap();
        drop(state);
        let artwork = match cache.iter().position(|art| art.key == key) {
            Some(index) => cache.remove(index).unwrap(),
            None => Artwork {
                handle: load(&key.uri),
                key,
            },
        };
        cache.push_front(artwork.clone());
        cache.truncate(CACHE_SIZE);
        if send.send(Some(artwork)).is_err() {
            return;
        }
    }
}

// Receiver identity stays stable while the latest result is replaced.
#[derive(Clone)]
struct Updates(Arc<tokio::sync::watch::Receiver<Option<Artwork>>>);

impl std::hash::Hash for Updates {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

fn stream(receive: &Updates) -> impl cosmic::iced::futures::Stream<Item = Artwork> + use<> {
    let mut receive = receive.0.as_ref().clone();
    cosmic::iced::stream::channel(1, async move |mut output| {
        loop {
            let art = receive.borrow_and_update().clone();
            if let Some(art) = art
                && output.send(art).await.is_err()
            {
                return;
            }
            if receive.changed().await.is_err() {
                return;
            }
        }
    })
}

fn bounded(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Album art exceeds the size limit".into());
    }
    Ok(bytes)
}

fn load(agent: &ureq::Agent, uri: &str) -> Result<image::Handle, String> {
    let url = url::Url::parse(uri).map_err(|e| e.to_string())?;
    let bytes = match url.scheme() {
        "file" => {
            let path = url.to_file_path().map_err(|_| "Invalid album art path")?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
                .map_err(|e| e.to_string())?;
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("Album art is not a regular file".into());
            }
            bounded(file)?
        }
        "http" | "https" => {
            let response = agent.get(uri).call().map_err(|e| e.to_string())?;
            if response.status() != 200 {
                return Err("Album art request failed".into());
            }
            bounded(response.into_reader())?
        }
        "data" => {
            let (metadata, data) = uri.split_once(',').ok_or("Invalid album art data")?;
            if !metadata.starts_with("data:image/")
                || !metadata.ends_with(";base64")
                || data.len() > MAX_BYTES * 4 / 3 + 4
            {
                return Err("Invalid album art data".into());
            }
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| e.to_string())?
        }
        _ => return Err("Unsupported album art URL".into()),
    };
    decode(bytes)
}

fn decode(bytes: Vec<u8>) -> Result<image::Handle, String> {
    let mut reader = image_codec::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image_codec::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| e.to_string())?
        .thumbnail(SIDE, SIDE)
        .to_rgba8();
    Ok(image::Handle::from_rgba(
        image.width(),
        image.height(),
        image.into_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(uri: &str) -> Key {
        Key::from_player(&ferese_ipc::media::Player {
            art_url: Some(uri.into()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn artwork_is_invalidated_when_the_track_changes_or_has_no_art() {
        let mut model = crate::media::Model::default();
        let player = ferese_ipc::media::Player {
            art_url: Some("file:///nonexistent/ferese-media-test.png".into()),
            track_id: Some("/track/one".into()),
            ..Default::default()
        };
        let art = Artwork {
            key: Key::from_player(&player).unwrap(),
            handle: Some(image::Handle::from_rgba(1, 1, vec![255; 4])),
        };
        model.snapshot = Arc::new(ferese_ipc::media::Snapshot {
            selected: Some(player.clone()),
            ..Default::default()
        });
        model.refresh_art(true);
        assert!(model.art.accepts(&art));
        model.artwork = Some(art.clone());
        let mut next = player;
        next.track_id = Some("/track/two".into());
        model.snapshot = Arc::new(ferese_ipc::media::Snapshot {
            selected: Some(next.clone()),
            ..Default::default()
        });
        model.refresh_art(true);
        assert!(!model.art.accepts(&art));
        assert!(model.artwork.is_none());
        model.artwork = Some(art);
        next.art_url = None;
        model.snapshot = Arc::new(ferese_ipc::media::Snapshot {
            selected: Some(next),
            ..Default::default()
        });
        model.refresh_art(true);
        assert!(model.artwork.is_none());
    }

    #[test]
    fn one_decoder_coalesces_replacements_while_an_image_is_loading() {
        let queue = Arc::new((
            Mutex::new(Queue {
                pending: Some(key("first")),
                stopped: false,
            }),
            Condvar::new(),
        ));
        let (send, _receive) = tokio::sync::watch::channel(None);
        let (started, starts) = std::sync::mpsc::channel();
        let (resume, resumes) = std::sync::mpsc::channel();
        let worker_queue = queue.clone();
        let worker = std::thread::spawn(move || {
            work(worker_queue, send, |uri| {
                started.send(uri.to_owned()).unwrap();
                if uri == "first" {
                    resumes.recv().unwrap();
                }
                None
            })
        });
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), "first");
        queue.0.lock().unwrap().pending = Some(key("second"));
        queue.0.lock().unwrap().pending = Some(key("latest"));
        resume.send(()).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), "latest");
        queue.0.lock().unwrap().stopped = true;
        queue.1.notify_one();
        worker.join().unwrap();
        assert!(starts.try_recv().is_err());
    }

    #[test]
    fn encoded_and_decoded_art_have_resource_limits() {
        assert!(bounded(Cursor::new(vec![0; MAX_BYTES + 1])).is_err());
        let mut bytes = Cursor::new(Vec::new());
        image_codec::DynamicImage::new_rgba8(5000, 1)
            .write_to(&mut bytes, image_codec::ImageFormat::Png)
            .unwrap();
        assert!(decode(bytes.into_inner()).is_err());
        assert!(decode(vec![0; 40]).is_err());
    }

    #[test]
    fn unsupported_urls_do_not_fetch() {
        assert!(load(&ureq::Agent::new(), "ftp://example.test/art.png").is_err());
        assert!(load(&ureq::Agent::new(), "data:text/plain;base64,dGVzdA==").is_err());
    }
}
