use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferese_protocols::window_capture::v1::client::ferese_window_capture_manager_v1 as window_manager;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1 as source,
    ext_output_image_capture_source_manager_v1 as output_manager,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as manager,
    ext_image_copy_capture_session_v1 as session,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1 as params, zwp_linux_dmabuf_v1 as dmabuf,
};

use super::activity::FailureKind;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Constraints {
    pub size: (u32, u32),
    pub shm: bool,
    pub device: Option<u64>,
    pub modifiers: Vec<u64>,
}

const MAX_DMA_MODIFIERS: usize = 64;
const MAX_DMA_PLANES: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DmaFormat {
    pub modifier: u64,
    pub planes: u32,
}

impl Constraints {
    fn record_modifiers(&mut self, bytes: &[u8]) {
        for bytes in bytes.chunks_exact(8) {
            let modifier = u64::from_ne_bytes(bytes.try_into().expect("modifier width"));
            if modifier == u64::from(gbm::Modifier::Invalid) || self.modifiers.contains(&modifier) {
                continue;
            }
            if self.modifiers.len() < MAX_DMA_MODIFIERS {
                self.modifiers.push(modifier);
            } else if modifier == 0 {
                // Keep the broadly importable linear layout even if the
                // compositor listed many native modifiers before it.
                self.modifiers[MAX_DMA_MODIFIERS - 1] = modifier;
            }
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        let (width, height) = self.size;
        if width == 0
            || height == 0
            || u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|pixels| pixels.checked_mul(4))
                .is_none_or(|bytes| bytes > 128 * 1024 * 1024)
            || !self.shm
        {
            return Err("Invalid capture buffer constraints".into());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) enum Completion {
    Ready,
    Retry,
    Failed { kind: FailureKind, error: String },
}

impl Completion {
    fn failed(reason: WEnum<frame::FailureReason>) -> Self {
        if reason == WEnum::Value(frame::FailureReason::BufferConstraints) {
            return Self::Retry;
        }
        let kind = match &reason {
            WEnum::Value(frame::FailureReason::Unknown) => FailureKind::Unknown,
            WEnum::Value(frame::FailureReason::Stopped) => FailureKind::Stopped,
            _ => FailureKind::Other,
        };
        Self::Failed {
            kind,
            error: format!("Capture failed: {reason:?}"),
        }
    }
}

pub(super) struct Buffer {
    pub proxy: wl_buffer::WlBuffer,
    pub planes: Vec<Plane>,
    pub dma: Option<DmaFormat>,
    pub size: (u32, u32),
    _bo: Option<gbm::BufferObject<()>>,
}

pub(super) struct Plane {
    pub fd: File,
    pub stride: u32,
    pub offset: u32,
    pub bytes: u32,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

#[derive(Default)]
struct State {
    outputs: HashMap<u32, (wl_output::WlOutput, String)>,
    manager: Option<manager::ExtImageCopyCaptureManagerV1>,
    windows: Option<window_manager::FereseWindowCaptureManagerV1>,
    output_manager: Option<output_manager::ExtOutputImageCaptureSourceManagerV1>,
    shm: Option<wl_shm::WlShm>,
    dma: Option<dmabuf::ZwpLinuxDmabufV1>,
    pending_constraints: Constraints,
    constraints: Option<Constraints>,
    logical_size: Option<(u32, u32)>,
    changed: bool,
    stopped: bool,
    completion: Option<Completion>,
    error: Option<String>,
    selected: Option<u32>,
}

pub(super) struct Capture {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    session: session::ExtImageCopyCaptureSessionV1,
    source: source::ExtImageCaptureSourceV1,
    frame: Option<frame::ExtImageCopyCaptureFrameV1>,
    pub device: Option<gbm::Device<File>>,
    device_id: Option<u64>,
    workspace: bool,
}

impl Capture {
    pub(super) fn connect(
        name: &str,
        cursor: bool,
        generation: Option<u32>,
        include_shell: bool,
        workspace_quality: crate::workspace::Quality,
        stop: &AtomicBool,
    ) -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|error| error.to_string())?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let mut state = State::default();
        connection.display().get_registry(&qh, ());
        // Startup alone uses bounded roundtrips. Streaming dispatch is integrated
        // with PipeWire's fd loop and never waits for an unchanged source.
        let deadline = Instant::now() + Duration::from_secs(5);
        for _ in 0..2 {
            let done = std::sync::Arc::new(AtomicBool::new(false));
            connection.display().sync(&qh, done.clone());
            while !done.load(Ordering::Relaxed) {
                if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    return Err("Capture startup cancelled or timed out".into());
                }
                dispatch(&connection, &mut queue, &mut state, 20)?;
            }
        }
        let source = match crate::capture::SourceKind::parse(name) {
            crate::capture::SourceKind::Window(id) => state
                .windows
                .as_ref()
                .ok_or("Persistent window capture is unavailable")?
                .create_source((id >> 32) as u32, id as u32, &qh, ()),
            crate::capture::SourceKind::Workspace(id) => {
                let manager = state
                    .windows
                    .as_ref()
                    .filter(|manager| manager.version() >= 3)
                    .ok_or("Workspace capture is unavailable")?;
                if workspace_quality != crate::workspace::Quality::Balanced {
                    if manager.version() < 5 {
                        return Err("Workspace capture quality selection is unavailable".into());
                    }
                    manager.create_workspace_source_with_options(
                        (id >> 32) as u32,
                        id as u32,
                        u32::from(include_shell),
                        workspace_quality.wire(),
                        &qh,
                        (),
                    )
                } else if include_shell {
                    if manager.version() < 4 {
                        return Err("Workspace shell capture is unavailable".into());
                    }
                    manager.create_workspace_source_with_shell(
                        (id >> 32) as u32,
                        id as u32,
                        &qh,
                        (),
                    )
                } else {
                    manager.create_workspace_source((id >> 32) as u32, id as u32, &qh, ())
                }
            }
            crate::capture::SourceKind::Invalid => return Err("Invalid capture source".into()),
            crate::capture::SourceKind::Monitor => {
                let (&id, (output, _)) = state
                    .outputs
                    .iter()
                    .find(|(_, (_, output_name))| output_name == name)
                    .ok_or("Shared display disconnected")?;
                if generation.is_some_and(|generation| generation != id) {
                    return Err("Shared display was replaced".into());
                }
                state.selected = Some(id);
                state
                    .output_manager
                    .as_ref()
                    .ok_or("Persistent display capture is unavailable")?
                    .create_source(output, &qh, ())
            }
        };
        let options = if cursor {
            manager::Options::PaintCursors
        } else {
            manager::Options::empty()
        };
        let session = state
            .manager
            .as_ref()
            .ok_or("Persistent screen capture is unavailable")?
            .create_session(&source, options, &qh, ());
        while state.constraints.is_none() && !state.stopped {
            if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return Err("Capture startup cancelled or timed out".into());
            }
            dispatch(&connection, &mut queue, &mut state, 20)?;
        }
        if state.stopped {
            return Err("Capture source is unavailable".into());
        }
        let constraints = state
            .constraints
            .as_ref()
            .expect("capture constraints received");
        constraints.validate()?;
        let device_id = constraints.device;
        let device = constraints
            .device
            .and_then(open_device)
            .and_then(|file| gbm::Device::new(file).ok());
        Ok(Self {
            connection,
            queue,
            state,
            session,
            source,
            frame: None,
            device,
            device_id,
            workspace: matches!(
                crate::capture::SourceKind::parse(name),
                crate::capture::SourceKind::Workspace(_)
            ),
        })
    }

    pub(super) fn poll_fd(&self) -> Result<OwnedFd, String> {
        self.connection
            .backend()
            .poll_fd()
            .try_clone_to_owned()
            .map_err(|error| error.to_string())
    }
    pub(super) fn constraints(&self) -> &Constraints {
        self.state
            .constraints
            .as_ref()
            .expect("capture constraints received")
    }
    pub(super) fn logical_size(&self) -> Option<(u32, u32)> {
        self.state.logical_size
    }
    pub(super) fn stopped(&self) -> bool {
        self.state.stopped || self.state.error.is_some()
    }
    pub(super) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.state.changed)
    }
    pub(super) fn dma_formats(&self) -> Vec<DmaFormat> {
        if self.state.dma.is_none() {
            return Vec::new();
        }
        // Probe one allocation at a time, without creating/importing Wayland
        // buffers. Only layouts that fit the compositor's reservation are offered.
        self.constraints()
            .modifiers
            .iter()
            .filter_map(|&modifier| {
                let (bo, planes) = self.allocate_dma(modifier).ok()?;
                Some(DmaFormat {
                    modifier: bo.modifier().into(),
                    planes: planes.len() as u32,
                })
            })
            .collect()
    }
    pub(super) fn dispatch(&mut self) {
        if let Err(error) = dispatch(&self.connection, &mut self.queue, &mut self.state, 0) {
            self.state.error = Some(error);
        }
        let device = self.constraints().device;
        if device != self.device_id {
            self.device = device
                .and_then(open_device)
                .and_then(|file| gbm::Device::new(file).ok());
            self.device_id = device;
        }
    }
    pub(super) fn completion(&mut self) -> Option<Completion> {
        let completion = self.state.completion.take()?;
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
        Some(completion)
    }
    pub(super) fn set_active(&mut self, active: bool) -> Result<(), String> {
        if self.workspace {
            self.state
                .windows
                .as_ref()
                .ok_or("Workspace capture manager disappeared")?
                .set_source_active(&self.source, u32::from(active));
            self.connection.flush().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(super) fn refresh(&mut self) -> Result<(), String> {
        self.state
            .windows
            .as_ref()
            .ok_or("Capture refresh is unavailable")?
            .refresh_source(&self.source);
        self.connection.flush().map_err(|error| error.to_string())
    }

    pub(super) fn submit(&mut self, buffer: &Buffer) -> Result<(), String> {
        if self.frame.is_some() || self.stopped() {
            return Err("Capture is already pending or stopped".into());
        }
        let frame = self.session.create_frame(&self.queue.handle(), ());
        frame.attach_buffer(&buffer.proxy);
        // PipeWire may return any pool buffer. Mark it fully stale; only source
        // damage wakes the session, then the compositor fills this whole buffer.
        frame.damage_buffer(0, 0, buffer.size.0 as i32, buffer.size.1 as i32);
        frame.capture();
        self.frame = Some(frame);
        self.connection.flush().map_err(|error| error.to_string())
    }
    fn allocate_dma(&self, modifier: u64) -> Result<(gbm::BufferObject<()>, Vec<Plane>), String> {
        if !self.constraints().modifiers.contains(&modifier) {
            return Err("DMA-BUF modifier is not supported by the capture source".into());
        }
        let (width, height) = self.constraints().size;
        let device = self.device.as_ref().ok_or("No DMA-BUF allocation device")?;
        let modifier = gbm::Modifier::from(modifier);
        let bo = device
            .create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                gbm::Format::Xrgb8888,
                std::iter::once(modifier),
                gbm::BufferObjectFlags::RENDERING,
            )
            .or_else(|error| {
                if modifier != gbm::Modifier::Linear {
                    return Err(error);
                }
                // Drivers predating explicit modifier allocation may still
                // support the linear fallback through the original GBM API.
                device.create_buffer_object::<()>(
                    width,
                    height,
                    gbm::Format::Xrgb8888,
                    gbm::BufferObjectFlags::RENDERING | gbm::BufferObjectFlags::LINEAR,
                )
            })
            .map_err(|error| error.to_string())?;
        if bo.modifier() != modifier || !(1..=MAX_DMA_PLANES).contains(&bo.plane_count()) {
            return Err("Allocator returned an unexpected DMA-BUF layout".into());
        }
        let mut planes = Vec::new();
        let mut allocations = HashMap::new();
        for index in 0..bo.plane_count() as i32 {
            let fd = File::from(bo.fd_for_plane(index).map_err(|error| error.to_string())?);
            let metadata = fd.metadata().map_err(|error| error.to_string())?;
            let bytes =
                u32::try_from(metadata.len()).map_err(|_| "DMA-BUF allocation is too large")?;
            let stride = bo.stride_for_plane(index);
            let offset = bo.offset(index);
            if bytes == 0 || stride == 0 || stride > i32::MAX as u32 || offset >= bytes {
                return Err("Invalid DMA-BUF plane layout".into());
            }
            allocations.insert((metadata.dev(), metadata.ino()), u64::from(bytes));
            planes.push(Plane {
                fd,
                stride,
                offset,
                bytes,
            });
        }
        validate_dma_reservation(self.constraints().size, allocations.values().copied())?;
        Ok((bo, planes))
    }

    pub(super) fn allocate(&mut self, dma: Option<DmaFormat>) -> Result<Buffer, String> {
        let (width, height) = self.constraints().size;
        let qh = self.queue.handle();
        if let Some(format) = dma {
            let (bo, planes) = self.allocate_dma(format.modifier)?;
            if planes.len() != format.planes as usize {
                return Err("DMA-BUF plane count changed after negotiation".into());
            }
            let params = self
                .state
                .dma
                .as_ref()
                .ok_or("Missing DMA-BUF global")?
                .create_params(&qh, ());
            for (index, plane) in planes.iter().enumerate() {
                params.add(
                    plane.fd.as_fd(),
                    index as u32,
                    plane.offset,
                    plane.stride,
                    (format.modifier >> 32) as u32,
                    format.modifier as u32,
                );
            }
            let proxy = params.create_immed(
                width as i32,
                height as i32,
                gbm::Format::Xrgb8888 as u32,
                params::Flags::empty(),
                &qh,
                (),
            );
            params.destroy();
            return Ok(Buffer {
                proxy,
                planes,
                dma: Some(format),
                size: (width, height),
                _bo: Some(bo),
            });
        }
        let stride = width.checked_mul(4).ok_or("SHM row is too large")?;
        let bytes = stride
            .checked_mul(height)
            .filter(|bytes| *bytes <= 128 * 1024 * 1024)
            .ok_or("SHM buffer is too large")?;
        // SAFETY: this returns a new owned anonymous file with a fixed C name.
        let fd = unsafe {
            libc::memfd_create(
                c"ferese-stream".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: memfd_create succeeded and ownership transfers to File.
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(u64::from(bytes))
            .map_err(|error| error.to_string())?;
        // SAFETY: this file is owned here and has its final size. The compositor
        // can map it but neither process needs to resize a pool buffer.
        if unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let pool = self
            .state
            .shm
            .as_ref()
            .ok_or("Missing SHM global")?
            .create_pool(file.as_fd(), bytes as i32, &qh, ());
        let proxy = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Xrgb8888,
            &qh,
            (),
        );
        pool.destroy();
        Ok(Buffer {
            proxy,
            planes: vec![Plane {
                fd: file,
                stride,
                offset: 0,
                bytes,
            }],
            dma: None,
            size: (width, height),
            _bo: None,
        })
    }
}

fn validate_dma_reservation(
    (width, height): (u32, u32),
    allocations: impl IntoIterator<Item = u64>,
) -> Result<(), String> {
    // Count each fd allocation once, including auxiliary compression planes.
    // This matches the compositor's per-image, 4-KiB row-aligned reservation.
    let reserved = (u64::from(width) * 4 + 4095) & !4095;
    let reserved = reserved
        .checked_mul(u64::from(height))
        .ok_or("DMA-BUF image is too large")?;
    let mut used = 0u64;
    for bytes in allocations {
        used = used
            .checked_add(bytes)
            .ok_or("DMA-BUF allocation is too large")?;
        if used > reserved || used > 128 * 1024 * 1024 {
            return Err("DMA-BUF allocation exceeds the capture memory reservation".into());
        }
    }
    Ok(())
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
        self.session.destroy();
        self.source.destroy();
        let _ = self.connection.flush();
    }
}

