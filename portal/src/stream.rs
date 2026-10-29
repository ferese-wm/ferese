use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pipewire as pw;
use pipewire::properties::properties;
use pipewire::spa;
use serde::{Deserialize, Serialize};
use spa::pod::Pod;

mod activity;
mod capture;
mod rate;
use activity::{Activity, FailureAction};
use capture::{Capture, Completion, DmaFormat};
use rate::FrameRate;

#[derive(Debug, Serialize, Deserialize)]
pub struct Ready {
    #[serde(default)]
    pub logical_size: Option<(u32, u32)>,
    pub node: u32,
    pub width: u32,
    pub height: u32,
}

struct PoolBuffer {
    generation: u64,
    buffer: capture::Buffer,
}

#[derive(Clone, Copy)]
struct HeldBuffer {
    raw: NonNull<pw::sys::pw_buffer>,
    generation: u64,
    activity: activity::Token,
}

struct Data {
    capture: Rc<RefCell<Capture>>,
    stop: Arc<AtomicBool>,
    rate: Arc<FrameRate>,
    buffers: HashMap<usize, PoolBuffer>,
    held: Option<HeldBuffer>,
    generation: u64,
    sequence: u64,
    activity: Activity,
    announced: bool,
    negotiated: (u32, u32),
    dma: Option<DmaFormat>,
    offered_dma: Vec<DmaFormat>,
    force_shm: bool,
    renegotiate: bool,
    negotiating: bool,
    next_capture: Instant,
}

impl Data {
    fn complete(&mut self, stream: &pw::stream::Stream, completion: Completion) {
        let Some(held) = self.held.take() else {
            return;
        };
        let Some(pool) = self
            .buffers
            .get(&(held.raw.as_ptr() as usize))
            .filter(|pool| pool.generation == held.generation)
        else {
            // The completed image belongs to a retired pool. Refresh once more
            // so the replacement pool receives its own initial image, then
            // resume damage waiting. Never queue the retired raw pointer.
            if let Err(error) = self.capture.borrow_mut().refresh() {
                eprintln!("Could not refresh replacement capture buffers: {error}");
                self.stop.store(true, Ordering::Relaxed);
            }
            return;
        };
        let source_stopped = self.capture.borrow().stopped();
        let current_layout = pool.buffer.size == self.negotiated && pool.buffer.dma == self.dma;
        let mut deliver = matches!(completion, Completion::Ready)
            && self.activity.active()
            && !source_stopped
            && current_layout;
        if !current_layout
            && matches!(completion, Completion::Ready)
            && let Err(error) = self.capture.borrow_mut().refresh()
        {
            eprintln!("Could not refresh renegotiated capture buffers: {error}");
            self.stop.store(true, Ordering::Relaxed);
        }
        match completion {
            Completion::Retry => {
                self.renegotiate = self.negotiated != self.capture.borrow().constraints().size
            }
            Completion::Failed { kind, error } => {
                match self.activity.failure_action(
                    held.activity,
                    source_stopped,
                    self.dma.is_some(),
                    kind,
                ) {
                    FailureAction::Discard => (),
                    FailureAction::Retry => {
                        if let Err(error) = self.capture.borrow_mut().refresh() {
                            eprintln!("Could not retry capture: {error}");
                            self.stop.store(true, Ordering::Relaxed);
                        }
                    }
                    FailureAction::FallbackToShm => {
                        eprintln!("DMA-BUF capture unavailable ({error}); using shared memory");
                        self.force_shm = true;
                        self.renegotiate = true;
                        // Retry unchanged content after a failed GPU transfer.
                        if let Err(error) = self.capture.borrow_mut().refresh() {
                            eprintln!("Could not refresh fallback capture: {error}");
                            self.stop.store(true, Ordering::Relaxed);
                        }
                    }
                    FailureAction::Stop => {
                        eprintln!("{error}");
                        self.stop.store(true, Ordering::Relaxed);
                    }
                }
                deliver = false;
            }
            Completion::Ready => self.activity.ready(),
        }
        let raw = held.raw;
        // SAFETY: this pointer is still registered with the same allocation
        // generation and exclusively dequeued by us. All callbacks run on the
        // owning PipeWire loop; remove_buffer invalidates the registry entry.
        unsafe {
            let buffer = raw.as_ref().buffer.as_mut().expect("registered SPA buffer");
            if !finish_planes(buffer, &pool.buffer.planes, deliver) {
                self.stop.store(true, Ordering::Relaxed);
                return;
            }
            if deliver {
                timestamp(raw, self.sequence);
                self.sequence = self.sequence.wrapping_add(1);
            }
            stream.queue_raw_buffer(raw.as_ptr());
        }
    }
}
/// The caller owns the SPA buffer and guarantees that its data/chunk pointers
/// describe live allocations for the advertised counts on the PipeWire loop.
unsafe fn plane_datas(
    buffer: &mut spa::sys::spa_buffer,
    count: usize,
) -> Option<&mut [spa::sys::spa_data]> {
    if buffer.n_datas as usize != count || buffer.datas.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees the SPA array; exact counts are checked
    // before constructing a slice, so modifier plane counts cannot overrun it.
    let datas = unsafe { std::slice::from_raw_parts_mut(buffer.datas, count) };
    datas
        .iter()
        .all(|data| !data.chunk.is_null())
        .then_some(datas)
}

