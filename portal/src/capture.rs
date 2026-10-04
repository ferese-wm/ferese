//! Bounded, cancellable Wayland capture. No screenshots or external capture tools.
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferese_protocols::window_capture::v1::client::ferese_window_capture_manager_v1 as window_manager;
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as manager,
};

pub const BUSY: &str = "Window capture snapshot pool is busy";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub label: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: i32,
}

impl Source {
    pub fn window_id(&self) -> Option<u64> {
        self.name.strip_prefix("window:")?.parse().ok()
    }
}

struct Output {
    global: u32,
    proxy: wl_output::WlOutput,
    source: Source,
    transform: wl_output::Transform,
}

pub struct Frame {
    pub logical_size: Option<(u32, u32)>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixels: Vec<u8>,
}

struct Buffer {
    _file: File,
    map: MmapMut,
    proxy: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

#[derive(Default)]
struct State {
    outputs: Vec<Output>,
    manager: Option<manager::ZwlrScreencopyManagerV1>,
    window_manager: Option<window_manager::FereseWindowCaptureManagerV1>,
    shm: Option<wl_shm::WlShm>,
    buffer: Option<Buffer>,
    result: Option<Result<(), String>>,
    flipped: bool,
    logical_size: Option<(u32, u32)>,
    busy: bool,
}

pub struct Capture {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    selected_output: Option<u32>,
}

impl Capture {
    pub fn connect(stop: &AtomicBool) -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|e| e.to_string())?;
        Self::connect_with_connection(connection, stop)
    }

    fn connect_with_connection(connection: Connection, stop: &AtomicBool) -> Result<Self, String> {
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut this = Self {
            connection,
            queue,
            state: State::default(),
            selected_output: None,
        };
        // Two syncs deliver registry bindings and output metadata without an
        // uninterruptible roundtrip on a stalled compositor.
        this.sync(stop)?;
        this.sync(stop)?;
        if (this.state.manager.is_none() && this.state.window_manager.is_none()) || this.state.shm.is_none() {
            return Err("Ferese screen capture is unavailable in this session".into());
        }
        Ok(this)
    }

    fn sync(&mut self, stop: &AtomicBool) -> Result<(), String> {
        let done = Arc::new(AtomicBool::new(false));
        self.connection.display().sync(&self.queue.handle(), done.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::Relaxed) {
            self.dispatch(stop, deadline)?;
        }
        Ok(())
    }

    pub fn supports_windows(&self) -> bool {
        self.state.window_manager.is_some()
    }

    pub fn generation(&self, name: &str) -> Option<u32> {
        self.state
            .outputs
            .iter()
            .find(|output| output.source.name == name)
            .map(|output| output.global)
    }

    pub fn pin_output(&mut self, name: &str, generation: u32) -> Result<(), String> {
        if self.generation(name) != Some(generation) {
            return Err("Shared monitor was replaced".into());
        }
        self.selected_output = Some(generation);
        Ok(())
    }

    pub fn validate_sources(&mut self, selected: &[(String, u32)], stop: &AtomicBool) -> Result<(), String> {
        self.sync(stop)?;
        for (name, generation) in selected {
            if self.generation(name) != Some(*generation) {
                return Err("A selected display disconnected during approval".into());
            }
        }
        Ok(())
    }

    pub fn sources(&self) -> Vec<Source> {
        self.state
            .outputs
            .iter()
            .map(|o| {
                let mut source = o.source.clone();
                if swaps_axes(o.transform) {
                    std::mem::swap(&mut source.width, &mut source.height);
                }
                source
            })
            .collect()
    }

    pub fn frame(&mut self, name: &str, cursor: bool, stop: &AtomicBool, mut pixels: Vec<u8>) -> Result<Frame, String> {
        self.state.result = None;
        self.state.flipped = false;
        self.state.logical_size = None;
        self.state.busy = false;
        let (frame, transform) = if let Some(id) = name.strip_prefix("window:").and_then(|id| id.parse::<u64>().ok()) {
            let manager = self
                .state
                .window_manager
                .as_ref()
                .ok_or("Window capture is unavailable")?;
            (
                manager.capture_window((id >> 32) as u32, id as u32, cursor as u32, &self.queue.handle(), ()),
                wl_output::Transform::Normal,
            )
        } else {
            let output = self
                .state
                .outputs
                .iter()
                .find(|o| o.source.name == name)
                .ok_or("Shared monitor disconnected")?;
            if self.selected_output.is_some_and(|selected| selected != output.global) {
                return Err("Shared monitor was replaced".into());
            }
            self.selected_output = Some(output.global);
            let manager = self.state.manager.as_ref().ok_or("Monitor capture is unavailable")?;
            (
                manager.capture_output(cursor as i32, &output.proxy, &self.queue.handle(), ()),
                output.transform,
            )
        };
        let deadline = Instant::now() + Duration::from_secs(5);

        while self.state.result.is_none() {
            if let Err(error) = self.dispatch(stop, deadline) {
                frame.destroy();
                return Err(error);
            }
        }

        frame.destroy();
        self.state.result.take().unwrap()?;

        let buffer = self.state.buffer.as_ref().ok_or("Capture supplied no buffer")?;
        let (width, height) = copy_oriented_pixels(
            &buffer.map,
            buffer.width,
            buffer.height,
            buffer.stride,
            transform,
            self.state.flipped,
            &mut pixels,
        );

        Ok(Frame {
            logical_size: self.state.logical_size,
            width,
            height,
            stride: width * 4,
            pixels,
        })
    }

    fn dispatch(&mut self, stop: &AtomicBool, deadline: Instant) -> Result<(), String> {
        if stop.load(Ordering::Relaxed) {
            return Err("Capture cancelled".into());
        }

        if Instant::now() >= deadline {
            return Err("Compositor capture timed out".into());
        }

        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| e.to_string())?;
        self.connection.flush().map_err(|e| e.to_string())?;

        if let Some(read) = self.queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: read.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: a single valid pollfd, bounded timeout; Wayland owns the FD.
            let ready = unsafe { libc::poll(&mut fd, 1, 20) };

            if ready > 0 {
                read.read().map_err(|e| e.to_string())?;
            } else if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }

        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_output" => state.outputs.push(Output {
                    global: name,
                    transform: wl_output::Transform::Normal,
                    proxy: registry.bind(name, version.min(4), qh, name),
                    source: Source {
                        name: format!("output-{name}"),
                        label: "Display".into(),
                        width: 0,
                        height: 0,
                        x: 0,
                        y: 0,
                        scale: 1,
                    },
                }),
                "ferese_window_capture_manager_v1" => state.window_manager = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => state.manager = Some(registry.bind(name, version.min(3), qh, ())),
                _ => (),
            },
            wl_registry::Event::GlobalRemove { name } => state.outputs.retain(|o| o.global != name),
            _ => (),
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.iter_mut().find(|o| o.global == *name) else {
            return;
        };

        match event {
            wl_output::Event::Name { name } => output.source.name = name,
            wl_output::Event::Description { description } => output.source.label = description,
            wl_output::Event::Geometry { x, y, transform, .. } => {
                if let WEnum::Value(transform) = transform {
                    output.transform = transform;
                }
                output.source.x = x;
                output.source.y = y;
            }
            wl_output::Event::Scale { factor } => output.source.scale = factor.max(1),
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                output.source.width = width;
                output.source.height = height;
            }
            _ => (),
        }
    }
}