fn open_device(device: u64) -> Option<File> {
    std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            if !entry.file_name().as_encoded_bytes().starts_with(b"renderD")
                || entry.metadata().ok()?.rdev() != device
            {
                return None;
            }
            File::options()
                .read(true)
                .write(true)
                .open(entry.path())
                .ok()
        })
}

fn dispatch(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    timeout: i32,
) -> Result<(), String> {
    queue
        .dispatch_pending(state)
        .map_err(|error| error.to_string())?;
    connection.flush().map_err(|error| error.to_string())?;
    if let Some(read) = queue.prepare_read() {
        let mut fd = libc::pollfd {
            fd: read.connection_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll receives one live descriptor owned by the read guard.
        let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
        if ready > 0 {
            read.read().map_err(|error| error.to_string())?;
        } else if ready < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    queue
        .dispatch_pending(state)
        .map_err(|error| error.to_string())?;
    Ok(())
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
                "wl_output" => {
                    state.outputs.insert(
                        name,
                        (
                            registry.bind(name, version.min(4), qh, name),
                            format!("output-{name}"),
                        ),
                    );
                }
                "ext_image_copy_capture_manager_v1" => {
                    state.manager = Some(registry.bind(name, 1, qh, ()))
                }
                "ext_output_image_capture_source_manager_v1" => {
                    state.output_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "ferese_window_capture_manager_v1" if version >= 2 => {
                    state.windows = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwp_linux_dmabuf_v1" if version >= 3 => {
                    state.dma = Some(registry.bind(name, version.min(4), qh, ()))
                }
                _ => (),
            },
            wl_registry::Event::GlobalRemove { name } => {
                state.outputs.remove(&name);
                if state.selected == Some(name) {
                    state.stopped = true;
                }
            }
            _ => (),
        }
    }
}