/// See `plane_datas` for the caller's pointer ownership requirements.
unsafe fn assign_planes(
    buffer: &mut spa::sys::spa_buffer,
    planes: &[capture::Plane],
    dma: bool,
) -> bool {
    // SAFETY: our caller supplies the same live SPA allocation.
    let Some(datas) = (unsafe { plane_datas(buffer, planes.len()) }) else {
        return false;
    };
    let target_iter = datas.iter_mut().zip(planes);
    for (target, plane) in target_iter {
        target.type_ = if dma {
            spa::sys::SPA_DATA_DmaBuf
        } else {
            spa::sys::SPA_DATA_MemFd
        };
        target.flags = spa::sys::SPA_DATA_FLAG_READWRITE;
        target.fd = i64::from(plane.fd.as_raw_fd());
        target.mapoffset = 0;
        target.maxsize = plane.bytes;
        target.data = std::ptr::null_mut();
        // SAFETY: plane_datas checked the caller-owned chunk pointer.
        let chunk = unsafe { &mut *target.chunk };
        chunk.offset = plane.offset;
        chunk.size = 0;
        chunk.stride = plane.stride as i32;
    }
    true
}

/// See `plane_datas` for the caller's pointer ownership requirements.
unsafe fn finish_planes(
    buffer: &mut spa::sys::spa_buffer,
    planes: &[capture::Plane],
    deliver: bool,
) -> bool {
    // SAFETY: our caller supplies the same live SPA allocation.
    let Some(datas) = (unsafe { plane_datas(buffer, planes.len()) }) else {
        return false;
    };
    let target_iter = datas.iter_mut().zip(planes);
    for (target, plane) in target_iter {
        // SAFETY: plane_datas checked the caller-owned chunk pointer.
        let chunk = unsafe { &mut *target.chunk };
        chunk.offset = plane.offset;
        chunk.size = if deliver {
            plane.bytes - plane.offset
        } else {
            0
        };
        chunk.stride = plane.stride as i32;
    }
    true
}

fn int_property(key: u32, value: i32) -> spa::pod::Property {
    spa::pod::Property {
        key,
        flags: spa::pod::PropertyFlags::empty(),
        value: spa::pod::Value::Int(value),
    }
}
fn pod(object: spa::pod::Object) -> Vec<u8> {
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .expect("SPA pod serialization into an owned Vec cannot fail for these object types")
    .0
    .into_inner()
}

fn video_format(width: u32, height: u32, rate: &FrameRate) -> Vec<u8> {
    pod(spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Id,
            spa::param::video::VideoFormat::BGRx
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            spa::utils::Rectangle { width, height }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            rate.fraction(),
            spa::utils::Fraction { num: 1, denom: 1 },
            spa::utils::Fraction {
                num: rate.maximum,
                denom: 1
            }
        )
    ))
}

fn timestamp(raw: std::ptr::NonNull<pw::sys::pw_buffer>, sequence: u64) {
    // SAFETY: mutable access is exclusive, the metadata lookup checks its size,
    // and clock_gettime writes only to our initialized stack timespec.
    unsafe {
        let buffer = raw.as_ref().buffer;
        if buffer.is_null() {
            return;
        }
        let header = spa::sys::spa_buffer_find_meta_data(
            buffer,
            spa::sys::SPA_META_Header,
            std::mem::size_of::<spa::sys::spa_meta_header>(),
        )
        .cast::<spa::sys::spa_meta_header>();
        if let Some(header) = header.as_mut() {
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            header.pts = if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) == 0 {
                now.tv_sec * 1_000_000_000 + now.tv_nsec
            } else {
                -1
            };
            header.flags = 0;
            header.offset = 0;
            header.dts_offset = 0;
            header.seq = sequence;
        }
    }
}