impl Dispatch<frame::ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &frame::ZwlrScreencopyFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                let result = (|| {
                    if format != wl_shm::Format::Argb8888 && format != wl_shm::Format::Xrgb8888 {
                        return Err("Unsupported capture pixel format".into());
                    }

                    let bytes = buffer_size(width, height, stride)?;
                    if let Some(buffer) = &state.buffer
                        && (buffer.width, buffer.height, buffer.stride, buffer.format)
                            == (width, height, stride, format)
                    {
                        frame.copy(&buffer.proxy);
                        return Ok(());
                    }
                    // SAFETY: fixed NUL-terminated name and a newly owned memfd.
                    let fd = unsafe { libc::memfd_create(c"ferese-screencast".as_ptr(), libc::MFD_CLOEXEC) };

                    if fd < 0 {
                        return Err(std::io::Error::last_os_error().to_string());
                    }

                    let file = unsafe { File::from_raw_fd(fd) };
                    file.set_len(bytes as u64).map_err(|e| e.to_string())?;
                    // SAFETY: the file has the checked mapping length and lives with the mapping.
                    let map = unsafe { MmapMut::map_mut(&file) }.map_err(|e| e.to_string())?;
                    let pool = state.shm.as_ref().ok_or("Missing shared memory global")?.create_pool(
                        file.as_fd(),
                        bytes as i32,
                        qh,
                        (),
                    );
                    let proxy = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());

                    pool.destroy();
                    frame.copy(&proxy);
                    state.buffer = Some(Buffer {
                        _file: file,
                        map,
                        proxy,
                        width,
                        height,
                        stride,
                        format,
                    });

                    Ok::<(), String>(())
                })();

                if let Err(e) = result {
                    state.result = Some(Err(e));
                }
            }
            frame::Event::Ready { .. } => state.result = Some(Ok(())),
            frame::Event::Failed => {
                state.result = Some(Err(if state.busy {
                    BUSY.into()
                } else {
                    "Capture ended: session locked, source closed, or compositor unavailable".into()
                }))
            }
            frame::Event::Flags {
                flags: WEnum::Value(flags),
            } => state.flipped = flags.contains(frame::Flags::YInvert),
            _ => (),
        }
    }
}