impl Dispatch<session::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &session::ExtImageCopyCaptureSessionV1,
        event: session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            session::Event::BufferSize { width, height } => {
                state.pending_constraints.size = (width, height)
            }
            session::Event::ShmFormat {
                format: WEnum::Value(wl_shm::Format::Xrgb8888),
            } => state.pending_constraints.shm = true,
            session::Event::DmabufDevice { device }
                if device.len() == std::mem::size_of::<libc::dev_t>() =>
            {
                let mut bytes = [0u8; 8];
                bytes[..device.len()].copy_from_slice(&device);
                state.pending_constraints.device = Some(u64::from_ne_bytes(bytes));
            }
            session::Event::DmabufFormat { format, modifiers }
                if format == gbm::Format::Xrgb8888 as u32 =>
            {
                state.pending_constraints.record_modifiers(&modifiers);
            }
            session::Event::Done => {
                let constraints = std::mem::take(&mut state.pending_constraints);
                match constraints.validate() {
                    Ok(()) => {
                        state.changed |= state
                            .constraints
                            .as_ref()
                            .is_some_and(|old| old != &constraints);
                        state.constraints = Some(constraints);
                    }
                    Err(error) => state.error = Some(error),
                }
            }
            session::Event::Stopped => state.stopped = true,
            _ => (),
        }
    }
}