fn buffer_params((width, height): (u32, u32), dma: Option<DmaFormat>) -> Vec<u8> {
    pod(spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamBuffers,
        spa::param::ParamType::Buffers,
        int_property(spa::sys::SPA_PARAM_BUFFERS_buffers, 4),
        int_property(
            spa::sys::SPA_PARAM_BUFFERS_blocks,
            dma.map_or(1, |format| format.planes as i32)
        ),
        int_property(
            spa::sys::SPA_PARAM_BUFFERS_size,
            if dma.is_some() {
                0
            } else {
                (width * height * 4) as i32
            }
        ),
        int_property(
            spa::sys::SPA_PARAM_BUFFERS_stride,
            if dma.is_some() { 0 } else { (width * 4) as i32 }
        ),
        int_property(spa::sys::SPA_PARAM_BUFFERS_align, 16),
        // This is a set of supported memory types, not an exact integer.
        // Consumers such as OBS offer a scalar mask with several bits;
        // a flags choice lets SPA intersect it with our one supported type.
        spa::pod::Property {
            key: spa::sys::SPA_PARAM_BUFFERS_dataType,
            flags: spa::pod::PropertyFlags::empty(),
            value: spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Flags {
                    default: 1
                        << if dma.is_some() {
                            spa::sys::SPA_DATA_DmaBuf
                        } else {
                            spa::sys::SPA_DATA_MemFd
                        },
                    flags: Vec::new(),
                },
            ),)),
        }
    ))
}

fn formats(size: (u32, u32), rate: &FrameRate, dma: &[DmaFormat]) -> Vec<Vec<u8>> {
    let shm = video_format(size.0, size.1, rate);
    let mut offers = Vec::with_capacity(dma.len() + 1);
    for dma in dma {
        let (_, mut format) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(&shm)
            .expect("serialized video format");
        if let spa::pod::Value::Object(ref mut object) = format {
            object.properties.push(spa::pod::Property {
                key: spa::sys::SPA_FORMAT_VIDEO_modifier,
                flags: spa::pod::PropertyFlags::MANDATORY,
                value: spa::pod::Value::Long(dma.modifier as i64),
            });
        }
        offers.push(
            spa::pod::serialize::PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &format,
            )
            .expect("serializable video format")
            .0
            .into_inner(),
        );
    }
    // Explicit, pre-probed modifier offers let the consumer intersect its own
    // modifier set without a DONT_FIXATE allocation/retry loop.
    offers.push(shm);
    offers
}

