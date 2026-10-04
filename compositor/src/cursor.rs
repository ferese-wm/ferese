use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopSignal, RegistrationToken};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};
use xcursor::CursorTheme;

const DEFAULT_CURSOR_SIZE: u32 = 24;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAMES: usize = 256;
const MAX_PIXELS: usize = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct CursorFrame {
    pub buffer: MemoryRenderBuffer,
    pub hotspot: Point<i32, Buffer>,
    pub size: Size<i32, Buffer>,
    pub buffer_scale: i32,
    delay: Duration,
}

impl CursorFrame {
    pub(crate) fn logical_rect(&self, pointer: Point<f64, Logical>) -> Rectangle<f64, Logical> {
        Rectangle::new(
            pointer
                - Point::from((
                    f64::from(self.hotspot.x) / f64::from(self.buffer_scale),
                    f64::from(self.hotspot.y) / f64::from(self.buffer_scale),
                )),
            (
                f64::from(self.size.w) / f64::from(self.buffer_scale),
                f64::from(self.size.h) / f64::from(self.buffer_scale),
            )
                .into(),
        )
    }

    pub(crate) fn physical_location(&self, pointer: Point<f64, Logical>, scale: f64) -> Point<f64, Physical> {
        self.logical_rect(pointer).loc.to_physical(scale)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NamedCursor {
    frames: Arc<[CursorFrame]>,
    bytes: usize,
    requested_scale: i32,
}

impl NamedCursor {
    fn sample(&self, elapsed: Duration) -> (usize, Option<Duration>) {
        if self.frames.len() <= 1 {
            return (0, None);
        }

        let period: Duration = self.frames.iter().map(|frame| frame.delay).sum();
        let mut offset = elapsed.as_nanos() % period.as_nanos();
        for (index, frame) in self.frames.iter().enumerate() {
            if offset < frame.delay.as_nanos() {
                return (index, Some(frame.delay - Duration::from_nanos(offset as u64)));
            }
            offset -= frame.delay.as_nanos();
        }
        unreachable!("cursor phase is within the bounded, nonzero period")
    }
}

#[derive(Default)]
struct Requests {
    pending: Option<(CursorIcon, i32)>,
    in_flight: Option<(CursorIcon, i32)>,
    closed: bool,
}

struct Loaded {
    icon: CursorIcon,
    scale: i32,
    cursor: Option<NamedCursor>,
}

pub(crate) struct Loader {
    shared: Arc<(Mutex<Requests>, Condvar)>,
    results: mpsc::Receiver<Loaded>,
}

impl Loader {
    pub(crate) fn new(signal: LoopSignal) -> std::io::Result<Self> {
        Self::with_loader(signal, {
            // Theme discovery and inherited-theme reads belong to the worker too.
            let mut theme = None;
            move |(icon, scale)| {
                let theme = theme.get_or_insert_with(|| {
                    CursorTheme::load(&std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into()))
                });
                load_named_cursor(theme, icon, scale)
            }
        })
    }

    fn with_loader(
        signal: LoopSignal,
        mut load: impl FnMut((CursorIcon, i32)) -> Option<NamedCursor> + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(Requests {
                pending: Some((CursorIcon::Default, 1)),
                ..Requests::default()
            }),
            Condvar::new(),
        ));
        let worker = shared.clone();
        let (sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("ferese-cursors".into())
            .spawn(move || {
                loop {
                    let (requests, ready) = &*worker;
                    let mut requests = ready
                        .wait_while(requests.lock().unwrap(), |requests| {
                            !requests.closed && requests.pending.is_none()
                        })
                        .unwrap();
                    if requests.closed {
                        break;
                    }
                    let request = requests.pending.take().unwrap();
                    requests.in_flight = Some(request);
                    drop(requests);
                    let cursor = load(request);
                    let (icon, scale) = request;
                    if sender.send(Loaded { icon, scale, cursor }).is_err() {
                        break;
                    }
                    signal.wakeup();
                    worker.0.lock().unwrap().in_flight = None;
                }
            })?;
        Ok(Self { shared, results })
    }