impl Dispatch<frame::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &frame::ExtImageCopyCaptureFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Ready => state.completion = Some(Completion::Ready),
            frame::Event::Failed { reason } => {
                state.completion = Some(Completion::failed(reason));
            }
            frame::Event::Transform { transform }
                if transform != WEnum::Value(wl_output::Transform::Normal) =>
            {
                state.error = Some("Capture returned a non-upright buffer".into())
            }
            _ => (),
        }
    }
}

impl Dispatch<window_manager::FereseWindowCaptureManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &window_manager::FereseWindowCaptureManagerV1,
        event: window_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let window_manager::Event::SourceGeometry { width, height, .. } = event {
            state.logical_size = Some((width, height));
        }
    }
}

impl Dispatch<wayland_client::protocol::wl_callback::WlCallback, std::sync::Arc<AtomicBool>>
    for State
{
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_callback::WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        done: &std::sync::Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        done.store(true, Ordering::Relaxed);
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some((_, output_name)) = state.outputs.get_mut(id)
        {
            *output_name = name;
        }
    }
}
wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(State: ignore manager::ExtImageCopyCaptureManagerV1);
wayland_client::delegate_noop!(State: ignore output_manager::ExtOutputImageCaptureSourceManagerV1);
wayland_client::delegate_noop!(State: ignore source::ExtImageCaptureSourceV1);
wayland_client::delegate_noop!(State: ignore dmabuf::ZwpLinuxDmabufV1);
wayland_client::delegate_noop!(State: ignore params::ZwpLinuxBufferParamsV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_events_preserve_explicit_layouts_and_bound_negotiation_work() {
        let mut constraints = Constraints::default();
        let modifiers = [42, 0, u64::from(gbm::Modifier::Invalid), 42];
        let mut bytes = modifiers
            .into_iter()
            .flat_map(u64::to_ne_bytes)
            .collect::<Vec<_>>();
        bytes.extend([1, 2, 3]);
        constraints.record_modifiers(&bytes);
        assert_eq!(constraints.modifiers, vec![42, 0]);
        constraints.record_modifiers(&(1..1000u64).flat_map(u64::to_ne_bytes).collect::<Vec<_>>());
        assert_eq!(constraints.modifiers.len(), MAX_DMA_MODIFIERS);
        let mut linear_last = Constraints::default();
        linear_last.record_modifiers(
            &(1..1000u64)
                .chain([0])
                .flat_map(u64::to_ne_bytes)
                .collect::<Vec<_>>(),
        );
        assert_eq!(linear_last.modifiers.len(), MAX_DMA_MODIFIERS);
        assert!(linear_last.modifiers.contains(&0));
    }

    #[test]
    fn auxiliary_plane_allocations_cannot_exceed_the_image_memory_reservation() {
        // 1024-wide XR24 rows already fill their 4-KiB alignment allowance.
        assert!(validate_dma_reservation((1024, 512), [2 * 1024 * 1024]).is_ok());
        assert!(validate_dma_reservation((1024, 512), [1024 * 1024, 1024 * 1024]).is_ok());
        assert!(validate_dma_reservation((1024, 512), [2 * 1024 * 1024, 1]).is_err());
        assert!(validate_dma_reservation((1024, 512), [u64::MAX, 1]).is_err());
        assert!(validate_dma_reservation((u32::MAX, u32::MAX), []).is_err());
    }

    #[test]
    fn protocol_failure_reasons_preserve_retry_and_stop_semantics() {
        assert!(matches!(
            Completion::failed(WEnum::Value(frame::FailureReason::BufferConstraints)),
            Completion::Retry
        ));
        for (reason, expected) in [
            (
                WEnum::Value(frame::FailureReason::Unknown),
                FailureKind::Unknown,
            ),
            (
                WEnum::Value(frame::FailureReason::Stopped),
                FailureKind::Stopped,
            ),
            (WEnum::Unknown(u32::MAX), FailureKind::Other),
        ] {
            let Completion::Failed { kind, error } = Completion::failed(reason) else {
                panic!("an error must retain its failure kind");
            };
            assert_eq!(kind, expected);
            assert!(!error.is_empty());
        }
    }

    #[test]
    fn buffer_constraints_are_bounded_without_integer_overflow() {
        let mut constraints = Constraints {
            size: (640, 480),
            shm: true,
            ..Constraints::default()
        };
        assert!(constraints.validate().is_ok());
        constraints.size = (16384, 2048);
        assert!(constraints.validate().is_ok());
        for size in [(0, 1), (1, 0), (16384, 2049), (u32::MAX, u32::MAX)] {
            constraints.size = size;
            assert!(constraints.validate().is_err(), "{size:?}");
        }
        constraints.size = (640, 480);
        constraints.shm = false;
        assert!(constraints.validate().is_err());
    }
}