pub fn run(
    name: String,
    cursor: bool,
    generation: Option<u32>,
    include_shell: bool,
    workspace_quality: crate::workspace::Quality,
) -> Result<(), Box<dyn std::error::Error>> {
    let configured_rate = std::env::var("FERESE_CAPTURE_MAX_FPS").ok();
    let rate = Arc::new(FrameRate::try_new(configured_rate.as_deref())?);
    let stop = Arc::new(AtomicBool::new(false));
    let (cancel_read, mut cancel_write) = std::os::unix::net::UnixStream::pair()?;
    cancel_read.set_nonblocking(true)?;
    let pipe_stop = stop.clone();
    std::thread::spawn(move || {
        let mut byte = [0u8];
        let _ = std::io::stdin().read(&mut byte);
        pipe_stop.store(true, Ordering::Relaxed);
        let _ = cancel_write.write_all(&[1]);
    });
    let capture = Rc::new(RefCell::new(Capture::connect(
        &name,
        cursor,
        generation,
        include_shell,
        workspace_quality,
        &stop,
    )?));
    let size = capture.borrow().constraints().size;
    let logical_size = capture.borrow().logical_size();
    let force_shm = std::env::var_os("FERESE_CAPTURE_FORCE_SHM").is_some_and(|value| value == "1");
    // Probe the actual plane count and bounded allocation for every offered
    // layout. A failed probe leaves SHM available in the same capture session.
    let offered_dma = if force_shm {
        Vec::new()
    } else {
        capture.borrow().dma_formats()
    };
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "Ferese ScreenCast",
        properties! {
            "media.class" => "Video/Source", "media.type" => "Video", "media.category" => "Capture",
            "media.role" => "Screen", "node.name" => format!("ferese.screencast.{}", std::process::id()),
            "node.description" => format!("Ferese — {name}"), "node.virtual" => "true",
            "node.want-driver" => "true", "node.always-process" => "false", "node.pause-on-idle" => "true",
            "node.latency" => format!("{}/{}", rate.fraction().denom, rate.fraction().num),
        },
    )?;
    let data = Rc::new(RefCell::new(Data {
        capture: capture.clone(),
        stop: stop.clone(),
        rate: rate.clone(),
        buffers: HashMap::new(),
        held: None,
        generation: 0,
        sequence: 0,
        activity: Activity::default(),
        announced: false,
        negotiated: size,
        dma: None,
        offered_dma: offered_dma.clone(),
        force_shm,
        renegotiate: false,
        negotiating: false,
        next_capture: Instant::now(),
    }));
    let _listener = stream
        .add_local_listener_with_user_data(data.clone())
        .state_changed(move |stream, data, _, state| {
            let mut data = data.borrow_mut();
            let active = matches!(state, pw::stream::StreamState::Streaming);
            let resumed = data.activity.set_active(active);
            if let Err(error) = data.capture.borrow_mut().set_active(active) {
                eprintln!("Could not update capture activity: {error}");
                data.stop.store(true, Ordering::Relaxed);
            }
            if resumed && let Err(error) = data.capture.borrow_mut().refresh() {
                eprintln!("Could not resume capture: {error}");
                data.stop.store(true, Ordering::Relaxed);
            }
            if resumed {
                data.next_capture = Instant::now();
            }
            match state {
                pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming
                    if !data.announced =>
                {
                    let node = stream.node_id();
                    if node != u32::MAX {
                        data.announced = true;
                        println!(
                            "{}",
                            serde_json::to_string(&Ready {
                                logical_size,
                                node,
                                width: size.0,
                                height: size.1
                            })
                            .expect("serializable capture readiness")
                        );
                        let _ = std::io::stdout().flush();
                    }
                }
                pw::stream::StreamState::Unconnected if data.announced => {
                    data.stop.store(true, Ordering::Relaxed)
                }
                pw::stream::StreamState::Error(error) => {
                    eprintln!("PipeWire stream: {error}");
                    data.stop.store(true, Ordering::Relaxed);
                }
                _ => (),
            }
        })
        .param_changed(|stream, shared, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                return;
            };
            let mut format = spa::param::video::VideoInfoRaw::default();
            let mut data = shared.borrow_mut();
            if format.parse(param).is_err()
                || format.format() != spa::param::video::VideoFormat::BGRx
                || (format.size().width, format.size().height)
                    != data.capture.borrow().constraints().size
                || data.rate.negotiate(format.framerate()).is_err()
            {
                data.stop.store(true, Ordering::Relaxed);
                return;
            }
            data.negotiated = (format.size().width, format.size().height);
            let dma = format
                .flags()
                .contains(spa::param::video::VideoFlags::MODIFIER);
            data.dma = if dma {
                let selected = data
                    .offered_dma
                    .iter()
                    .find(|offer| offer.modifier == format.modifier())
                    .copied();
                if data.force_shm || selected.is_none() {
                    data.force_shm = true;
                    data.renegotiate = true;
                    return;
                }
                selected
            } else {
                None
            };
            data.negotiating = false;
            let selected = data.rate.fraction();
            let properties =
                properties! { "node.latency" => format!("{}/{}", selected.denom, selected.num) };
            // SAFETY: this loop owns the stream; PipeWire copies the live dictionary.
            unsafe {
                pw::sys::pw_stream_update_properties(
                    stream.as_raw_ptr(),
                    properties.dict().as_raw_ptr(),
                );
            }
            let buffers = buffer_params(data.negotiated, data.dma);
            let header = pod(spa::pod::object!(
                spa::utils::SpaTypes::ObjectParamMeta,
                spa::param::ParamType::Meta,
                spa::pod::Property {
                    key: spa::sys::SPA_PARAM_META_type,
                    flags: spa::pod::PropertyFlags::empty(),
                    value: spa::pod::Value::Id(spa::utils::Id(spa::sys::SPA_META_Header))
                },
                int_property(
                    spa::sys::SPA_PARAM_META_size,
                    std::mem::size_of::<spa::sys::spa_meta_header>() as i32
                )
            ));
            if stream
                .update_params(&mut [
                    Pod::from_bytes(&buffers).expect("serialized buffer constraints"),
                    Pod::from_bytes(&header).expect("serialized metadata constraints"),
                ])
                .is_err()
            {
                data.stop.store(true, Ordering::Relaxed);
            }
        })
        .add_buffer(|_, shared, raw| {
            let mut data = shared.borrow_mut();
            if raw.is_null() || data.buffers.len() >= 8 {
                data.stop.store(true, Ordering::Relaxed);
                return;
            }
            let result = data.capture.borrow_mut().allocate(data.dma);
            match result {
                Ok(buffer) => {
                    // SAFETY: PipeWire grants mutable access during add_buffer.
                    // ALLOC_BUFFERS delegates fd ownership to us until remove_buffer.
                    unsafe {
                        let Some(spa) = (*raw).buffer.as_mut() else {
                            data.stop.store(true, Ordering::Relaxed);
                            return;
                        };
                        if !assign_planes(spa, &buffer.planes, data.dma.is_some()) {
                            if data.dma.is_some() {
                                data.force_shm = true;
                                data.renegotiate = true;
                            } else {
                                data.stop.store(true, Ordering::Relaxed);
                            }
                            return;
                        }
                    }
                    data.generation = data.generation.wrapping_add(1);
                    let generation = data.generation;
                    data.buffers
                        .insert(raw as usize, PoolBuffer { generation, buffer });
                }
                Err(error) if data.dma.is_some() => {
                    eprintln!("DMA-BUF allocation failed ({error}); using shared memory");
                    data.force_shm = true;
                    data.renegotiate = true;
                }
                Err(error) => {
                    eprintln!("Capture allocation failed: {error}");
                    data.stop.store(true, Ordering::Relaxed);
                }
            }
        })
        .remove_buffer(|_, shared, raw| {
            shared.borrow_mut().buffers.remove(&(raw as usize));
        })
        .process(|stream, shared| {
            let mut data = shared.borrow_mut();
            if !data.activity.active()
                || data.held.is_some()
                || data.negotiating
                || Instant::now() < data.next_capture
                || data.stop.load(Ordering::Relaxed)
            {
                return;
            }
            // SAFETY: called only on the owning loop. The registry and held token
            // retain exclusive ownership until completion, removal or disconnect.
            let Some(raw) = NonNull::new(unsafe { stream.dequeue_raw_buffer() }) else {
                return;
            };
            let key = raw.as_ptr() as usize;
            let result = data
                .buffers
                .get(&key)
                .filter(|pool| {
                    pool.buffer.size == data.capture.borrow().constraints().size
                        && pool.buffer.dma == data.dma
                })
                .map(|pool| {
                    (
                        pool.generation,
                        data.capture.borrow_mut().submit(&pool.buffer),
                    )
                });
            if let Some((generation, Ok(()))) = result {
                data.held = Some(HeldBuffer {
                    raw,
                    generation,
                    activity: data.activity.token(),
                });
                data.next_capture = Instant::now() + data.rate.interval();
            } else {
                // SAFETY: this buffer was just dequeued and hasn't been handed to
                // Wayland. Return it empty; old dimensions cannot reach consumers.
                unsafe {
                    let buffer = &mut *(*raw.as_ptr()).buffer;
                    if !buffer.datas.is_null() {
                        for target in
                            std::slice::from_raw_parts_mut(buffer.datas, buffer.n_datas as usize)
                        {
                            if let Some(chunk) = target.chunk.as_mut() {
                                chunk.size = 0;
                            }
                        }
                    }
                    stream.queue_raw_buffer(raw.as_ptr());
                }
            }
        })
        .register()?;
    // Readiness wakes the loop; dispatch once at its top, alongside deadlines
    // and PipeWire callbacks, instead of draining the same queue twice.
    let _capture_io = mainloop.loop_().add_io(
        capture.borrow().poll_fd()?,
        spa::support::system::IoFlags::IN,
        |_| {},
    );
    let _cancel_io =
        mainloop
            .loop_()
            .add_io(cancel_read, spa::support::system::IoFlags::IN, |_| {});
    let frame_timer = mainloop.loop_().add_timer(|_| {});
    let advertised = formats(size, &rate, &offered_dma);
    let mut parameters: Vec<_> = advertised
        .iter()
        .map(|format| Pod::from_bytes(format).expect("serialized video format"))
        .collect();
    // RT_PROCESS stays disabled: PipeWire callbacks, Wayland dispatch and the
    // buffer registry all belong to this single main loop.
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::ALLOC_BUFFERS | pw::stream::StreamFlags::DRIVER,
        &mut parameters,
    )?;
    let mut next_frame = Instant::now();
    while !stop.load(Ordering::Relaxed) && !capture.borrow().stopped() {
        capture.borrow_mut().dispatch();
        let completion = capture.borrow_mut().completion();
        if let Some(completion) = completion {
            data.borrow_mut().complete(&stream, completion);
        }
        // A stopped source may be the only Wayland event while the consumer
        // is paused. Exit now instead of sleeping indefinitely for another fd
        // wakeup after revocation, disconnection or a terminal copy failure.
        if stop.load(Ordering::Relaxed) || capture.borrow().stopped() {
            break;
        }
        let changed = capture.borrow_mut().take_changed();
        if changed
            && !matches!(
                crate::capture::SourceKind::parse(&name),
                crate::capture::SourceKind::Window(_)
            )
            && capture.borrow().constraints().size != size
        {
            eprintln!("Shared monitor changed size; start sharing again");
            break;
        }
        let renegotiate = {
            let mut data = data.borrow_mut();
            std::mem::take(&mut data.renegotiate) || changed
        };
        if renegotiate {
            data.borrow_mut().negotiating = true;
            let offered_dma = if data.borrow().force_shm {
                Vec::new()
            } else {
                capture.borrow().dma_formats()
            };
            let advertised = formats(capture.borrow().constraints().size, &rate, &offered_dma);
            data.borrow_mut().offered_dma = offered_dma;
            let mut params: Vec<_> = advertised
                .iter()
                .map(|format| Pod::from_bytes(format).expect("serialized video format"))
                .collect();
            stream.update_params(&mut params)?;
        }
        let can_capture = {
            let data = data.borrow();
            data.activity.active() && data.held.is_none() && !data.renegotiate && !data.negotiating
        };
        let deadline = next_frame.max(data.borrow().next_capture);
        if can_capture && Instant::now() >= deadline {
            stream.trigger_process()?;
            next_frame = next_frame_deadline(deadline, Instant::now(), rate.interval());
        }
        update_frame_timer(
            &frame_timer,
            can_capture.then(|| next_frame.max(data.borrow().next_capture)),
        )?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        mainloop.loop_().iterate(pw::loop_::Timeout::Infinite);
    }
    let _ = stream.disconnect();
    Ok(())
}

