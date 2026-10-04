//! Converts output-buffer readback into upright image coordinates.

use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender as ReplySender;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use ferese_ipc::Response;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};

const BYTES_PER_PIXEL: usize = 4;

pub(crate) struct OutputFrame {
    pub(crate) preserve_alpha: bool,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) stride: usize,
    pub(crate) pixels: Vec<u8>,
    pub(crate) transform: Transform,
    pub(crate) location: (i32, i32),
    pub(crate) scale: f64,
    // The clipped logical extent this readback was taken for, in upright
    // orientation. Dividing the readback dimensions by `scale` would not
    // recover it exactly at fractional scales, so it is carried through
    // unchanged rather than recomputed on the worker.
    pub(crate) logical_width: i32,
    pub(crate) logical_height: i32,
}

pub(crate) struct Canvas {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
}

const MAX_CANVAS_BYTES: usize = 128 * 1024 * 1024;

impl Canvas {
    fn new(width: u32, height: u32) -> Option<Self> {
        let length = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        if width == 0 || height == 0 || length > MAX_CANVAS_BYTES {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels: vec![0; length],
        })
    }

    fn blit(&mut self, source: &[u8], source_width: usize, source_height: usize, x: i32, y: i32) {
        let canvas_width = self.width as usize;
        let source_row_bytes = source_width.checked_mul(BYTES_PER_PIXEL);
        let (Some(source_row_bytes), Some(canvas_row_bytes)) =
            (source_row_bytes, canvas_width.checked_mul(BYTES_PER_PIXEL))
        else {
            return;
        };

        let first_row = y.clamp(0, self.height as i32) as usize;
        let last_row = (y + source_height as i32).clamp(0, self.height as i32) as usize;
        if first_row >= last_row {
            return;
        }
        let first_column = x.clamp(0, self.width as i32) as usize;
        let last_column = (x + source_width as i32).clamp(0, self.width as i32) as usize;
        if first_column >= last_column {
            return;
        }

        let width = (last_column - first_column) * BYTES_PER_PIXEL;
        let source_column_offset = (first_column as i32 - x) as usize * BYTES_PER_PIXEL;
        for row in first_row..last_row {
            let from = (row - y as usize) * source_row_bytes + source_column_offset;
            let to = row * canvas_row_bytes + first_column * BYTES_PER_PIXEL;
            if from + width <= source.len() && to + width <= self.pixels.len() {
                self.pixels[to..to + width].copy_from_slice(&source[from..from + width]);
            }
        }
    }
}

fn scale_and_convert(frame: &OutputFrame, target_width: usize, target_height: usize) -> Option<Vec<u8>> {
    let (upright_width, upright_height) = upright_size(frame);
    if upright_width <= 0 || upright_height <= 0 || target_width == 0 || target_height == 0 {
        return None;
    }

    let mut out = vec![0_u8; target_width.checked_mul(target_height)?.checked_mul(4)?];
    let row_bytes = target_width.checked_mul(BYTES_PER_PIXEL)?;

    if upright_width as usize == target_width && upright_height as usize == target_height {
        for row in 0..target_height {
            for column in 0..target_width {
                let (x, y) =
                    upright_source_pixel(column as i32, row as i32, frame.width, frame.height, frame.transform);
                let from = (y as usize)
                    .checked_mul(frame.stride)?
                    .checked_add(x as usize * BYTES_PER_PIXEL)?;
                if from + BYTES_PER_PIXEL > frame.pixels.len() {
                    return None;
                }
                let to = row * row_bytes + column * BYTES_PER_PIXEL;
                let pixel = &frame.pixels[from..from + BYTES_PER_PIXEL];
                out[to] = pixel[2];
                out[to + 1] = pixel[1];
                out[to + 2] = pixel[0];
                out[to + 3] = if frame.preserve_alpha { pixel[3] } else { 255 };
            }
        }
        return Some(out);
    }

    // Destination columns map to source columns, and only the row varies within
    // a destination row, so resolve each column once.
    let columns: Vec<i32> = (0..target_width)
        .map(|column| (column * upright_width as usize / target_width) as i32)
        .collect();

    for row in 0..target_height {
        let source_row = row * upright_height as usize / target_height;
        for (column, source_column) in columns.iter().enumerate() {
            let (x, y) = upright_source_pixel(
                *source_column,
                source_row as i32,
                frame.width,
                frame.height,
                frame.transform,
            );
            let from = (y as usize)
                .checked_mul(frame.stride)?
                .checked_add(x as usize * BYTES_PER_PIXEL)?;
            if from + BYTES_PER_PIXEL > frame.pixels.len() {
                return None;
            }
            let to = row * row_bytes + column * BYTES_PER_PIXEL;
            let pixel = &frame.pixels[from..from + BYTES_PER_PIXEL];
            out[to] = pixel[2];
            out[to + 1] = pixel[1];
            out[to + 2] = pixel[0];
            out[to + 3] = if frame.preserve_alpha { pixel[3] } else { 255 };
        }
    }
    Some(out)
}

fn upright_source_pixel(x: i32, y: i32, width: i32, height: i32, transform: Transform) -> (i32, i32) {
    match transform {
        Transform::_90 => (y, height - 1 - x),
        Transform::_180 => (width - 1 - x, height - 1 - y),
        Transform::_270 => (width - 1 - y, x),
        Transform::Flipped => (width - 1 - x, y),
        Transform::Flipped90 => (y, x),
        Transform::Flipped180 => (x, height - 1 - y),
        Transform::Flipped270 => (width - 1 - y, height - 1 - x),
        Transform::Normal => (x, y),
    }
}

pub(crate) fn swaps_axes(transform: Transform) -> bool {
    matches!(
        transform,
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
    )
}

pub(crate) fn compose(frames: &[OutputFrame]) -> Result<Canvas, String> {
    for frame in frames {
        validate(frame)?;
    }

    let (left, top) = logical_origin(frames).ok_or("Screenshot region is empty")?;
    let scale = frames.iter().map(|frame| frame.scale).fold(0.0_f64, f64::max);
    if !(scale.is_finite() && scale > 0.0) {
        return Err("Screenshot outputs have an invalid scale".into());
    }

    let mut placement = Vec::with_capacity(frames.len());
    let mut width = 0_i32;
    let mut height = 0_i32;
    for frame in frames {
        // Placement is rounded from the canvas origin, and the far edge is
        // derived from the near edge plus the physical extent, so a frame at the
        // maximum scale keeps its exact pixel dimensions and adjacent frames
        // share their boundary.
        let x0 = ((frame.location.0 - left) as f64 * scale).round() as i32;
        let y0 = ((frame.location.1 - top) as f64 * scale).round() as i32;
        let target_width = canvas_extent(frame.logical_width, scale);
        let target_height = canvas_extent(frame.logical_height, scale);
        let x1 = x0.saturating_add(target_width);
        let y1 = y0.saturating_add(target_height);
        width = width.max(x1);
        height = height.max(y1);
        placement.push((x0, y0, target_width, target_height));
    }

    if width <= 0 || height <= 0 {
        return Err("Screenshot region is empty".into());
    }
    let mut canvas = Canvas::new(width as u32, height as u32).ok_or("Screenshot image is too large to encode")?;

    for (frame, (x0, y0, target_width, target_height)) in frames.iter().zip(placement) {
        if target_width == 0 || target_height == 0 {
            continue;
        }
        let scaled =
            scale_and_convert(frame, target_width as usize, target_height as usize).ok_or("Invalid capture size")?;
        canvas.blit(&scaled, target_width as usize, target_height as usize, x0, y0);
    }

    Ok(canvas)
}

fn canvas_extent(logical: i32, scale: f64) -> i32 {
    if logical <= 0 || !(scale.is_finite() && scale > 0.0) {
        return 0;
    }
    ((logical as f64 * scale).round() as i32).max(0)
}

fn validate(frame: &OutputFrame) -> Result<(), String> {
    let row = (frame.width as usize)
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or("Capture dimensions overflow")?;
    if frame.width <= 0
        || frame.height <= 0
        || frame.logical_width <= 0
        || frame.logical_height <= 0
        || frame.stride < row
        || !(frame.scale.is_finite() && frame.scale > 0.0)
    {
        return Err("Capture has invalid dimensions or scale".into());
    }
    let length = frame
        .stride
        .checked_mul(frame.height as usize)
        .ok_or("Capture dimensions overflow")?;
    if frame.pixels.len() < length {
        return Err("Capture buffer is shorter than its dimensions".into());
    }
    Ok(())
}

fn upright_size(frame: &OutputFrame) -> (i32, i32) {
    if swaps_axes(frame.transform) {
        (frame.height, frame.width)
    } else {
        (frame.width, frame.height)
    }
}

