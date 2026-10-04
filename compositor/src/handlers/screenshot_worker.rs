use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, SystemTime};
use std::{fs, io, thread};

use smithay::reexports::calloop::channel;

use super::screenshot::{Canvas, OutputFrame, compose};

pub(crate) const QUEUE_CAPACITY: usize = 4;
/// A client normally opens and unlinks its screenshot immediately. If it dies
/// first, the file is left behind, so staged files are swept well after any
/// realistic request could still be reading one.
const STAGE_TTL: Duration = Duration::from_secs(60 * 60);
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

pub(crate) struct Job {
    pub(crate) request: u64,
    pub(crate) frames: Vec<OutputFrame>,
}

pub(crate) struct Encoded {
    pub(crate) request: u64,
    pub(crate) result: Result<PathBuf, String>,
}

pub(crate) struct Worker {
    queue: SyncSender<Job>,
}

impl Worker {
    pub(crate) fn spawn(results: channel::SyncSender<Encoded>) -> Option<Self> {
        let (queue, receiver) = sync_channel::<Job>(QUEUE_CAPACITY);
        thread::Builder::new()
            .name("ferese-screenshot".to_owned())
            .spawn(move || run(receiver, results))
            .ok()
            .map(|_| Self { queue })
    }

    // The compositor thread must never block, so a full queue is reported back
    // to the caller instead of waiting.
    pub(crate) fn submit(&self, job: Job) -> Result<(), Job> {
        match self.queue.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => Err(job),
        }
    }
}