fn swaps_axes(transform: wl_output::Transform) -> bool {
    matches!(
        transform,
        wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270
    )
}

// Screencopy supplies output-buffer coordinates. Normalize both the output's
// presentation transform and the independent YInvert storage flag before
// publishing upright, tightly packed BGRx frames to PipeWire.
fn copy_oriented_pixels(
    source: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    transform: wl_output::Transform,
    y_invert: bool,
    pixels: &mut Vec<u8>,
) -> (u32, u32) {
    use wl_output::Transform;
    let (out_width, out_height) = if swaps_axes(transform) {
        (height, width)
    } else {
        (width, height)
    };
    let row = out_width as usize * 4;
    pixels.resize(row * out_height as usize, 0);

    // The usual DRM and nested outputs need only one contiguous copy per row.
    if matches!(transform, Transform::Normal | Transform::Flipped180) {
        let flip = y_invert ^ (transform == Transform::Flipped180);
        for (y, dst) in pixels.chunks_exact_mut(row).enumerate() {
            let sy = if flip { height as usize - 1 - y } else { y };
            let start = sy * stride as usize;
            dst.copy_from_slice(&source[start..start + row]);
        }
        return (out_width, out_height);
    }

    for y in 0..out_height {
        for x in 0..out_width {
            let (sx, mut sy) = match transform {
                Transform::_90 => (y, height - 1 - x),
                Transform::_180 => (width - 1 - x, height - 1 - y),
                Transform::_270 => (width - 1 - y, x),
                Transform::Flipped => (width - 1 - x, y),
                Transform::Flipped90 => (y, x),
                Transform::Flipped270 => (width - 1 - y, height - 1 - x),
                _ => (x, y),
            };
            if y_invert {
                sy = height - 1 - sy;
            }
            let src = sy as usize * stride as usize + sx as usize * 4;
            let dst = y as usize * row + x as usize * 4;
            pixels[dst..dst + 4].copy_from_slice(&source[src..src + 4]);
        }
    }
    (out_width, out_height)
}