fn logical_origin(frames: &[OutputFrame]) -> Option<(i32, i32)> {
    let mut origin: Option<(i32, i32)> = None;
    for frame in frames {
        if frame.width <= 0 || frame.height <= 0 {
            continue;
        }
        origin = Some(match origin {
            None => (frame.location.0, frame.location.1),
            Some((x, y)) => (x.min(frame.location.0), y.min(frame.location.1)),
        });
    }
    origin
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Geometry {
    All,
    Region { x: i32, y: i32, width: i32, height: i32 },
}

impl Geometry {
    // IPC arguments arrive as JSON numbers, so this is the gate that keeps a
    // negative or zero extent from reaching rectangle arithmetic.
    pub(crate) fn new_region(x: i32, y: i32, width: i32, height: i32) -> Result<Self, String> {
        if width <= 0 || height <= 0 {
            return Err("Screenshot region must have a positive size".into());
        }
        x.checked_add(width).ok_or("screenshot geometry overflows")?;
        y.checked_add(height).ok_or("screenshot geometry overflows")?;
        Ok(Geometry::Region { x, y, width, height })
    }
}

pub(crate) fn parse_geometry(raw: &str) -> Result<Geometry, String> {
    let raw = raw.trim();
    let invalid = || format!("screenshot geometry must be \"x,y WxH\": {raw:?}");
    if raw.is_empty() {
        return Err(invalid());
    }
    let (position, size) = raw.split_once(' ').ok_or_else(invalid)?;
    let (x, y) = position.split_once(',').ok_or_else(invalid)?;
    let (width, height) = size.split_once('x').ok_or_else(invalid)?;

    let x: i32 = x.trim().parse().map_err(|_| invalid())?;
    let y: i32 = y.trim().parse().map_err(|_| invalid())?;
    let width: i32 = width.trim().parse().map_err(|_| invalid())?;
    let height: i32 = height.trim().parse().map_err(|_| invalid())?;

    Geometry::new_region(x, y, width, height)
}

#[derive(Debug)]
pub(crate) struct CaptureBuffer {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) stride: usize,
    pub(crate) pixels: Vec<u8>,
}

// A capture part completes exactly once, with either pixels or an error. The
// compositor thread sends it over a channel that never fills while it is
// rendering; a dropped part would leave its request waiting indefinitely.
#[derive(Debug)]
pub(crate) struct PartOutcome {
    pub(crate) request: u64,
    pub(crate) part: usize,
    pub(crate) result: Result<CaptureBuffer, String>,
    pub(crate) permit: Option<BudgetPermit>,
}

// A calloop channel so publishing a part also wakes the event loop, whichever
// backend produced the pixels.
pub(crate) type PartSender = smithay::reexports::calloop::channel::Sender<PartOutcome>;

// Geometry snapshotted when the request was admitted, so the encoding worker
// never re-queries an output whose transform or scale has since changed.
pub(crate) struct PartSpec {
    pub(crate) preserve_alpha: bool,
    pub(crate) transform: Transform,
    pub(crate) scale: f64,
    pub(crate) location: (i32, i32),
    pub(crate) logical_width: i32,
    pub(crate) logical_height: i32,
    pub(crate) buffer_width: i32,
    pub(crate) buffer_height: i32,
}

enum PartState {
    Pending,
    Ready(OutputFrame),
}

enum RequestState {
    Collecting,
    Encoding,
}

struct Request {
    owner: u64,
    response_id: u64,
    reply: ReplySender<Response>,
    state: RequestState,
    parts: Vec<(PartSpec, PartState)>,
    received: usize,
    deadline: Instant,
    permit: Option<BudgetPermit>,
}

pub(crate) enum Action {
    None,
    Encode {
        request: u64,
        frames: Vec<OutputFrame>,
        permit: BudgetPermit,
    },
    // The path was already sent to the caller; it must not be cleaned up.
    Delivered(PathBuf),
    // The request is gone, so this file is ours to remove.
    DiscardFile(PathBuf),
}

const MAX_OUTSTANDING: usize = 8;
const MAX_PENDING_BYTES: usize = 256 * 1024 * 1024;

#[derive(Default, Debug)]
struct Budget {
    used: AtomicUsize,
}

#[derive(Debug)]
struct Reservation {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BudgetPermit {
    _reservation: Arc<Reservation>,
}

impl Budget {
    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<BudgetPermit, String> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= MAX_PENDING_BYTES)
            })
            .map_err(|_| "Screenshot memory budget is exhausted".to_string())?;
        Ok(BudgetPermit {
            _reservation: Arc::new(Reservation {
                budget: self.clone(),
                bytes,
            }),
        })
    }
}

// A request is answered or abandoned this long after it is admitted. Parts are
// produced by the render path, so an output that is never repainted would
// otherwise leave the request in Collecting forever, holding one of the few
// outstanding slots against every later request.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Coordinator {
    requests: std::collections::HashMap<u64, Request>,
    next_id: u64,
    budget: Arc<Budget>,
    deadline_changed: Option<Box<dyn Fn(Option<Instant>)>>,
    notified_deadline: Option<Instant>,
}

impl Coordinator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_deadline_observer(&mut self, observer: impl Fn(Option<Instant>) + 'static) {
        self.notified_deadline = self.next_deadline();
        observer(self.notified_deadline);
        self.deadline_changed = Some(Box::new(observer));
    }

    pub(crate) fn admit(
        &mut self,
        owner: u64,
        response_id: u64,
        reply: ReplySender<Response>,
        specs: Vec<PartSpec>,
    ) -> Result<u64, String> {
        self.admit_at(owner, response_id, reply, specs, Instant::now())
    }

    fn admit_at(
        &mut self,
        owner: u64,
        response_id: u64,
        reply: ReplySender<Response>,
        specs: Vec<PartSpec>,
        now: Instant,
    ) -> Result<u64, String> {
        if specs.is_empty() {
            return Err("No outputs are enabled".into());
        }
        if self.requests.len() >= MAX_OUTSTANDING {
            return Err("Too many screenshot requests are in flight".into());
        }
        // Budget every participating readback before the first one allocates.
        let mut pending = 0_usize;
        for spec in &specs {
            if spec.buffer_width <= 0 || spec.buffer_height <= 0 {
                return Err("Screenshot region is too large".into());
            }
            let Some(bytes) = (spec.buffer_width as usize)
                .checked_mul(BYTES_PER_PIXEL)
                .and_then(|row| row.checked_mul(spec.buffer_height as usize))
            else {
                return Err("Screenshot region is too large".into());
            };
            pending = pending.saturating_add(bytes);
        }
        if pending > MAX_PENDING_BYTES {
            return Err("Screenshot region is too large".into());
        }

        let permit = self.budget.reserve(pending)?;
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.requests.insert(
            id,
            Request {
                owner,
                response_id,
                reply,
                state: RequestState::Collecting,
                parts: specs.into_iter().map(|spec| (spec, PartState::Pending)).collect(),
                received: 0,
                deadline: now + REQUEST_TIMEOUT,
                permit: Some(permit),
            },
        );
        self.notify_deadline_change();
        Ok(id)
    }

    pub(crate) fn permit(&self, request: u64) -> Option<BudgetPermit> {
        self.requests.get(&request).and_then(|entry| entry.permit.clone())
    }

    // Used by the completion tests to assert that a request is still tracked.
    #[allow(dead_code)]
    pub(crate) fn is_live(&self, request: u64) -> bool {
        self.requests.contains_key(&request)
    }

    pub(crate) fn is_encoding(&self, request: u64) -> bool {
        matches!(
            self.requests.get(&request).map(|entry| &entry.state),
            Some(RequestState::Encoding)
        )
    }

    pub(crate) fn on_part(&mut self, request: u64, part: usize, result: Result<CaptureBuffer, String>) -> Action {
        let Some(entry) = self.requests.get_mut(&request) else {
            return Action::None;
        };
        if !matches!(entry.state, RequestState::Collecting) {
            return Action::None;
        }
        let Some((spec, slot)) = entry.parts.get_mut(part) else {
            return Action::None;
        };
        if !matches!(slot, PartState::Pending) {
            return Action::None;
        }

        let frame = match result {
            Ok(buffer) => OutputFrame {
                preserve_alpha: spec.preserve_alpha,
                width: buffer.width,
                height: buffer.height,
                stride: buffer.stride,
                pixels: buffer.pixels,
                transform: spec.transform,
                location: spec.location,
                scale: spec.scale,
                logical_width: spec.logical_width,
                logical_height: spec.logical_height,
            },
            Err(error) => {
                self.reject_inner(request, &error);
                return Action::None;
            }
        };

        let Some(entry) = self.requests.get_mut(&request) else {
            return Action::None;
        };
        entry.parts[part].1 = PartState::Ready(frame);
        entry.received += 1;
        if entry.received < entry.parts.len() {
            return Action::None;
        }

        let frames: Vec<OutputFrame> = std::mem::take(&mut entry.parts)
            .into_iter()
            .filter_map(|(_, state)| match state {
                PartState::Ready(frame) => Some(frame),
                PartState::Pending => None,
            })
            .collect();
        entry.state = RequestState::Encoding;
        Action::Encode {
            request,
            frames,
            permit: entry.permit.take().expect("collecting request owns its reservation"),
        }
    }

    pub(crate) fn on_encoded(&mut self, request: u64, result: Result<PathBuf, String>) -> Action {
        if !self.is_encoding(request) {
            // Terminated while encoding: the file is ours to clean up, and a
            // late result must not revive the request.
            return match result {
                Ok(path) => Action::DiscardFile(path),
                Err(_) => Action::None,
            };
        }
        let entry = self.remove_request(request).expect("checked above");
        match result {
            Ok(path) => {
                reply(&entry, Ok(&path));
                Action::Delivered(path)
            }
            Err(error) => {
                reply(&entry, Err(&error));
                Action::None
            }
        }
    }

    pub(crate) fn reject(&mut self, request: u64, error: &str) {
        self.reject_inner(request, error);
    }

    // Answers every request that outlived its deadline. The returned ids let the
    // caller drop readbacks that would otherwise publish into a request that no
    // longer exists.
    pub(crate) fn expire_at(&mut self, now: Instant) -> Vec<u64> {
        let expired: Vec<u64> = self
            .requests
            .iter()
            .filter(|(_, entry)| now >= entry.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.reject_inner(*id, "Screenshot timed out: an output was not redrawn in time");
        }
        expired
    }

    // Absolute deadlines do not drift when unrelated work wakes the loop.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.requests.values().map(|entry| entry.deadline).min()
    }

    pub(crate) fn terminate_owner(&mut self, owner: u64) -> Vec<u64> {
        let ids: Vec<u64> = self
            .requests
            .iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.reject_inner(*id, "Screenshot cancelled: the client disconnected");
        }
        ids
    }

    pub(crate) fn terminate_all(&mut self) -> Vec<u64> {
        self.terminate_all_with_reason("Screenshot cancelled: the session was locked")
    }

    pub(crate) fn terminate_all_with_reason(&mut self, reason: &str) -> Vec<u64> {
        let ids: Vec<u64> = self.requests.keys().copied().collect();
        for id in &ids {
            self.reject_inner(*id, reason);
        }
        ids
    }

    fn reject_inner(&mut self, request: u64, error: &str) {
        if let Some(entry) = self.remove_request(request) {
            reply(&entry, Err(error));
        }
    }

    // All terminal paths pass through here, including readback/encode failure,
    // owner closure, cancellation and timeout, so none can leave a stale timer.
    fn remove_request(&mut self, request: u64) -> Option<Request> {
        let entry = self.requests.remove(&request)?;
        self.notify_deadline_change();
        Some(entry)
    }

    fn notify_deadline_change(&mut self) {
        let deadline = self.next_deadline();
        if deadline != self.notified_deadline {
            self.notified_deadline = deadline;
            if let Some(observer) = &self.deadline_changed {
                observer(deadline);
            }
        }
    }
}