fn next_frame_deadline(deadline: Instant, now: Instant, interval: Duration) -> Instant {
    let next = deadline + interval;
    if next > now {
        // Preserve cadence when a timer wakes slightly late.
        next
    } else {
        // Resume after a pause or slow frame without a burst of catch-up frames.
        now + interval
    }
}

fn update_frame_timer(
    timer: &pw::loop_::TimerSource<'_>,
    deadline: Option<Instant>,
) -> Result<(), spa::utils::result::Error> {
    // Loop timeouts truncate to milliseconds and spin near a frame deadline.
    // A timer preserves sub-millisecond waits without changing frame pacing.
    let delay = deadline.map(|deadline| {
        deadline
            .saturating_duration_since(Instant::now())
            // Zero disarms a PipeWire timer; an overdue frame must wake instead.
            .max(Duration::from_nanos(1))
    });
    timer.update_timer(delay, None).into_result()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spa_plane_descriptors_retain_each_fd_offset_and_stride() {
        let planes = vec![
            capture::Plane {
                fd: std::fs::File::open("/dev/null").unwrap(),
                stride: 64,
                offset: 32,
                bytes: 1280,
            },
            capture::Plane {
                fd: std::fs::File::open("/dev/null").unwrap(),
                stride: 16,
                offset: 192,
                bytes: 256,
            },
        ];
        let mut chunks = [
            spa::sys::spa_chunk {
                offset: 0,
                size: 0,
                stride: 0,
                flags: 0,
            },
            spa::sys::spa_chunk {
                offset: 0,
                size: 0,
                stride: 0,
                flags: 0,
            },
        ];
        let mut datas = chunks
            .iter_mut()
            .map(|chunk| spa::sys::spa_data {
                type_: 0,
                flags: 0,
                fd: -1,
                mapoffset: 0,
                maxsize: 0,
                data: std::ptr::null_mut(),
                chunk,
            })
            .collect::<Vec<_>>();
        let mut buffer = spa::sys::spa_buffer {
            n_metas: 0,
            n_datas: 2,
            metas: std::ptr::null_mut(),
            datas: datas.as_mut_ptr(),
        };
        // SAFETY: this test owns every data/chunk allocation for the full call.
        unsafe {
            assert!(assign_planes(&mut buffer, &planes, true));
            let plane_iter = planes.iter().enumerate();
            for (index, plane) in plane_iter {
                assert_eq!(datas[index].fd, i64::from(plane.fd.as_raw_fd()));
                assert_eq!(datas[index].type_, spa::sys::SPA_DATA_DmaBuf);
                assert_eq!(datas[index].maxsize, plane.bytes);
                assert_eq!(chunks[index].offset, plane.offset);
                assert_eq!(chunks[index].stride, plane.stride as i32);
            }
            assert!(finish_planes(&mut buffer, &planes, true));
            assert_eq!([chunks[0].size, chunks[1].size], [1248, 64]);
            assert!(finish_planes(&mut buffer, &planes, false));
            assert_eq!([chunks[0].size, chunks[1].size], [0, 0]);
            for wrong_count in [0, 1, 3] {
                buffer.n_datas = wrong_count;
                assert!(!assign_planes(&mut buffer, &planes, true));
                assert!(!finish_planes(&mut buffer, &planes, true));
            }
            buffer.n_datas = 2;
            datas[1].chunk = std::ptr::null_mut();
            assert!(!assign_planes(&mut buffer, &planes, true));
        }
    }

    #[test]
    fn non_linear_modifier_intersection_keeps_the_probed_plane_count() {
        let rate = FrameRate::try_new(None).unwrap();
        let tiled = DmaFormat {
            modifier: 0x0100000000000004,
            planes: 2,
        };
        let offers = formats(
            (640, 480),
            &rate,
            &[
                tiled,
                DmaFormat {
                    modifier: 0,
                    planes: 1,
                },
            ],
        );
        let (_, mut consumer) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(
            &video_format(640, 480, &rate),
        )
        .unwrap();
        let spa::pod::Value::Object(ref mut object) = consumer else {
            panic!("video format object")
        };
        object.properties.push(spa::pod::Property {
            key: spa::sys::SPA_FORMAT_VIDEO_modifier,
            flags: spa::pod::PropertyFlags::MANDATORY,
            value: spa::pod::Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Enum {
                    default: 97,
                    alternatives: vec![97, tiled.modifier as i64],
                },
            ))),
        });
        let consumer = pod(object.clone());
        for reverse in [false, true] {
            let (offer, filter) = if reverse {
                (&consumer, &offers[0])
            } else {
                (&offers[0], &consumer)
            };
            let mut bytes = vec![0; 1024];
            let builder = spa::pod::builder::Builder::new(&mut bytes);
            let mut result = std::ptr::null_mut();
            // SAFETY: both pods, builder storage and resulting pod stay live.
            unsafe {
                assert!(
                    spa::sys::spa_pod_filter(
                        builder.as_raw_ptr(),
                        &mut result,
                        Pod::from_bytes(offer).unwrap().as_raw_ptr(),
                        Pod::from_bytes(filter).unwrap().as_raw_ptr()
                    ) >= 0
                );
                let filtered = Pod::from_raw(result);
                let mut selected = spa::param::video::VideoInfoRaw::default();
                selected.parse(filtered).unwrap();
                assert_eq!(selected.modifier(), tiled.modifier);
            }
        }
        let bytes = buffer_params((640, 480), Some(tiled));
        let (_, value) =
            spa::pod::deserialize::PodDeserializer::deserialize_any_from(&bytes).unwrap();
        let spa::pod::Value::Object(object) = value else {
            panic!("buffer parameters object")
        };
        for (key, expected) in [
            (spa::sys::SPA_PARAM_BUFFERS_blocks, 2),
            (spa::sys::SPA_PARAM_BUFFERS_size, 0),
            (spa::sys::SPA_PARAM_BUFFERS_stride, 0),
        ] {
            assert_eq!(
                object
                    .properties
                    .iter()
                    .find(|property| property.key == key)
                    .unwrap()
                    .value,
                spa::pod::Value::Int(expected)
            );
        }
        assert_eq!(formats((640, 480), &rate, &[]), vec![offers[2].clone()]);
    }

    #[test]
    fn frame_clock_preserves_cadence_but_does_not_catch_up_after_stalls() {
        let start = Instant::now();
        let interval = Duration::from_nanos(16_666_667);
        let mut deadline = start;
        for frame in 1..=240 {
            deadline =
                next_frame_deadline(deadline, deadline + Duration::from_micros(200), interval);
            assert_eq!(deadline, start + interval * frame);
        }
        for delay in [interval, Duration::from_secs(2)] {
            let resumed = deadline + delay;
            assert_eq!(
                next_frame_deadline(deadline, resumed, interval),
                resumed + interval
            );
        }
    }

    #[test]
    fn frame_timer_wakes_once_for_due_frames_and_disarms_while_paused() {
        let mainloop = pw::main_loop::MainLoopRc::new(None).unwrap();
        let expirations = Rc::new(std::cell::Cell::new(0));
        let received = expirations.clone();
        let timer = mainloop.loop_().add_timer(move |count| {
            received.set(received.get() + count);
        });
        for delay in [Duration::ZERO, Duration::from_micros(100)] {
            update_frame_timer(&timer, Some(Instant::now() + delay)).unwrap();
            let expected = expirations.get() + 1;
            let limit = Instant::now() + Duration::from_secs(1);
            while expirations.get() < expected && Instant::now() < limit {
                mainloop
                    .loop_()
                    .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(10)));
            }
            assert_eq!(expirations.get(), expected);
            mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(5)));
            assert_eq!(expirations.get(), expected, "frame timer must be one-shot");
        }
        update_frame_timer(&timer, Some(Instant::now())).unwrap();
        update_frame_timer(&timer, None).unwrap();
        let previous = expirations.get();
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(5)));
        assert_eq!(expirations.get(), previous, "paused capture must not wake");
    }

    #[test]
    fn buffer_types_intersect_obs_masks_in_either_negotiation_order() {
        // PipeWire expands OBS's MemPtr bit to also permit MemFd transport.
        let obs_mask = (1 << spa::sys::SPA_DATA_MemPtr)
            | (1 << spa::sys::SPA_DATA_MemFd)
            | (1 << spa::sys::SPA_DATA_DmaBuf);
        let consumer = pod(spa::pod::object!(
            spa::utils::SpaTypes::ObjectParamBuffers,
            spa::param::ParamType::Buffers,
            int_property(spa::sys::SPA_PARAM_BUFFERS_dataType, obs_mask)
        ));
        for dma in [false, true] {
            let producer = buffer_params(
                (735, 840),
                dma.then_some(DmaFormat {
                    modifier: 0,
                    planes: 1,
                }),
            );
            for reverse in [false, true] {
                let (offer, filter) = if reverse {
                    (&consumer, &producer)
                } else {
                    (&producer, &consumer)
                };
                let mut bytes = vec![0; 1024];
                let builder = spa::pod::builder::Builder::new(&mut bytes);
                let mut result = std::ptr::null_mut();
                // SAFETY: both input pods and the builder's writable storage
                // remain alive throughout filtering and reading the result.
                let status = unsafe {
                    spa::sys::spa_pod_filter(
                        builder.as_raw_ptr(),
                        &mut result,
                        Pod::from_bytes(offer).unwrap().as_raw_ptr(),
                        Pod::from_bytes(filter).unwrap().as_raw_ptr(),
                    )
                };
                assert!(
                    status >= 0,
                    "SPA rejected dma={dma} reverse={reverse}: {status}"
                );
                assert!(!result.is_null());
                // SAFETY: successful filtering initialized this pod in the
                // still-live builder storage; no subsequent writes occur.
                let filtered = unsafe { Pod::from_raw(result) };
                let (_, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(
                    filtered.as_bytes(),
                )
                .unwrap();
                let spa::pod::Value::Object(object) = value else {
                    panic!("buffer parameters must remain an object")
                };
                let types = object
                    .properties
                    .iter()
                    .find(|property| property.key == spa::sys::SPA_PARAM_BUFFERS_dataType)
                    .unwrap();
                let expected = 1
                    << if dma {
                        spa::sys::SPA_DATA_DmaBuf
                    } else {
                        spa::sys::SPA_DATA_MemFd
                    };
                match &types.value {
                    spa::pod::Value::Int(mask) => assert_eq!(*mask, expected),
                    spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                        _,
                        choice,
                    ))) => match choice {
                        spa::utils::ChoiceEnum::Flags { default, .. }
                        | spa::utils::ChoiceEnum::None(default) => assert_eq!(*default, expected),
                        other => panic!("unexpected filtered choice: {other:?}"),
                    },
                    other => panic!("unexpected filtered mask: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn dma_is_an_explicit_format_with_a_shared_memory_alternative() {
        let rate = FrameRate::try_new(None).unwrap();
        let offers = formats(
            (640, 480),
            &rate,
            &[DmaFormat {
                modifier: 0,
                planes: 1,
            }],
        );
        assert_eq!(offers.len(), 2);
        let offer_iter = offers.iter().enumerate();
        for (index, offer) in offer_iter {
            let (_, value) =
                spa::pod::deserialize::PodDeserializer::deserialize_any_from(offer).unwrap();
            let spa::pod::Value::Object(object) = value else {
                panic!("video format must be an object");
            };
            let modifier = object
                .properties
                .iter()
                .find(|property| property.key == spa::sys::SPA_FORMAT_VIDEO_modifier);
            if index == 0 {
                let modifier = modifier.expect("DMA-BUF requires an explicit modifier");
                assert!(modifier.flags.contains(spa::pod::PropertyFlags::MANDATORY));
                assert_eq!(modifier.value, spa::pod::Value::Long(0));
            } else {
                assert!(
                    modifier.is_none(),
                    "SHM must not inherit the DMA-BUF modifier"
                );
            }
            assert!(object.properties.iter().any(|property| property.key
                == spa::sys::SPA_FORMAT_VIDEO_framerate
                && matches!(property.value, spa::pod::Value::Choice(_))));
        }
        assert_eq!(formats((640, 480), &rate, &[]), vec![offers[1].clone()]);
    }
}