    pub(crate) fn request(&self, icon: CursorIcon, scale: i32) {
        let request = (icon, scale);
        let mut requests = self.shared.0.lock().unwrap();
        if requests.in_flight == Some(request) || requests.pending == Some(request) {
            return;
        }
        requests.pending = Some(request);
        self.shared.1.notify_one();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        let mut requests = self.shared.0.lock().unwrap();
        requests.closed = true;
        requests.pending = None;
        self.shared.1.notify_one();
    }
}

pub(crate) struct Animation {
    icon: Option<CursorIcon>,
    epoch: Instant,
    frame: usize,
    timer: Option<RegistrationToken>,
    pub(crate) reset: bool,
    pub(crate) buffer_scale: i32,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            icon: None,
            epoch: Instant::now(),
            frame: 0,
            timer: None,
            reset: false,
            buffer_scale: 1,
        }
    }
}

impl crate::Ferese {
    pub(crate) fn named_cursor_frame(&self) -> Option<&CursorFrame> {
        let CursorImageStatus::Named(icon) = self.cursor_status else {
            return None;
        };
        let cursor = self
            .named_cursors
            .get(&icon)
            .or_else(|| self.named_cursors.get(&CursorIcon::Default))?;
        let frame = if self.cursor_animation.icon == Some(icon) {
            self.cursor_animation.frame
        } else {
            0
        };
        cursor.frames.get(frame).or_else(|| cursor.frames.first())
    }

    pub(crate) fn update_named_cursor(&mut self, now: Instant) {
        self.cursor_animation.buffer_scale = if let Some(backend) = self.direct_backend.as_ref() {
            backend
                .physical_outputs()
                .map(|output| output.current_scale().fractional_scale().ceil() as i32)
                .max()
        } else {
            self.space
                .outputs()
                .map(|output| output.current_scale().fractional_scale().ceil() as i32)
                .max()
        }
        .unwrap_or(1)
        .clamp(1, 8);
        while let Ok(Loaded { icon, scale, cursor }) = self.cursor_loader.results.try_recv() {
            let mut cursor = cursor.unwrap_or_else(fallback_cursor);
            cursor.requested_scale = scale;
            let active = match self.cursor_status {
                CursorImageStatus::Named(icon) => Some(icon),
                _ => None,
            };
            let affects_current = active == Some(icon)
                || (icon == CursorIcon::Default
                    && active.is_some_and(|active| !self.named_cursors.contains_key(&active)));
            let mut bytes: usize = self.named_cursors.values().map(|cursor| cursor.bytes).sum();
            while bytes + cursor.bytes > MAX_CACHE_BYTES {
                let Some(evict) = self
                    .named_cursors
                    .keys()
                    .copied()
                    .find(|key| *key != CursorIcon::Default && *key != icon && Some(*key) != active)
                else {
                    break;
                };
                bytes -= self.named_cursors.remove(&evict).unwrap().bytes;
            }
            self.named_cursors.insert(icon, cursor);
            if affects_current {
                self.cursor_animation.reset = true;
                self.cursor_redraw_pending |= crate::backends::direct::cursor_animation_visible(self);
            }
        }

        let icon = match self.cursor_status {
            CursorImageStatus::Named(icon) => Some(icon),
            _ => None,
        };
        if let Some(icon) = icon
            && self
                .named_cursors
                .get(&icon)
                .is_none_or(|cursor| cursor.requested_scale != self.cursor_animation.buffer_scale)
        {
            self.cursor_loader.request(icon, self.cursor_animation.buffer_scale);
        }
        let animated = icon
            .and_then(|icon| {
                self.named_cursors
                    .get(&icon)
                    .or_else(|| self.named_cursors.get(&CursorIcon::Default))
            })
            .is_some_and(|cursor| cursor.frames.len() > 1);
        let visible = animated && crate::backends::direct::cursor_animation_visible(self);
        if !visible || self.cursor_animation.icon != icon || self.cursor_animation.reset {
            if let Some(token) = self.cursor_animation.timer.take() {
                self.loop_handle.remove(token);
            }
            self.cursor_animation.icon = icon.filter(|_| visible);
            self.cursor_animation.epoch = now;
            self.cursor_animation.frame = 0;
            self.cursor_animation.reset = false;
        }
        if !visible {
            return;
        }
        let icon = icon.unwrap();
        let cursor = self
            .named_cursors
            .get(&icon)
            .or_else(|| self.named_cursors.get(&CursorIcon::Default))
            .unwrap();
        let (frame, remaining) = cursor.sample(now.saturating_duration_since(self.cursor_animation.epoch));
        if self.cursor_animation.frame != frame {
            self.cursor_animation.frame = frame;
            self.cursor_redraw_pending = true;
        }
        if let Some(remaining) = remaining
            && self.cursor_animation.timer.is_none()
        {
            match self
                .loop_handle
                .insert_source(Timer::from_deadline(now + remaining), |_, _, state| {
                    state.cursor_animation.timer = None;
                    state.cursor_redraw_pending = true;
                    TimeoutAction::Drop
                }) {
                Ok(token) => self.cursor_animation.timer = Some(token),
                Err(error) => tracing::warn!(%error, "could not schedule animated cursor"),
            }
        }
    }
}