fn reply(entry: &Request, result: Result<&Path, &str>) {
    let response = match result {
        Ok(path) => Response::success(entry.response_id, serde_json::json!({ "path": path })),
        Err(error) => Response::error(entry.response_id, "screenshot_failed", error),
    };
    // The IPC worker is already blocked in recv(), so this never blocks. A
    // disconnected client simply drops the receiver.
    let _ = entry.reply.try_send(response);
}

// The layout facts the planner needs, snapshotted from a live output.
#[derive(Clone)]
pub(crate) struct OutputLayout {
    pub(crate) mode_size: Size<i32, Physical>,
    pub(crate) scale: f64,
    pub(crate) transform: Transform,
    // Global logical position of the output, as the layout sees it.
    pub(crate) location: Point<i32, Logical>,
}

impl OutputLayout {
    fn logical_size(&self) -> Size<i32, Logical> {
        crate::handlers::screencopy::output_logical_size(self.mode_size, self.scale, self.transform)
    }
}

pub(crate) struct PlannedPart {
    // Index of the contributing output in the caller's list.
    pub(crate) index: usize,
    // The clipped global logical rectangle, in the upright orientation the
    // caller sees. Composition places by this, so it is never recovered from
    // the readback dimensions.
    pub(crate) logical: Rectangle<i32, Logical>,
    // The readback rectangle, relative to this output's buffer origin.
    pub(crate) buffer: Rectangle<i32, Buffer>,
}

impl PlannedPart {
    pub(crate) fn spec(&self, output: &OutputLayout) -> PartSpec {
        PartSpec {
            preserve_alpha: false,
            transform: output.transform,
            scale: output.scale,
            location: (self.logical.loc.x, self.logical.loc.y),
            logical_width: self.logical.size.w,
            logical_height: self.logical.size.h,
            buffer_width: self.buffer.size.w,
            buffer_height: self.buffer.size.h,
        }
    }
}

// Turns a requested logical region into the per-output reads that cover it.
// The logical-to-buffer conversion is shared with the screencopy path so both
// agree on rounding at fractional scales.
pub(crate) fn plan(geometry: &Geometry, outputs: &[OutputLayout]) -> Result<Vec<PlannedPart>, String> {
    if outputs.is_empty() {
        return Err("No outputs are enabled".into());
    }
    // Validate before building any Rectangle: Size construction panics on a
    // negative extent, and this input arrives over IPC.
    if let Geometry::Region { width, height, .. } = geometry
        && (*width <= 0 || *height <= 0)
    {
        return Err("Screenshot region must have a positive size".into());
    }
    let logical_sizes: Vec<Size<i32, Logical>> = outputs.iter().map(OutputLayout::logical_size).collect();

    let requested = match geometry {
        Geometry::All => {
            let mut union: Option<Rectangle<i32, Logical>> = None;
            for (index, output) in outputs.iter().enumerate() {
                let rect = Rectangle {
                    loc: output.location,
                    size: logical_sizes[index],
                };
                union = Some(match union {
                    Some(current) => {
                        let top_left =
                            Point::<i32, Logical>::new(current.loc.x.min(rect.loc.x), current.loc.y.min(rect.loc.y));
                        let bottom_right = Point::<i32, Logical>::new(
                            (current.loc.x + current.size.w).max(rect.loc.x + rect.size.w),
                            (current.loc.y + current.size.h).max(rect.loc.y + rect.size.h),
                        );
                        Rectangle::from_extremities(top_left, bottom_right)
                    }
                    None => rect,
                });
            }
            union.expect("outputs is not empty")
        }
        Geometry::Region { x, y, width, height } => Rectangle {
            loc: (*x, *y).into(),
            size: (*width, *height).into(),
        },
    };
    if requested.size.w <= 0 || requested.size.h <= 0 {
        return Err("Screenshot region must have a positive size".into());
    }

    let mut parts = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        if !(output.scale.is_finite() && output.scale > 0.0) {
            return Err("Output scale is not usable".into());
        }
        let output_rect = Rectangle {
            loc: output.location,
            size: logical_sizes[index],
        };
        let Some(clipped) = requested.intersection(output_rect) else {
            continue;
        };
        // Smithay's intersection already excludes a merely adjacent output, but
        // a degenerate clip must never reach the hard failure below.
        if clipped.size.w <= 0 || clipped.size.h <= 0 {
            continue;
        }
        // capture_region_for_geometry works in output-local logical space.
        let local = Rectangle {
            loc: (clipped.loc.x - output.location.x, clipped.loc.y - output.location.y).into(),
            size: clipped.size,
        };
        // The clip above already intersected against this same logical rect, so
        // a failure here means the two paths disagree. Report it rather than
        // quietly returning a smaller image than was asked for.
        let Some(buffer) = crate::handlers::screencopy::capture_region_for_geometry(
            output.mode_size,
            output.scale,
            output.transform,
            Some(local),
        ) else {
            return Err(format!(
                "Could not compute a capture region for the output at {}x{}",
                output.location.x, output.location.y
            ));
        };
        parts.push(PlannedPart {
            index,
            logical: clipped,
            buffer,
        });
    }
    if parts.is_empty() {
        return Err("The requested region does not intersect any enabled output".into());
    }
    Ok(parts)
}

#[cfg(test)]
mod planning {
    use super::*;

    fn output(x: i32, y: i32, width: i32, height: i32, scale: f64) -> OutputLayout {
        let mode = Size::<i32, Physical>::new(
            (f64::from(width) * scale).round() as i32,
            (f64::from(height) * scale).round() as i32,
        );
        OutputLayout {
            mode_size: mode,
            scale,
            transform: Transform::Normal,
            location: Point::new(x, y),
        }
    }

    fn region(x: i32, y: i32, width: i32, height: i32) -> Geometry {
        Geometry::Region { x, y, width, height }
    }