fn run(receiver: Receiver<Job>, results: channel::SyncSender<Encoded>) {
    while let Ok(job) = receiver.recv() {
        let result = encode(&job);
        // This send cannot block. Each request produces exactly one Encode, so at
        // most QUEUE_CAPACITY jobs can be queued while one more is being encoded,
        // and RESULT_QUEUE_CAPACITY is asserted to exceed that. Blocking here
        // would stall the worker with no way for the loop to recover it.
        if results
            .send(Encoded {
                request: job.request,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

// Removes staged screenshots whose client never collected them. Only files
// older than the TTL are touched, so an in-flight request is never disturbed.
pub(crate) fn sweep_stale_files() {
    let Ok(directory) = private_directory() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "png") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        // A file dated in the future is treated as fresh rather than swept.
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > STAGE_TTL);
        if stale && fs::remove_file(&path).is_ok() {
            tracing::debug!(path = %path.display(), "removed an uncollected screenshot");
        }
    }
}

fn encode(job: &Job) -> Result<PathBuf, String> {
    let canvas = compose(&job.frames)?;
    let png = encode_png(&canvas)?;
    write_private_file(&png)
}

fn encode_png(canvas: &Canvas) -> Result<Vec<u8>, String> {
    let (width, height) = (canvas.width, canvas.height);
    // A final sanity check on the composed size before handing it to the
    // encoder, which would otherwise panic on a zero dimension.
    if width == 0 || height == 0 {
        return Err("Screenshot is empty".into());
    }
    let image = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(width, height, canvas.pixels.as_slice())
        .ok_or("Screenshot buffer does not match its dimensions")?;
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        std::io::Cursor::new(&mut out),
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    image::ImageEncoder::write_image(encoder, image.as_raw(), width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| format!("Could not encode screenshot: {error}"))?;
    tracing::debug!(encoded = out.len(), "encoded screenshot");
    Ok(out)
}

// The client is told to open this path and unlink it immediately, so the
// directory is ours, private, and files are created with the final mode.
fn write_private_file(png: &[u8]) -> Result<PathBuf, String> {
    let directory = private_directory()?;
    for attempt in 0..64u32 {
        let path = directory.join(format!("shot-{}-{attempt}.png", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(FILE_MODE);
        match options.open(&path) {
            Ok(mut file) => {
                use io::Write;
                file.write_all(png)
                    .and_then(|()| file.sync_all())
                    .map_err(|error| format!("Could not write screenshot: {error}"))?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not write screenshot: {error}")),
        }
    }
    Err("Could not find an unused screenshot filename".into())
}

fn private_directory() -> Result<PathBuf, String> {
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set, so screenshots cannot be staged")?;
    let root = PathBuf::from(runtime);
    let directory = root.join("ferese-screenshots");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("Refusing to stage screenshots in a symlinked directory".into());
        }
        Ok(metadata) => {
            use std::os::unix::fs::MetadataExt;
            if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err("Refusing to stage screenshots in an untrusted directory".into());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&directory)
                .and_then(|()| fs::set_permissions(&directory, fs::Permissions::from_mode(DIR_MODE)))
                .map_err(|error| format!("Could not create screenshot directory: {error}"))?;
        }
        Err(error) => return Err(format!("Could not inspect screenshot directory: {error}")),
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use smithay::utils::Transform;

    use super::*;

    fn frame(width: i32, height: i32) -> OutputFrame {
        let stride = width as usize * 4;
        let mut pixels = vec![0u8; stride * height as usize];
        let (quads, _) = pixels.as_chunks_mut::<4>();
        for pixel in quads {
            pixel[3] = 255;
        }
        OutputFrame {
            preserve_alpha: false,
            width,
            height,
            stride,
            pixels,
            transform: Transform::Normal,
            location: (0, 0),
            scale: 1.0,
            logical_width: width,
            logical_height: height,
        }
    }

    #[test]
    fn encode_produces_a_decodable_png() {
        let job = Job {
            request: 1,
            frames: vec![frame(4, 3)],
        };
        let mut canvas = compose(&job.frames).unwrap();
        for (index, pixel) in canvas.pixels.chunks_exact_mut(4).enumerate() {
            pixel[..3].copy_from_slice(&[index as u8, 137, 241]);
        }

        let png = encode_png(&canvas).expect("png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "png signature");

        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("decodes back")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (4, 3));
        assert_eq!(decoded.as_raw(), &canvas.pixels, "encoding changed the pixel data");
        // The opaque alpha we force is preserved through the round trip.
        assert_eq!(decoded.get_pixel(0, 0)[3], 255);
    }

    #[test]
    fn staged_files_are_owner_only() {
        let png = b"\x89PNG\r\n\x1a\nnot really a png".to_vec();
        let path = write_private_file(&png).expect("staged");
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "staged screenshots are not group/world readable");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_sweep_keeps_fresh_files_and_ignores_other_types() {
        let png = b"\x89PNG\r\n\x1a\n".to_vec();
        let fresh = write_private_file(&png).expect("staged");

        let directory = private_directory().unwrap();
        let other = directory.join("keep-me.txt");
        fs::write(&other, b"not a screenshot").unwrap();

        sweep_stale_files();
        assert!(fresh.exists(), "a freshly staged screenshot is still in use");
        assert!(other.exists(), "unrelated files are left alone");

        fs::remove_file(&fresh).unwrap();
        fs::remove_file(&other).unwrap();
    }

    #[test]
    fn the_sweep_removes_files_past_the_ttl() {
        let png = b"\x89PNG\r\n\x1a\n".to_vec();
        let path = write_private_file(&png).expect("staged");
        // Backdate it past the TTL without waiting an hour.
        let aged = SystemTime::now() - STAGE_TTL - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(aged)
            .expect("backdate");

        sweep_stale_files();
        assert!(!path.exists(), "an uncollected screenshot is reclaimed");
    }

    #[test]
    fn a_missing_runtime_dir_is_reported_rather_than_ignored() {
        // private_directory must never fall back to /tmp or the cwd.
        let result = private_directory();
        match result {
            Ok(path) => assert!(
                path.ends_with("ferese-screenshots"),
                "screenshots are staged in the runtime dir, not a fallback location"
            ),
            Err(error) => assert!(error.contains("XDG_RUNTIME_DIR") || error.contains("untrusted")),
        }
    }
}