fn buffer_size(width: u32, height: u32, stride: u32) -> Result<usize, String> {
    let row = width.checked_mul(4).ok_or("Capture dimensions overflow")?;
    let bytes = stride.checked_mul(height).ok_or("Capture dimensions overflow")?;

    if width == 0 || height == 0 || stride < row || bytes > 128 * 1024 * 1024 {
        return Err("Unsupported capture dimensions".into());
    }

    Ok(bytes as usize)
}

impl Dispatch<wayland_client::protocol::wl_callback::WlCallback, Arc<AtomicBool>> for State {
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_callback::WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        done.store(true, Ordering::Relaxed);
    }
}

wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(State: ignore manager::ZwlrScreencopyManagerV1);
impl Dispatch<window_manager::FereseWindowCaptureManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &window_manager::FereseWindowCaptureManagerV1,
        event: window_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            window_manager::Event::Geometry { width, height, .. } => state.logical_size = Some((width, height)),
            window_manager::Event::Busy { .. } => state.busy = true,
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_and_final_validation_cancel_on_a_stalled_compositor() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;

        for discovery in [true, false] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let worker = std::thread::spawn(move || {
                let connection = Connection::from_socket(client).unwrap();
                if discovery {
                    Capture::connect_with_connection(connection, &flag).err().unwrap()
                } else {
                    let queue = connection.new_event_queue();
                    let mut capture = Capture {
                        connection,
                        queue,
                        state: State::default(),
                        selected_output: None,
                    };
                    capture.validate_sources(&[], &flag).unwrap_err()
                }
            });
            // Wait for the real Wayland request before cancelling its roundtrip.
            assert!(server.read(&mut [0u8; 64]).unwrap() > 0);
            let cancelled = Instant::now();
            stop.store(true, Ordering::SeqCst);
            assert!(worker.join().unwrap().to_lowercase().contains("cancel"));
            assert!(cancelled.elapsed() < Duration::from_secs(1));
        }
    }

    #[test]
    fn capture_orientation_matches_presentation_for_all_transforms() {
        use wl_output::Transform::*;
        // Asymmetric 3x2 image with row padding and distinct four-byte pixels.
        let cases = [
            (Normal, (3, 2), "abcdef"),
            (_90, (2, 3), "daebfc"),
            (_180, (3, 2), "fedcba"),
            (_270, (2, 3), "cfbead"),
            (Flipped, (3, 2), "cbafed"),
            (Flipped90, (2, 3), "adbecf"),
            (Flipped180, (3, 2), "defabc"),
            (Flipped270, (2, 3), "fcebda"),
        ];
        let pixel = |value: u8| [value, value + 1, value + 2, 255];
        let mut pixels = vec![0; 100];
        for (transform, size, expected) in cases {
            for y_invert in [false, true] {
                let mut source = Vec::new();
                let rows = if y_invert { [b"def", b"abc"] } else { [b"abc", b"def"] };
                for row in rows {
                    for &value in row {
                        source.extend_from_slice(&pixel(value));
                    }
                    source.extend_from_slice(&[0; 4]);
                }
                assert_eq!(
                    copy_oriented_pixels(&source, 3, 2, 16, transform, y_invert, &mut pixels),
                    size,
                );
                let expected: Vec<_> = expected.bytes().flat_map(pixel).collect();
                assert_eq!(pixels, expected, "{transform:?}, YInvert={y_invert}");
            }
        }
    }

    #[test]
    fn rejects_unbounded_and_invalid_buffers() {
        assert_eq!(buffer_size(640, 480, 2560).unwrap(), 1228800);
        assert!(buffer_size(640, 480, 12).is_err());
        assert!(buffer_size(0, 480, 0).is_err());
        assert!(buffer_size(u32::MAX, 2, u32::MAX).is_err());
        assert!(buffer_size(20000, 20000, 80000).is_err());
    }
}