    #[test]
    fn a_crop_crossing_a_seam_splits_into_one_part_per_output() {
        let outputs = [output(0, 0, 1920, 1080, 1.0), output(1920, 0, 1920, 1080, 1.0)];
        let parts = plan(&region(1800, 0, 200, 100), &outputs).unwrap();
        assert_eq!(parts.len(), 2);

        assert_eq!(parts[0].index, 0);
        assert_eq!((parts[0].logical.loc.x, parts[0].logical.size.w), (1800, 120));
        assert_eq!(
            (parts[1].index, parts[1].logical.loc.x, parts[1].logical.size.w),
            (1, 1920, 80)
        );
        // The clipped widths must add back up to the request.
        assert_eq!(parts[0].logical.size.w + parts[1].logical.size.w, 200);

        // Readbacks are rebased onto each output's own buffer origin.
        assert_eq!((parts[0].buffer.loc.x, parts[0].buffer.size.w), (1800, 120));
        assert_eq!((parts[1].buffer.loc.x, parts[1].buffer.size.w), (0, 80));
    }

    #[test]
    fn a_spec_carries_the_clipped_logical_rect_not_the_readback() {
        // A fractional-scale output readbacks more pixels than logical*scale
        // exactly, so the spec must not be rebuilt from the buffer size.
        let outputs = [output(0, 0, 3, 3, 1.5)];
        let parts = plan(&region(0, 0, 3, 3), &outputs).unwrap();
        let spec = parts[0].spec(&outputs[0]);
        assert_eq!((spec.logical_width, spec.logical_height), (3, 3));
        assert_eq!(spec.location, (0, 0));
        assert_eq!(spec.scale, 1.5);
        assert_ne!(
            (spec.buffer_width, spec.buffer_height),
            (spec.logical_width, spec.logical_height),
            "this fixture only makes sense if the readback is scaled up"
        );
    }

    #[test]
    fn mixed_scale_keeps_both_clips_in_logical_space() {
        let outputs = [output(0, 0, 800, 600, 1.0), output(800, 0, 1600, 1200, 1.5)];
        let parts = plan(&region(600, 0, 400, 300), &outputs).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].logical.loc.x, parts[0].logical.size.w), (600, 200));
        assert_eq!((parts[1].logical.loc.x, parts[1].logical.size.w), (800, 200));
        // The 1.5 output reads back at 1.5x while still reporting logical 200.
        let spec = parts[1].spec(&outputs[1]);
        assert_eq!(spec.logical_width, 200);
        assert_eq!(spec.buffer_width, 300);
        assert_eq!(spec.buffer_height, 450);
    }

    #[test]
    fn all_geometry_unions_every_output() {
        let outputs = [output(0, 0, 1920, 1080, 1.0), output(-1280, -400, 1280, 800, 1.0)];
        let parts = plan(&Geometry::All, &outputs).unwrap();
        assert_eq!(parts.len(), 2);
        // Neither output is moved into a normalised origin at plan time.
        assert_eq!(parts[0].logical.loc.x, 0);
        assert_eq!(parts[1].logical.loc.x, -1280);
        assert_eq!(parts[1].logical.loc.y, -400);
    }

    #[test]
    fn a_crop_misses_outputs_that_do_not_intersect() {
        let outputs = [output(0, 0, 1920, 1080, 1.0), output(1920, 0, 1920, 1080, 1.0)];
        let parts = plan(&region(4000, 0, 100, 100), &outputs);
        assert!(parts.is_err(), "a region off the desktop is reported");
    }

    #[test]
    fn an_output_adjacent_to_the_region_is_skipped_not_fatal() {
        // The second output starts exactly where the region ends. It contributes
        // nothing, so the capture must still succeed with just the first output
        // rather than failing on a region it does not overlap.
        let outputs = [output(0, 0, 1920, 1080, 1.0), output(1920, 0, 1920, 1080, 1.0)];
        let parts = plan(&region(100, 100, 1820, 980), &outputs).expect("captures the overlap");
        assert_eq!(parts.len(), 1, "only the overlapping output is read");
        assert_eq!(parts[0].index, 0);
        assert_eq!((parts[0].logical.size.w, parts[0].logical.size.h), (1820, 980));
    }

    #[test]
    fn a_rotated_output_reads_back_with_swapped_axes() {
        let mut rotated = output(0, 0, 1920, 1080, 1.0);
        rotated.transform = Transform::_90;
        // A 90-degree transform turns the 1920x1080 mode into a 1080x1920
        // logical area, and the readback follows the same rotation.
        let parts = plan(&region(0, 0, 1080, 400), std::slice::from_ref(&rotated)).unwrap();
        assert_eq!((parts[0].logical.size.w, parts[0].logical.size.h), (1080, 400));
        let spec = parts[0].spec(&rotated);
        assert_eq!(spec.transform, Transform::_90);
        assert!(spec.buffer_height > spec.buffer_width, "buffer axes are swapped");
    }

    #[test]
    fn the_region_constructor_validates_instead_of_panicking() {
        // Size construction panics on a negative extent, so this must never
        // reach rectangle arithmetic.
        assert!(Geometry::new_region(0, 0, 100, -5).is_err());
        assert!(Geometry::new_region(0, 0, 0, 100).is_err());
        assert!(Geometry::new_region(i32::MAX, 0, 2, 2).is_err());
        assert!(Geometry::new_region(0, i32::MAX, 2, 2).is_err());
        assert!(
            Geometry::new_region(0, i32::MIN, 2, 2).is_ok(),
            "a large negative origin is representable"
        );
        let ok = Geometry::new_region(-5, -7, 10, 20).expect("negative origins are fine");
        assert_eq!(
            ok,
            Geometry::Region {
                x: -5,
                y: -7,
                width: 10,
                height: 20
            }
        );
        // The parser goes through the same gate.
        assert!(parse_geometry("0,0 100x-5").is_err());
    }

    #[test]
    fn a_non_positive_or_empty_request_is_rejected() {
        let outputs = [output(0, 0, 1920, 1080, 1.0)];
        assert!(plan(&region(0, 0, 0, 100), &outputs).is_err());
        assert!(plan(&region(0, 0, 100, -5), &outputs).is_err());
        assert!(plan(&Geometry::All, &[]).is_err());
    }
}

#[cfg(test)]
mod completion {
    use std::sync::mpsc::sync_channel;

    use smithay::reexports::calloop::channel::channel as loop_channel;

    use super::*;
    use crate::handlers::screencopy::CaptureSink;

    fn spec(width: i32, height: i32) -> PartSpec {
        PartSpec {
            preserve_alpha: false,
            transform: Transform::Normal,
            scale: 1.0,
            location: (0, 0),
            logical_width: width,
            logical_height: height,
            buffer_width: width,
            buffer_height: height,
        }
    }

    fn buffer(width: i32, height: i32) -> CaptureBuffer {
        let stride = width as usize * BYTES_PER_PIXEL;
        CaptureBuffer {
            width,
            height,
            stride,
            pixels: vec![0u8; stride * height as usize],
        }
    }

    struct Harness {
        coordinator: Coordinator,
        received: std::sync::mpsc::Receiver<Response>,
    }

    impl Harness {
        fn new(parts: usize) -> Self {
            let (reply, received) = sync_channel(8);
            let mut coordinator = Coordinator::new();
            coordinator
                .admit(1, 42, reply.clone(), (0..parts).map(|_| spec(2, 2)).collect())
                .expect("admitted");
            Self { coordinator, received }
        }

        fn response(&self) -> Option<Response> {
            self.received.try_recv().ok()
        }
    }

    #[test]
    fn completed_readback_moves_its_pixels_to_the_encoder() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        let mut captured = buffer(2, 2);
        captured.pixels.fill(137);
        let allocation = captured.pixels.as_ptr();

