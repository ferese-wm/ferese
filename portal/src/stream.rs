use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pipewire as pw;
use pipewire::properties::properties;
use pipewire::spa;
use serde::{Deserialize, Serialize};
use spa::pod::Pod;

use crate::capture::{Capture, Frame};

#[derive(Debug, Serialize, Deserialize)]
pub struct Ready {
    #[serde(default)]
    pub logical_size: Option<(u32, u32)>,
    pub node: u32,
    pub width: u32,
    pub height: u32,
}

struct Data {
    latest: Arc<Mutex<Frame>>,
    stop: Arc<AtomicBool>,
    streaming: Arc<AtomicBool>,
    fresh: Arc<AtomicBool>,
    announced: bool,
    sequence: u64,
    negotiated: (u32, u32),
}

// pipewire-rs exposes read-only metadata through Buffer. Own a dequeued raw
// buffer here so header writes retain exclusive access and every exit requeues it.
struct OutputBuffer<'a> {
    stream: &'a pw::stream::Stream,
    raw: std::ptr::NonNull<pw::sys::pw_buffer>,
}
impl<'a> OutputBuffer<'a> {
    fn dequeue(stream: &'a pw::stream::Stream) -> Option<Self> {
        // SAFETY: called on the owning PipeWire loop; the guard queues exactly once.
        let raw = std::ptr::NonNull::new(unsafe { stream.dequeue_raw_buffer() })?;
        Some(Self { stream, raw })
    }
    fn data(&mut self) -> Option<&mut spa::buffer::Data> {
        // SAFETY: the dequeued buffer is exclusively ours until Drop. SPA Data
        // is repr(transparent), and PipeWire owns the mapped allocation.
        unsafe {
            let buffer = self.raw.as_ref().buffer.as_mut()?;
            if buffer.n_datas == 0 || buffer.datas.is_null() {
                return None;
            }
            buffer.datas.cast::<spa::buffer::Data>().as_mut()
        }
    }
    fn timestamp(&mut self, sequence: u64) {
        // SAFETY: mutable access is exclusive, the metadata lookup checks its size,
        // and clock_gettime writes only to our initialized stack timespec.
        unsafe {
            let buffer = self.raw.as_ref().buffer;
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
                let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
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
}
impl Drop for OutputBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: raw came from this stream and has not yet been queued.
        unsafe {
            self.stream.queue_raw_buffer(self.raw.as_ptr());
        }
    }
}

fn int_property(key: u32, value: i32) -> spa::pod::Property {
    spa::pod::Property {
        key,
        flags: spa::pod::PropertyFlags::empty(),
        value: spa::pod::Value::Int(value),
    }
}
fn pod(object: spa::pod::Object) -> Vec<u8> {
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(object))
        .unwrap()
        .0
        .into_inner()
}

fn video_format(width: u32, height: u32) -> Vec<u8> {
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
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 }
        )
    ))
}