fn load_named_cursor(theme: &CursorTheme, icon: CursorIcon, scale: i32) -> Option<NamedCursor> {
    let requested = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|size| size.parse::<u32>().ok())
        .filter(|size| *size > 0 && *size <= 1024)
        .unwrap_or(DEFAULT_CURSOR_SIZE);
    cursor_names(icon).iter().find_map(|name| {
        let path = theme.load_icon(name)?;
        let file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64).read_to_end(&mut bytes).ok()?;
        parse_cursor(&bytes, requested, scale)
    })
}

fn cursor_names(icon: CursorIcon) -> [&'static str; 3] {
    match icon {
        CursorIcon::Default => ["default", "left_ptr", "arrow"],
        CursorIcon::Pointer => ["pointer", "hand2", "left_ptr"],
        CursorIcon::Text => ["text", "xterm", "left_ptr"],
        _ => [icon.name(), "default", "left_ptr"],
    }
}

// Inspect every image before allocating buffers, then decode only the selected
// nominal size. TOC aliases count toward the budget just like separate frames.
fn parse_cursor(bytes: &[u8], requested: u32, scale: i32) -> Option<NamedCursor> {
    fn word(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
        ))
    }
    if bytes.len() > MAX_FILE_BYTES || bytes.get(..4)? != b"Xcur" {
        return None;
    }
    let header = word(bytes, 4)? as usize;
    let count = word(bytes, 12)? as usize;
    if header < 16 || count > MAX_FRAMES || header.checked_add(count.checked_mul(12)?)? > bytes.len() {
        return None;
    }
    let mut images = Vec::new();
    let mut total_pixels = 0usize;
    for i in 0..count {
        let toc = header + i * 12;
        if word(bytes, toc)? != 0xfffd0002 {
            continue;
        }
        let nominal = word(bytes, toc + 4)?;
        let pos = word(bytes, toc + 8)? as usize;
        if word(bytes, pos)? != 36
            || word(bytes, pos.checked_add(4)?)? != 0xfffd0002
            || word(bytes, pos.checked_add(8)?)? != nominal
            || word(bytes, pos.checked_add(12)?)? != 1
        {
            return None;
        }
        let width = word(bytes, pos + 16)?;
        let height = word(bytes, pos + 20)?;
        let xhot = word(bytes, pos + 24)?;
        let yhot = word(bytes, pos + 28)?;
        if nominal == 0 || width == 0 || height == 0 || width > 1024 || height > 1024 || xhot > width || yhot > height {
            return None;
        }
        let pixels = (width as usize).checked_mul(height as usize)?;
        total_pixels = total_pixels.checked_add(pixels)?;
        if total_pixels > MAX_PIXELS {
            return None;
        }
        let start = pos.checked_add(36)?;
        let end = start.checked_add(pixels.checked_mul(4)?)?;
        let rgba = bytes.get(start..end)?;
        images.push((nominal, width, height, xhot, yhot, word(bytes, pos + 32)?, rgba));
    }
    let base = images
        .iter()
        .map(|image| image.0)
        .min_by_key(|size| (size.abs_diff(requested), *size))?;
    let scale = scale.clamp(1, 8);
    let desired = base.checked_mul(scale as u32)?;
    let nominal = images
        .iter()
        .map(|image| image.0)
        .min_by_key(|size| (size.abs_diff(desired), *size))?;
    let mut frames = Vec::new();
    let mut asset_bytes = 0usize;
    for (_, width, height, xhot, yhot, delay, rgba) in images.into_iter().filter(|image| image.0 == nominal) {
        let ratio = f64::from(desired) / f64::from(nominal);
        let w = (f64::from(width) * ratio).round().max(1.0) as u32;
        let h = (f64::from(height) * ratio).round().max(1.0) as u32;
        if w > 1024 || h > 1024 {
            return None;
        }
        let size: Size<i32, Buffer> = (w as i32, h as i32).into();
        asset_bytes = asset_bytes.checked_add((w as usize).checked_mul(h as usize)?.checked_mul(4)?)?;
        if asset_bytes > MAX_PIXELS * 4 {
            return None;
        }
        let resized;
        let pixels = if w != width || h != height {
            let original = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
            resized = image::imageops::resize(&original, w, h, image::imageops::FilterType::Triangle);
            resized.as_raw().as_slice()
        } else {
            rgba
        };
        frames.push(CursorFrame {
            buffer: MemoryRenderBuffer::from_slice(pixels, Fourcc::Abgr8888, size, scale, Transform::Normal, None),
            hotspot: (
                (f64::from(xhot) * ratio).round() as i32,
                (f64::from(yhot) * ratio).round() as i32,
            )
                .into(),
            size,
            buffer_scale: scale,
            // Zero-delay themes need a finite cadence rather than a busy loop.
            delay: Duration::from_millis(if delay == 0 { 16 } else { u64::from(delay) }),
        });
    }
    Some(NamedCursor {
        frames: frames.into(),
        bytes: asset_bytes,
        requested_scale: scale,
    })
}

pub(crate) fn fallback_cursor() -> NamedCursor {
    const WIDTH: usize = 12;
    const HEIGHT: usize = 18;
    let mut pixels = vec![0_u8; WIDTH * HEIGHT * 4];
    for y in 0..HEIGHT {
        let body_width = (y / 2 + 1).min(8);
        for x in 0..body_width {
            let edge = x == 0 || x + 1 == body_width || y == 0;
            let offset = (y * WIDTH + x) * 4;
            let color = if edge { 0 } else { 255 };
            pixels[offset..offset + 4].copy_from_slice(&[color, color, color, 255]);
        }
    }
    NamedCursor {
        frames: vec![CursorFrame {
            buffer: MemoryRenderBuffer::from_slice(
                &pixels,
                Fourcc::Abgr8888,
                (WIDTH as i32, HEIGHT as i32),
                1,
                Transform::Normal,
                None,
            ),
            hotspot: Point::default(),
            size: (WIDTH as i32, HEIGHT as i32).into(),
            buffer_scale: 1,
            delay: Duration::from_millis(16),
        }]
        .into(),
        bytes: pixels.len(),
        requested_scale: 1,
    }
}

#[cfg(test)]
mod tests;