        let Action::Encode { frames, .. } = harness.coordinator.on_part(id, 0, Ok(captured)) else {
            panic!("completed readback was not queued for encoding");
        };

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].pixels.as_ptr(), allocation, "readback pixels were copied");
        assert_eq!(frames[0].pixels, vec![137; 16]);
    }

    #[test]
    fn part_failure_terminates_the_request_with_an_error() {
        let mut harness = Harness::new(2);
        let id = harness.coordinator.next_id;

        assert!(matches!(
            harness.coordinator.on_part(id, 0, Ok(buffer(2, 2))),
            Action::None
        ));
        assert!(harness.coordinator.is_live(id), "still collecting after part 0");

        assert!(matches!(
            harness.coordinator.on_part(id, 1, Err("readback failed".into())),
            Action::None
        ));
        assert!(!harness.coordinator.is_live(id), "one failed part ends the request");

        let response = harness.response().expect("caller is always answered");
        assert_eq!(response.id, 42);
        assert!(response.result.is_none());
        assert_eq!(response.error.unwrap().code, "screenshot_failed");
    }

    #[test]
    fn a_disappearing_output_fails_the_request_and_discards_late_results() {
        let mut harness = Harness::new(2);
        let id = harness.coordinator.next_id;

        harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));
        // Output 1 is removed from the layout, so its pending capture is failed.
        harness.coordinator.on_part(id, 1, Err("output was removed".into()));
        assert!(!harness.coordinator.is_live(id));
        assert!(harness.response().unwrap().error.is_some());

        // A late readback for the sibling must not revive the request.
        assert!(matches!(
            harness.coordinator.on_part(id, 0, Ok(buffer(2, 2))),
            Action::None
        ));
        assert!(harness.response().is_none(), "no second response is sent");
    }

    #[test]
    fn a_part_completes_at_most_once() {
        let mut harness = Harness::new(2);
        let id = harness.coordinator.next_id;

        harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));
        // A duplicate publication for the same part is ignored rather than
        // overwriting pixels that were already accepted.
        assert!(matches!(
            harness.coordinator.on_part(id, 0, Ok(buffer(64, 64))),
            Action::None
        ));
        assert!(matches!(
            harness.coordinator.on_part(id, 1, Ok(buffer(2, 2))),
            Action::Encode { .. }
        ));
        // An unknown part index is ignored entirely.
        assert!(matches!(
            harness.coordinator.on_part(id, 99, Ok(buffer(2, 2))),
            Action::None
        ));
    }

    #[test]
    fn admission_enforces_one_budget_across_requests_and_late_readbacks() {
        let (reply, _received) = sync_channel(8);
        let mut coordinator = Coordinator::new();
        let first = coordinator.admit(1, 1, reply.clone(), vec![spec(4096, 8192)]).unwrap();
        let second = coordinator.admit(1, 2, reply.clone(), vec![spec(4096, 8192)]).unwrap();
        assert!(coordinator.admit(1, 3, reply.clone(), vec![spec(2, 2)]).is_err());

        // A cancelled request can still have a readback retained by a sink or channel.
        let late_readback = coordinator.permit(first).unwrap();
        coordinator.reject(first, "cancelled");
        assert!(coordinator.admit(1, 4, reply.clone(), vec![spec(2, 2)]).is_err());
        drop(late_readback);
        assert!(coordinator.admit(1, 5, reply, vec![spec(2, 2)]).is_ok());
        coordinator.reject(second, "finished");
    }

    #[test]
    fn aggregate_reservations_survive_request_cancellation_during_encoding() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        let bytes = harness.coordinator.budget.used.load(Ordering::Acquire);
        let Action::Encode { frames, permit, .. } = harness.coordinator.on_part(id, 0, Ok(buffer(2, 2))) else {
            panic!("ready request should encode");
        };
        harness.coordinator.reject(id, "cancelled while encoder owns pixels");
        assert_eq!(harness.coordinator.budget.used.load(Ordering::Acquire), bytes);
        assert!(harness.coordinator.budget.reserve(MAX_PENDING_BYTES).is_err());
        drop(frames);
        drop(permit);
        assert_eq!(harness.coordinator.budget.used.load(Ordering::Acquire), 0);
        let first = harness.coordinator.budget.reserve(MAX_PENDING_BYTES / 2 + 1).unwrap();
        assert!(harness.coordinator.budget.reserve(MAX_PENDING_BYTES / 2).is_err());
        drop(first);
        assert!(harness.coordinator.budget.reserve(MAX_PENDING_BYTES).is_ok());
    }

    #[test]
    fn all_parts_ready_hands_frames_to_the_encoder() {
        let mut harness = Harness::new(2);
        let id = harness.coordinator.next_id;

        assert!(matches!(
            harness.coordinator.on_part(id, 0, Ok(buffer(2, 2))),
            Action::None
        ));
        let action = harness.coordinator.on_part(id, 1, Ok(buffer(2, 2)));
        let Action::Encode { request, frames, .. } = action else {
            panic!("expected the request to move to encoding");
        };
        assert_eq!(request, id);
        assert_eq!(frames.len(), 2);
        assert!(harness.coordinator.is_encoding(id));
        assert!(harness.response().is_none(), "no reply until encoding ends");
    }

    #[test]
    fn lock_during_encoding_cancels_and_the_late_file_is_removed() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));
        assert!(harness.coordinator.is_encoding(id));

        // The session locks while the worker is still encoding.
        let terminated = harness.coordinator.terminate_all();
        assert_eq!(terminated, vec![id]);
        let response = harness.response().expect("cancellation is answered");
        assert!(response.error.is_some());

        // Unlocking does not resurrect the request; the file the worker
        // eventually produces is ours to delete.
        assert!(!harness.coordinator.is_live(id));
        let action = harness.coordinator.on_encoded(id, Ok(PathBuf::from("/tmp/late.png")));
        assert!(matches!(action, Action::DiscardFile(ref path) if path.ends_with("late.png")));
        assert!(harness.response().is_none(), "no late success is delivered");
    }

    #[test]
    fn privacy_change_cancels_encoding_and_discards_late_results() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));
        assert!(harness.coordinator.is_encoding(id));
        assert_eq!(
            harness.coordinator.terminate_all_with_reason("Capture privacy changed"),
            vec![id]
        );
        assert!(harness.response().unwrap().error.is_some());
        assert!(matches!(
            harness
                .coordinator
                .on_encoded(id, Ok(PathBuf::from("/tmp/private.png"))),
            Action::DiscardFile(_)
        ));
        assert!(harness.response().is_none());
    }

    #[test]
    fn encoded_result_is_delivered_once() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));

        harness.coordinator.on_encoded(id, Ok(PathBuf::from("/tmp/shot.png")));
        let response = harness.response().expect("success is answered");
        assert_eq!(response.id, 42);
        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["path"], "/tmp/shot.png");

        // A duplicate completion for the same request has nowhere to go, and
        // its file is an orphan rather than a second delivery.
        let action = harness.coordinator.on_encoded(id, Ok(PathBuf::from("/tmp/again.png")));
        assert!(matches!(action, Action::DiscardFile(_)));

        // A delivered path is reported separately so the caller cannot
        // mistake it for a file to delete.
        let mut other = Harness::new(1);
        let other_id = other.coordinator.next_id;
        other.coordinator.on_part(other_id, 0, Ok(buffer(2, 2)));
        assert!(matches!(
            other
                .coordinator
                .on_encoded(other_id, Ok(PathBuf::from("/tmp/live.png"))),
            Action::Delivered(ref path) if path.ends_with("live.png")
        ));
        assert!(harness.response().is_none());
    }

    #[test]
    fn encoder_failure_is_reported_to_the_caller() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));

        harness.coordinator.on_encoded(id, Err("encoding failed".into()));
        let response = harness.response().expect("failure is answered");
        assert_eq!(response.error.unwrap().message, "encoding failed");
        assert!(!harness.coordinator.is_live(id));
    }

    #[test]
    fn a_request_whose_output_is_never_redrawn_is_abandoned() {
        // Nothing ever calls on_part, which is what an unrendered output does.
        // Without a deadline the request would hold its slot forever and the
        // client would never be answered.
        let mut harness = Harness::new(2);
        let id = harness.coordinator.next_id;
        harness.coordinator.requests.get_mut(&id).expect("tracked").deadline = Instant::now() - Duration::from_secs(1);

        assert_eq!(harness.coordinator.expire_at(Instant::now()), vec![id]);
        let response = harness.response().expect("the caller is answered");
        assert_eq!(response.error.as_ref().unwrap().code, "screenshot_failed");
        assert!(
            response.error.as_ref().unwrap().message.contains("timed out"),
            "the failure explains itself: {:?}",
            response.error.as_ref().unwrap().message
        );
        assert!(!harness.coordinator.is_live(id));
        // A late part for the abandoned request is discarded, not revived.
        assert!(matches!(
            harness.coordinator.on_part(id, 0, Ok(buffer(2, 2))),
            Action::None
        ));
        assert!(harness.response().is_none());
    }

    #[test]
    fn a_live_request_is_not_expired_early() {
        let mut harness = Harness::new(1);
        let id = harness.coordinator.next_id;
        assert!(harness.coordinator.expire_at(Instant::now()).is_empty(), "not yet due");
        assert!(harness.coordinator.is_live(id));
        assert!(harness.response().is_none(), "still unanswered, not failed");
        let remaining = harness
            .coordinator
            .next_deadline()
            .expect("still armed")
            .saturating_duration_since(Instant::now());
        assert!(
            remaining <= REQUEST_TIMEOUT && remaining > REQUEST_TIMEOUT - Duration::from_secs(1),
            "the timer re-arms to roughly the full timeout, got {remaining:?}"
        );
    }

    #[test]
    fn expiring_frees_the_outstanding_slot() {
        let (reply, _received) = sync_channel(1);
        let mut coordinator = Coordinator::new();
        for _ in 0..MAX_OUTSTANDING {
            coordinator
                .admit(1, 42, reply.clone(), vec![spec(2, 2)])
                .expect("within the request cap");
        }
        for entry in coordinator.requests.values_mut() {
            entry.deadline = Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(coordinator.expire_at(Instant::now()).len(), MAX_OUTSTANDING);
        assert_eq!(coordinator.next_deadline(), None);
        // The cap is usable again rather than permanently degraded.
        coordinator
            .admit(1, 42, reply, vec![spec(2, 2)])
            .expect("a slot was released");
    }

    #[test]
    fn admission_is_bounded_by_requests_and_bytes() {
        let (reply, _received) = sync_channel(1);
        let mut coordinator = Coordinator::new();
        for _ in 0..MAX_OUTSTANDING {
            coordinator
                .admit(1, 42, reply.clone(), vec![spec(2, 2)])
                .expect("within the request cap");
        }
        let overflow = coordinator.admit(1, 42, reply.clone(), vec![spec(2, 2)]);
        assert!(overflow.is_err(), "outstanding requests are bounded");

        let huge = PartSpec {
            preserve_alpha: false,
            buffer_width: 1 << 20,
            buffer_height: 1 << 20,
            ..spec(2, 2)
        };
        let mut fresh = Coordinator::new();
        assert!(
            fresh.admit(1, 42, reply.clone(), vec![huge]).is_err(),
            "the byte budget is checked before any readback allocates"
        );
    }

    fn observe_deadlines(coordinator: &mut Coordinator) -> std::rc::Rc<std::cell::RefCell<Vec<Option<Instant>>>> {
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = changes.clone();
        coordinator.set_deadline_observer(move |deadline| observed.borrow_mut().push(deadline));
        changes
    }

    #[test]
    fn deadline_admission_after_long_idle_and_earliest_removal_rearm_exactly() {
        let now = Instant::now();
        let (reply, _received) = sync_channel(8);
        let mut coordinator = Coordinator::new();
        let changes = observe_deadlines(&mut coordinator);
        assert_eq!(&*changes.borrow(), &[None]);

        // An idle period does not establish a polling epoch. Every request gets
        // exactly the timeout measured from its own admission.
        let late = now + Duration::from_secs(3600);
        let first = coordinator
            .admit_at(1, 42, reply.clone(), vec![spec(2, 2)], late)
            .unwrap();
        let second = coordinator
            .admit_at(2, 43, reply.clone(), vec![spec(2, 2)], late + Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            changes.borrow().len(),
            2,
            "a later request does not rearm the earliest timer"
        );
        assert!(coordinator.admit_at(1, 44, reply, Vec::new(), late).is_err());
        assert_eq!(changes.borrow().len(), 2, "rejected admission does not touch the timer");
        assert_eq!(coordinator.terminate_owner(1), vec![first]);
        assert!(coordinator.is_live(second));
        assert_eq!(coordinator.terminate_owner(99), Vec::<u64>::new());
        coordinator.reject(second, "encoding queue is full");
        assert_eq!(
            &*changes.borrow(),
            &[
                None,
                Some(late + REQUEST_TIMEOUT),
                Some(late + Duration::from_secs(5) + REQUEST_TIMEOUT),
                None,
            ]
        );
    }

    #[test]
    fn deadline_survives_readback_and_encoding_then_disarms_on_completion() {
        for result in [Ok(PathBuf::from("/tmp/shot.png")), Err("encoder failed".into())] {
            let mut harness = Harness::new(2);
            let changes = observe_deadlines(&mut harness.coordinator);
            let id = harness.coordinator.next_id;
            let deadline = harness.coordinator.next_deadline();
            harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));
            assert!(matches!(
                harness.coordinator.on_part(id, 1, Ok(buffer(2, 2))),
                Action::Encode { .. }
            ));
            assert_eq!(&*changes.borrow(), &[deadline], "encoding keeps the original deadline");
            harness.coordinator.on_encoded(id, result);
            assert_eq!(&*changes.borrow(), &[deadline, None]);
            harness.coordinator.on_part(id, 0, Err("late readback".into()));
            harness.coordinator.on_encoded(id, Err("late encode".into()));
            assert_eq!(
                changes.borrow().len(),
                2,
                "late results cannot rearm an empty coordinator"
            );
        }
    }

    #[test]
    fn deadline_cancellations_disarm_collecting_and_encoding_requests() {
        for encoding in [false, true] {
            for cancellation in [
                "output was removed",
                "session locked",
                "privacy changed",
                "client closed",
            ] {
                // Topology failures arrive as parts while collecting. Once
                // encoding begins its snapshotted frames remain valid.
                if encoding && cancellation == "output was removed" {
                    continue;
                }
                let mut harness = Harness::new(1);
                let changes = observe_deadlines(&mut harness.coordinator);
                let id = harness.coordinator.next_id;
                if encoding {
                    harness.coordinator.on_part(id, 0, Ok(buffer(2, 2)));
                }
                match cancellation {
                    "output was removed" => {
                        harness.coordinator.on_part(id, 0, Err(cancellation.into()));
                    }
                    "session locked" => {
                        harness.coordinator.terminate_all();
                    }
                    "privacy changed" => {
                        harness.coordinator.terminate_all_with_reason(cancellation);
                    }
                    "client closed" => {
                        harness.coordinator.terminate_owner(1);
                    }
                    _ => unreachable!(),
                }
                assert_eq!(changes.borrow().last(), Some(&None), "{cancellation} must disarm");
                assert_eq!(changes.borrow().len(), 2);
                assert!(harness.response().unwrap().error.is_some());
                assert!(!harness.coordinator.is_live(id));
            }
        }
    }

    #[test]
    fn deadline_expiry_moves_to_next_request_and_frees_encoding_slots() {
        let now = Instant::now();
        let (reply, received) = sync_channel(8);
        let mut coordinator = Coordinator::new();
        let changes = observe_deadlines(&mut coordinator);
        let first = coordinator
            .admit_at(1, 42, reply.clone(), vec![spec(2, 2)], now)
            .unwrap();
        let second = coordinator
            .admit_at(2, 43, reply.clone(), vec![spec(2, 2)], now + Duration::from_secs(5))
            .unwrap();
        coordinator.on_part(first, 0, Ok(buffer(2, 2)));
        assert!(
            coordinator
                .expire_at(now + REQUEST_TIMEOUT - Duration::from_nanos(1))
                .is_empty()
        );
        assert_eq!(coordinator.expire_at(now + REQUEST_TIMEOUT), vec![first]);
        assert!(coordinator.is_live(second));
        assert_eq!(
            coordinator.expire_at(now + REQUEST_TIMEOUT + Duration::from_secs(5)),
            vec![second]
        );
        assert_eq!(
            &*changes.borrow(),
            &[
                None,
                Some(now + REQUEST_TIMEOUT),
                Some(now + REQUEST_TIMEOUT + Duration::from_secs(5)),
                None,
            ]
        );
        assert_eq!(received.try_iter().count(), 2, "both timeouts answer once");
        assert!(matches!(
            coordinator.on_encoded(first, Ok(PathBuf::from("/tmp/late.png"))),
            Action::DiscardFile(_)
        ));
        coordinator
            .admit_at(3, 44, reply, vec![spec(2, 2)], now + Duration::from_secs(3600))
            .unwrap();
        assert_eq!(
            changes.borrow().last(),
            Some(&Some(now + Duration::from_secs(3600) + REQUEST_TIMEOUT))
        );
    }

    #[test]
    fn a_sink_publishes_once_and_then_rejects_duplicates() {
        let (sender, receiver) = loop_channel();
        let sink = CaptureSink::owned(7, 1, PartSender::clone(&sender));
        assert!(sink.publish(buffer(2, 2)));
        // A second publication for the same part is refused, so the first
        // buffer that was handed off is never replaced.
        assert!(!sink.publish(buffer(64, 64)));

        let PartOutcome {
            request, part, result, ..
        } = receiver.try_recv().expect("published once");
        assert_eq!((request, part), (7, 1));
        assert_eq!(result.unwrap().width, 2);
        assert!(receiver.try_recv().is_err(), "no second buffer was sent");
    }

    #[test]
    fn failing_an_owned_sink_answers_the_part_with_an_error() {
        let (sender, receiver) = loop_channel();
        let sink = CaptureSink::owned(3, 0, PartSender::clone(&sender));
        sink.fail();
        // A capture that later completes cannot publish after the terminal
        // failure has already been reported.
        assert!(!sink.publish(buffer(2, 2)));

        let PartOutcome {
            request, part, result, ..
        } = receiver.try_recv().expect("answered");
        assert_eq!((request, part), (3, 0));
        assert!(result.is_err());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn completion_channel_keeps_more_parts_than_the_old_capacity() {
        let (sender, receiver) = loop_channel();
        for part in 0..65 {
            let sink = CaptureSink::owned(1, part, sender.clone());
            assert!(sink.publish(buffer(2, 2)));
        }
        for part in 0..65 {
            let outcome = receiver.try_recv().expect("every part was queued");
            assert_eq!((outcome.request, outcome.part), (1, part));
        }
        assert!(receiver.try_recv().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labelled_frame(transform: Transform, width: i32, height: i32) -> OutputFrame {
        let mut pixels = Vec::new();
        for index in 0..(width * height) {
            // First byte of each pixel is a unique row-major index.
            pixels.extend_from_slice(&[b'a' + index as u8, 0, 0, 255]);
        }
        OutputFrame {
            preserve_alpha: false,
            width,
            height,
            stride: width as usize * BYTES_PER_PIXEL,
            pixels,
            transform,
            location: (0, 0),
            scale: 1.0,
            logical_width: width,
            logical_height: height,
        }
    }

    fn decode(frame: &OutputFrame, from: usize) -> &'static str {
        const CELLS: [&str; 6] = ["a", "b", "c", "d", "e", "f"];
        let index = (frame.pixels[from] - b'a') as usize;
        CELLS.get(index).copied().unwrap_or("?")
    }

    fn frame(transform: Transform, width: i32, height: i32) -> OutputFrame {
        // Row-major fill with a distinct byte per pixel; the fixture is
        // asymmetric so a missing rotation cannot cancel out.
        let mut pixels = Vec::new();
        for index in 0..(width * height) {
            pixels.extend_from_slice(&[(index % 251) as u8, 0, 0, 255]);
        }
        OutputFrame {
            preserve_alpha: false,
            width,
            height,
            stride: width as usize * BYTES_PER_PIXEL,
            pixels,
            transform,
            location: (0, 0),
            scale: 1.0,
            logical_width: width,
            logical_height: height,
        }
    }

    #[test]
    fn upright_orientation_matches_hand_derived_layouts() {
        let cases: [(Transform, Vec<&str>); 8] = [
            (Transform::Normal, vec!["a", "b", "c", "d", "e", "f"]),
            (Transform::_90, vec!["d", "a", "e", "b", "f", "c"]),
            (Transform::_180, vec!["f", "e", "d", "c", "b", "a"]),
            (Transform::_270, vec!["c", "f", "b", "e", "a", "d"]),
            (Transform::Flipped, vec!["c", "b", "a", "f", "e", "d"]),
            (Transform::Flipped90, vec!["a", "d", "b", "e", "c", "f"]),
            (Transform::Flipped180, vec!["d", "e", "f", "a", "b", "c"]),
            (Transform::Flipped270, vec!["f", "c", "e", "b", "d", "a"]),
        ];

        for (transform, expected) in cases {
            let f = labelled_frame(transform, 3, 2);
            let (upright_width, upright_height) = upright_size(&f);
            let mut got = vec![""; (upright_width * upright_height) as usize];
            for y in 0..upright_height {
                for x in 0..upright_width {
                    let (sx, sy) = upright_source_pixel(x, y, f.width, f.height, f.transform);
                    assert!(
                        (0..f.width).contains(&sx) && (0..f.height).contains(&sy),
                        "{transform:?} maps ({x},{y}) outside the source"
                    );
                    let from = (sy * f.width + sx) as usize * BYTES_PER_PIXEL;
                    got[(y * upright_width + x) as usize] = decode(&f, from);
                }
            }
            assert_eq!(got, expected, "wrong upright layout for {transform:?}");
        }
    }

    #[test]
    fn upright_orientation_holds_for_the_transposed_fixture() {
        // The 2x3 fixture distinguishes a rotation from a transposition, which
        // the 3x2 fixture cannot.
        let cases: [(Transform, Vec<&str>); 8] = [
            (Transform::Normal, vec!["a", "b", "c", "d", "e", "f"]),
            (Transform::_90, vec!["e", "c", "a", "f", "d", "b"]),
            (Transform::_180, vec!["f", "e", "d", "c", "b", "a"]),
            (Transform::_270, vec!["b", "d", "f", "a", "c", "e"]),
            (Transform::Flipped, vec!["b", "a", "d", "c", "f", "e"]),
            (Transform::Flipped90, vec!["a", "c", "e", "b", "d", "f"]),
            (Transform::Flipped180, vec!["e", "f", "c", "d", "a", "b"]),
            (Transform::Flipped270, vec!["f", "d", "b", "e", "c", "a"]),
        ];

        for (transform, expected) in cases {
            let f = labelled_frame(transform, 2, 3);
            let (upright_width, upright_height) = upright_size(&f);
            let mut got = vec![""; (upright_width * upright_height) as usize];
            for y in 0..upright_height {
                for x in 0..upright_width {
                    let (sx, sy) = upright_source_pixel(x, y, f.width, f.height, f.transform);
                    assert!(
                        (0..f.width).contains(&sx) && (0..f.height).contains(&sy),
                        "{transform:?} maps ({x},{y}) outside the source"
                    );
                    let from = (sy * f.width + sx) as usize * BYTES_PER_PIXEL;
                    got[(y * upright_width + x) as usize] = decode(&f, from);
                }
            }
            assert_eq!(got, expected, "wrong upright layout for {transform:?}");
        }
    }

    #[test]
    fn every_transform_is_a_bijection_over_the_source() {
        for transform in [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ] {
            let f = frame(transform, 3, 2);
            let (upright_width, upright_height) = upright_size(&f);
            let mut seen = vec![false; (f.width * f.height) as usize];
            for y in 0..upright_height {
                for x in 0..upright_width {
                    let (sx, sy) = upright_source_pixel(x, y, f.width, f.height, f.transform);
                    let index = (sy * f.width + sx) as usize;
                    assert!(!seen[index], "{transform:?} reused source pixel {index}");
                    seen[index] = true;
                }
            }
            assert!(seen.iter().all(|hit| *hit), "{transform:?} dropped pixels");
        }
    }

    #[test]
    fn rotated_output_swaps_axis_but_keeps_size_shape() {
        let f = frame(Transform::_90, 3, 2);
        assert_eq!(upright_size(&f), (2, 3));
    }

    #[test]
    fn conversion_swaps_red_and_blue_and_forces_opaque_alpha() {
        let mut f = frame(Transform::Normal, 1, 1);
        f.pixels = vec![0x11, 0x22, 0x33, 0x00]; // B, G, R, A
        let out = scale_and_convert(&f, 1, 1).unwrap();
        assert_eq!(out, vec![0x33, 0x22, 0x11, 255]);
    }

    #[test]
    fn conversion_does_not_flip_vertically() {
        // Top row red, bottom row blue. An extra Y flip would swap them.
        let mut f = frame(Transform::Normal, 1, 2);
        f.pixels = vec![0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00];
        let out = scale_and_convert(&f, 1, 2).unwrap();
        assert_eq!(&out[0..4], &[0xFF, 0x00, 0x00, 255], "top row");
        assert_eq!(&out[4..8], &[0x00, 0x00, 0xFF, 255], "bottom row");
    }

    #[test]
    fn conversion_scales_to_target_and_handles_stride() {
        let mut f = frame(Transform::Normal, 2, 2);
        // Padded stride, as an owned buffer would carry.
        f.stride = 2 * BYTES_PER_PIXEL + 4;
        let mut padded = Vec::new();
        for row in 0..2 {
            for column in 0..2 {
                let value = (row * 2 + column) as u8;
                padded.extend_from_slice(&[value, value, value, 255]);
            }
            padded.extend_from_slice(&[0xEE; 4]);
        }
        f.pixels = padded;

        let out = scale_and_convert(&f, 4, 4).unwrap();
        assert_eq!(out.len(), 4 * 4 * 4);
        // Upscaling 2x must not pick up the padding bytes; source indices are
        // 0,1,2,3 in row-major order and the first byte of each pixel is red.
        for row in 0..4 {
            for column in 0..4 {
                let source_row = row / 2;
                let source_column = column / 2;
                let expected = (source_row * 2 + source_column) as u8;
                assert_eq!(
                    out[(row * 4 + column) * 4],
                    expected,
                    "row {row} column {column} should sample source index {}",
                    expected
                );
            }
        }
    }

    #[test]
    fn negative_origin_places_output_at_origin_of_canvas() {
        let mut f = frame(Transform::Normal, 2, 2);
        f.location = (-10, -20);
        let canvas = compose(&[f]).unwrap();
        assert_eq!((canvas.width, canvas.height), (2, 2));
    }

    #[test]
    fn mixed_scale_uses_highest_scale_and_preserves_logical_layout() {
        // 200x100 logical at scale 1, then 200x200 logical at scale 2.
        let mut left = frame(Transform::Normal, 200, 100);
        left.location = (0, 0);
        left.scale = 1.0;
        let mut right = frame(Transform::Normal, 400, 400);
        right.location = (200, 0);
        right.scale = 2.0;
        right.logical_width = 200;
        right.logical_height = 200;

        let canvas = compose(&[left, right]).unwrap();
        // Logical extent is 400x200; the highest participating scale is 2.0.
        assert_eq!((canvas.width, canvas.height), (800, 400));
    }

    #[test]
    fn adjacent_outputs_leave_no_seam() {
        // Two 1x1 logical outputs side by side at scale 1, with a gap in the
        // canvas only if rounding is done independently per output.
        let mut left = frame(Transform::Normal, 1, 1);
        left.location = (0, 0);
        left.scale = 1.0;
        let mut right = frame(Transform::Normal, 1, 1);
        right.location = (1, 0);
        right.scale = 1.0;

        let canvas = compose(&[left, right]).unwrap();
        assert_eq!((canvas.width, canvas.height), (2, 1));
        assert_eq!(canvas.pixels[3], 255, "left pixel opaque");
        assert_eq!(canvas.pixels[7], 255, "right pixel opaque");
    }

    #[test]
    fn fractional_scale_leaves_no_gaps_between_outputs() {
        // 3 + 2 logical pixels at scale 1.5 must total 8 device pixels exactly
        // when boundaries are rounded from a shared origin.
        // 2x1 and 2x1 logical, side by side, at scale 1.5.
        let mut left = frame(Transform::Normal, 3, 2);
        left.location = (0, 0);
        left.scale = 1.5;
        left.logical_width = 2;
        left.logical_height = 1;
        let mut right = frame(Transform::Normal, 3, 2);
        right.location = (2, 0);
        right.scale = 1.5;
        right.logical_width = 2;
        right.logical_height = 1;

        let canvas = compose(&[left, right]).unwrap();
        assert_eq!((canvas.width, canvas.height), (6, 2), "4x1 logical at 1.5");
        // Every pixel in the top row is covered by some output.
        for column in 0..canvas.width as usize {
            let index = column * 4 + 3;
            assert_eq!(canvas.pixels[index], 255, "column {column} has a seam");
        }
    }

    #[test]
    fn rejects_canvas_beyond_the_allocation_cap() {
        assert!(Canvas::new(20_000, 20_000).is_none());
        assert!(Canvas::new(0, 100).is_none());
        assert!(Canvas::new(100, 0).is_none());
    }

    #[test]
    fn rejects_frames_whose_buffer_is_shorter_than_their_dimensions() {
        let mut short = frame(Transform::Normal, 4, 4);
        short.pixels.truncate(short.pixels.len() - 4);
        assert!(compose(&[short]).is_err());

        let mut narrow = frame(Transform::Normal, 4, 4);
        narrow.stride = 4 * BYTES_PER_PIXEL - 1;
        assert!(compose(&[narrow]).is_err());

        let mut zero = frame(Transform::Normal, 4, 4);
        zero.width = 0;
        assert!(compose(&[zero]).is_err());

        let mut bad_scale = frame(Transform::Normal, 4, 4);
        bad_scale.scale = 0.0;
        assert!(compose(&[bad_scale]).is_err());

        let mut nan_scale = frame(Transform::Normal, 4, 4);
        nan_scale.scale = f64::NAN;
        assert!(compose(&[nan_scale]).is_err());
    }

    #[test]
    fn fractional_scale_keeps_physical_dimensions_exact() {
        // A single output at the maximum scale must keep its pixel dimensions.
        // 2560x1440 at 1.25 is exactly 2048x1152 logical.
        let mut f = frame(Transform::Normal, 2560, 1440);
        f.scale = 1.25;
        f.logical_width = 2048;
        f.logical_height = 1152;
        let canvas = compose(&[f]).unwrap();
        assert_eq!((canvas.width, canvas.height), (2560, 1440));
    }

    #[test]
    fn fractional_scale_upsamples_a_lower_scale_output() {
        // 1600x900 physical at scale 1.0 is 1600x900 logical, beside
        // 3200x1800 physical at scale 2.0 which is also 1600x900 logical.
        let mut low = frame(Transform::Normal, 1600, 900);
        low.location = (0, 0);
        low.scale = 1.0;
        let mut high = frame(Transform::Normal, 3200, 1800);
        high.location = (1600, 0);
        high.scale = 2.0;
        high.logical_width = 1600;
        high.logical_height = 900;

        let canvas = compose(&[low, high]).unwrap();
        // 3200x900 logical composed at the highest participating scale of 2.0.
        assert_eq!((canvas.width, canvas.height), (6400, 1800));
    }

    #[test]
    fn fractional_scale_crop_uses_the_original_logical_rect() {
        // 3 logical pixels at scale 1.5 read back as 5 physical pixels, so
        // recovering the logical extent by dividing would give 5/1.5 = 3.33 and
        // then 3.33*2.0 = 6.67 -> 7 on a 2.0 canvas, instead of 6.
        let mut low = frame(Transform::Normal, 5, 5);
        low.location = (0, 0);
        low.scale = 1.5;
        low.logical_width = 3;
        low.logical_height = 3;

        let mut high = frame(Transform::Normal, 4, 4);
        high.location = (3, 0);
        high.scale = 2.0;
        high.logical_width = 2;
        high.logical_height = 2;

        let canvas = compose(&[low, high]).unwrap();
        // 3 + 2 = 5 logical, composed at the highest participating scale of 2.0.
        assert_eq!((canvas.width, canvas.height), (10, 6));
    }

    #[test]
    fn adjacent_outputs_do_not_drift_across_repeated_rounding() {
        // Three fractional-scale outputs side by side. Boundaries are derived
        // from the logical rectangles, so accumulated rounding cannot open a
        // gap or overlap between them.
        let mut frames = Vec::new();
        for index in 0..3 {
            let mut f = frame(Transform::Normal, 5, 4);
            f.location = (index * 3, 0);
            f.scale = 1.5;
            f.logical_width = 3;
            f.logical_height = 3;
            frames.push(f);
        }
        let canvas = compose(&frames).unwrap();
        // 9 logical at scale 1.5.
        assert_eq!((canvas.width, canvas.height), (14, 5));
        for column in 0..canvas.width as usize {
            assert_eq!(canvas.pixels[column * 4 + 3], 255, "column {column} has a gap");
        }
    }

    #[test]
    fn outputs_below_negative_locations_share_a_seam() {
        let mut left = frame(Transform::Normal, 2, 2);
        left.location = (-4, -4);
        left.scale = 1.0;
        let mut right = frame(Transform::Normal, 2, 2);
        right.location = (-2, -4);
        right.scale = 1.0;

        let canvas = compose(&[left, right]).unwrap();
        assert_eq!((canvas.width, canvas.height), (4, 2));
        for column in 0..canvas.width as usize {
            assert_eq!(canvas.pixels[column * 4 + 3], 255, "column {column} seam");
        }
    }

    #[test]
    fn gap_between_outputs_stays_transparent() {
        // Two outputs with a one-pixel logical gap between them.
        let mut left = frame(Transform::Normal, 2, 2);
        left.location = (0, 0);
        left.scale = 1.0;
        let mut right = frame(Transform::Normal, 2, 2);
        right.location = (3, 0);
        right.scale = 1.0;

        let canvas = compose(&[left, right]).unwrap();
        assert_eq!(canvas.width, 5);
        assert_eq!(&canvas.pixels[2 * 4..3 * 4], &[0, 0, 0, 0], "gap is transparent");
        assert_eq!(canvas.pixels[3], 255);
        assert_eq!(canvas.pixels[3 * 4 + 3], 255);
    }

    #[test]
    fn parses_slurp_geometry_with_negative_origin() {
        assert_eq!(
            parse_geometry("100,200 800x600").unwrap(),
            Geometry::Region {
                x: 100,
                y: 200,
                width: 800,
                height: 600
            }
        );
        assert_eq!(
            parse_geometry("-1920,-40 1280x1024").unwrap(),
            Geometry::Region {
                x: -1920,
                y: -40,
                width: 1280,
                height: 1024
            }
        );
    }

    #[test]
    fn geometry_tolerates_surrounding_whitespace_from_slurp() {
        for raw in ["100,200 800x600\n", " 100,200 800x600 ", "100,200 800x600\r\n"] {
            assert_eq!(
                parse_geometry(raw).unwrap(),
                Geometry::Region {
                    x: 100,
                    y: 200,
                    width: 800,
                    height: 600
                },
                "should accept {raw:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_and_malformed_geometry() {
        for raw in [
            "",
            "   ",
            "\n",
            "100,200",
            "100 200 300",
            "100,200 800",
            "100,200 800x",
            "x,200 800x600",
            "100,y 800x600",
            "100,200 WxH",
            "100,200 800xH",
            "100.5,200 800x600",
            "1e3,200 800x600",
            "100,200,300 800x600",
        ] {
            assert!(parse_geometry(raw).is_err(), "should reject {raw:?}");
        }
    }

    #[test]
    fn rejects_non_positive_and_overflowing_geometry() {
        for raw in [
            "0,0 0x100",
            "0,0 100x0",
            "0,0 -100x100",
            "0,0 100x-1",
            "2147483647,0 2x100",
            "0,2147483647 100x2",
        ] {
            assert!(parse_geometry(raw).is_err(), "should reject {raw:?}");
        }
    }

    #[test]
    fn rejects_empty_frame_set() {
        assert!(compose(&[]).is_err());
    }
}