pub fn run(name: String, cursor: bool, generation: Option<u32>) -> Result<(), Box<dyn std::error::Error>> {
    let stop = Arc::new(AtomicBool::new(false));
    let pipe_stop = stop.clone();
    std::thread::spawn(move || {
        let mut byte = [0u8];
        let _ = std::io::stdin().read(&mut byte);
        pipe_stop.store(true, Ordering::Relaxed);
    });
    let mut capture = Capture::connect(&stop)?;
    if let Some(generation) = generation {
        capture.pin_output(&name, generation)?;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let first = loop {
        match capture.frame(&name, cursor, &stop, Vec::new()) {
            Err(error) if error == crate::capture::BUSY && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => break result?,
        }
    };
    let (width, height) = (first.width, first.height);
    let logical_size = first.logical_size;
    let resizable = name.starts_with("window:");
    let latest = Arc::new(Mutex::new(first));
    let streaming = Arc::new(AtomicBool::new(false));

    let fresh = Arc::new(AtomicBool::new(false));
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "Ferese ScreenCast",
        properties! {
            "media.class" => "Video/Source",
            "media.type" => "Video",
            "media.category" => "Capture",
            "media.role" => "Screen",
            "node.name" => format!("ferese.screencast.{}", std::process::id()),
            "node.description" => format!("Ferese — {name}"),
            "node.virtual" => "true",
            "node.want-driver" => "true",
            "node.always-process" => "false",
            "node.pause-on-idle" => "true",
            "node.latency" => "1/30",
        },
    )?;
    let format_frames = latest.clone();
    let data = Data {
        latest: latest.clone(),
        stop: stop.clone(),
        streaming: streaming.clone(),
        fresh: fresh.clone(),
        announced: false,
        sequence: 0,
        negotiated: (width, height),
    };
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |stream, data, _, state| {
            data.fresh.store(false, Ordering::SeqCst);
            data.streaming
                .store(matches!(state, pw::stream::StreamState::Streaming), Ordering::Relaxed);
            match state {
                pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming if !data.announced => {
                    let node = stream.node_id();
                    if node != u32::MAX {
                        data.announced = true;
                        println!(
                            "{}",
                            serde_json::to_string(&Ready {
                                logical_size,
                                node,
                                width,
                                height
                            })
                            .unwrap()
                        );
                        let _ = std::io::stdout().flush();
                    }
                }
                pw::stream::StreamState::Unconnected if data.announced => {
                    data.stop.store(true, Ordering::Relaxed);
                }
                pw::stream::StreamState::Error(error) => {
                    eprintln!("PipeWire stream: {error}");
                    data.stop.store(true, Ordering::Relaxed);
                }
                _ => (),
            }
        })
        .param_changed(move |stream, data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else { return };
            let mut format = spa::param::video::VideoInfoRaw::default();
            if format.parse(param).is_err()
                || format.size().width == 0
                || format.size().height == 0
                || (!resizable && (format.size().width, format.size().height) != (width, height))
                || u64::from(format.size().width) * u64::from(format.size().height) * 4 > 128 * 1024 * 1024
                || format.format() != spa::param::video::VideoFormat::BGRx
            {
                data.stop.store(true, Ordering::Relaxed);
                return;
            }
            let (width, height) = (format.size().width, format.size().height);
            data.negotiated = (width, height);
            let buffers = pod(spa::pod::object!(
                spa::utils::SpaTypes::ObjectParamBuffers,
                spa::param::ParamType::Buffers,
                int_property(spa::sys::SPA_PARAM_BUFFERS_buffers, 4),
                int_property(spa::sys::SPA_PARAM_BUFFERS_blocks, 1),
                int_property(spa::sys::SPA_PARAM_BUFFERS_size, (width * height * 4) as i32),
                int_property(spa::sys::SPA_PARAM_BUFFERS_stride, (width * 4) as i32),
                int_property(spa::sys::SPA_PARAM_BUFFERS_align, 16)
            ));
            let header = pod(spa::pod::object!(
                spa::utils::SpaTypes::ObjectParamMeta,
                spa::param::ParamType::Meta,
                spa::pod::Property {
                    key: spa::sys::SPA_PARAM_META_type,
                    flags: spa::pod::PropertyFlags::empty(),
                    value: spa::pod::Value::Id(spa::utils::Id(spa::sys::SPA_META_Header)),
                },
                int_property(
                    spa::sys::SPA_PARAM_META_size,
                    std::mem::size_of::<spa::sys::spa_meta_header>() as i32
                )
            ));
            if stream
                .update_params(&mut [Pod::from_bytes(&buffers).unwrap(), Pod::from_bytes(&header).unwrap()])
                .is_err()
            {
                data.stop.store(true, Ordering::Relaxed);
            }
        })
        .process(|stream, data| {
            let Some(mut buffer) = OutputBuffer::dequeue(stream) else {
                return;
            };
            let Some(target) = buffer.data() else {
                return;
            };
            if data.stop.load(Ordering::Relaxed) || !data.fresh.load(Ordering::SeqCst) {
                *target.chunk_mut().size_mut() = 0;
                return;
            }
            let frame = data.latest.lock().unwrap();
            if (frame.width, frame.height) != data.negotiated {
                *target.chunk_mut().size_mut() = 0;
                return;
            }
            let row = frame.width as usize * 4;
            let bytes = row * frame.height as usize;
            let Some(destination) = target.data() else {
                return;
            };
            if destination.len() < bytes {
                data.stop.store(true, Ordering::Relaxed);
                return;
            }
            for (dst, src) in destination[..bytes]
                .chunks_exact_mut(row)
                .zip(frame.pixels.chunks_exact(frame.stride as usize))
            {
                dst.copy_from_slice(&src[..row]);
            }
            *target.chunk_mut().offset_mut() = 0;
            *target.chunk_mut().size_mut() = bytes as u32;
            *target.chunk_mut().stride_mut() = row as i32;
            buffer.timestamp(data.sequence);
            data.sequence = data.sequence.wrapping_add(1);
        })
        .register()?;

    let format = video_format(width, height);

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::DRIVER,
        &mut [Pod::from_bytes(&format).unwrap()],
    )?;

    let interval = Duration::from_nanos(1_000_000_000 / 30);
    let capture_stop = stop.clone();
    let capture_streaming = streaming.clone();
    let capture_thread = std::thread::spawn(move || {
        let mut spare = Vec::new();
        let mut checked = Instant::now() - Duration::from_secs(1);
        while !capture_stop.load(Ordering::Relaxed) {
            let start = Instant::now();

            if capture_streaming.load(Ordering::Relaxed) || checked.elapsed() >= Duration::from_secs(1) {
                checked = Instant::now();
                match capture.frame(&name, cursor, &capture_stop, std::mem::take(&mut spare)) {
                    Ok(frame) if resizable || (frame.width == width && frame.height == height) => {
                        spare = std::mem::replace(&mut *latest.lock().unwrap(), frame).pixels;
                        fresh.store(capture_streaming.load(Ordering::Relaxed), Ordering::SeqCst);
                    }
                    Ok(_) => {
                        eprintln!("Shared monitor changed size; start sharing again");
                        capture_stop.store(true, Ordering::Relaxed);
                    }
                    Err(error) if error == crate::capture::BUSY => (),
                    Err(error) => {
                        eprintln!("{error}");
                        capture_stop.store(true, Ordering::Relaxed);
                    }
                }
            }
            std::thread::sleep(interval.saturating_sub(start.elapsed()));
        }
    });
    let mut next_frame = Instant::now();
    let mut requested_size = (width, height);
    let mut renegotiation_error = None;

    while !stop.load(Ordering::Relaxed) {
        let size = {
            let frame = format_frames.lock().unwrap();
            (frame.width, frame.height)
        };
        if resizable && size != requested_size {
            let format = video_format(size.0, size.1);
            if let Err(error) = stream.update_params(&mut [Pod::from_bytes(&format).unwrap()]) {
                renegotiation_error = Some(error);
                stop.store(true, Ordering::Relaxed);
                break;
            }
            requested_size = size;
        }
        let active = streaming.load(Ordering::Relaxed);
        let timeout = if active {
            next_frame
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10))
        } else {
            Duration::from_millis(10)
        };
        mainloop.loop_().iterate(pw::loop_::Timeout::Finite(timeout));
        if streaming.load(Ordering::Relaxed) && Instant::now() >= next_frame {
            let _ = stream.trigger_process();
            next_frame = Instant::now() + interval;
        } else if !streaming.load(Ordering::Relaxed) {
            next_frame = Instant::now();
        }
    }

    let _ = stream.disconnect();
    let _ = capture_thread.join();
    if let Some(error) = renegotiation_error {
        return Err(error.into());
    }

    Ok(())
}
