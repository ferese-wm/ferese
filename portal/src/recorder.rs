use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::recording::{Audio, Recording};
use futures_util::StreamExt;
use gstreamer as gst;
use gstreamer::prelude::*;
use tokio::sync::watch;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

type Options = HashMap<String, OwnedValue>;
const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";

pub fn event(state: &str, message: Option<&str>, path: Option<&Path>) {
    let value = serde_json::json!({"state": state, "message": message, "path": path});
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn option_string(value: String) -> OwnedValue {
    OwnedValue::try_from(Value::from(value))
        .expect("the D-Bus value uses an owned supported scalar or collection type")
}

async fn request<B: serde::Serialize + zbus::zvariant::DynamicType>(
    connection: &Connection,
    method: &str,
    token: &str,
    body: &B,
    stop: &mut watch::Receiver<bool>,
) -> Result<Option<Options>, String> {
    let sender = connection
        .unique_name()
        .ok_or("No D-Bus identity")?
        .as_str()[1..]
        .replace('.', "_");
    let path = format!("{DESKTOP_PATH}/request/{sender}/{token}");
    let request = Proxy::new(
        connection,
        DESKTOP,
        path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut responses = request
        .receive_signal("Response")
        .await
        .map_err(|e| e.to_string())?;
    let portal = Proxy::new(
        connection,
        DESKTOP,
        DESKTOP_PATH,
        "org.freedesktop.portal.ScreenCast",
    )
    .await
    .map_err(|e| e.to_string())?;
    let returned: OwnedObjectPath = portal
        .call(method, body)
        .await
        .map_err(|e| format!("Screen recording portal unavailable: {e}"))?;

    if returned.as_str() != path {
        return Err("Unexpected portal request handle".into());
    }

    if *stop.borrow() {
        let _: Result<(), _> = request.call("Close", &()).await;
        return Ok(None);
    }

    tokio::select! {
        _ = stop.changed() => {
            let _: Result<(), _> = request.call("Close", &()).await;
            Ok(None)
        }

        signal = tokio::time::timeout(Duration::from_secs(125), responses.next()) => {
            let signal = signal.map_err(|_| "Display selection timed out")?.ok_or("Portal disconnected")?;
            let (code, options): (u32, Options) = signal.body().deserialize().map_err(|e| e.to_string())?;
            match code { 0 => Ok(Some(options)), 1 => Ok(None), _ => Err("Screen recording request failed".into()) }
        }
    }
}

pub async fn run() -> Result<(), String> {
    gst::init().map_err(|e| e.to_string())?;

    for factory in [
        "pipewiresrc",
        "queue",
        "videoconvert",
        "vp8enc",
        "webmmux",
        "fdsink",
    ] {
        if gst::ElementFactory::find(factory).is_none() {
            return Err(format!(
                "Recording needs the GStreamer {factory} plugin. See the recording installation guide."
            ));
        }
    }

    let output = output_directory()?;
    let (stop_send, mut stop) = watch::channel(false);

    std::thread::spawn(move || {
        let _ = std::io::stdin().read(&mut [0u8]);
        let _ = stop_send.send(true);
    });

    let connection = Connection::session().await.map_err(|e| e.to_string())?;
    let token = |step| format!("ferese_record_{}_{step}", std::process::id());
    let Some(created) = request(
        &connection,
        "CreateSession",
        &token(0),
        &(HashMap::from([
            ("handle_token", option_string(token(0))),
            ("session_handle_token", option_string(token(0))),
        ]),),
        &mut stop,
    )
    .await?
    else {
        event("cancelled", None, None);
        return Ok(());
    };

    let handle = created
        .get("session_handle")
        .ok_or("Portal returned no session")?;
    // The public portal historically returns this path as a D-Bus string.
    let handle = OwnedObjectPath::try_from(
        String::try_from(handle.try_clone().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // Older/foreign backends keep their normal sharing indicator. Only Ferese
    // can verify this helper's identity and that its parent owns a bar stop button.
    let native = matches!(
        tokio::time::timeout(Duration::from_secs(3), async {
            let control = Proxy::new(
                &connection,
                "org.freedesktop.impl.portal.desktop.ferese",
                "/org/ferese/ScreenRecorder",
                "org.ferese.ScreenRecorder",
            )
            .await?;
            control
                .call::<_, _, ()>("UseBarControls", &(handle.clone(),))
                .await
        })
        .await,
        Ok(Ok(()))
    );

    let session = Proxy::new(
        &connection,
        DESKTOP,
        handle.as_str(),
        "org.freedesktop.portal.Session",
    )
    .await
    .map_err(|e| e.to_string())?;
    let result = record_session(
        &connection,
        &session,
        &handle,
        &token,
        &output,
        &mut stop,
        native,
    )
    .await;
    let _: Result<(), _> = session.call("Close", &()).await;

    match result {
        Ok(Some(path)) => event("saved", None, Some(&path)),
        Ok(None) => event("cancelled", None, None),
        Err(e) => return Err(e),
    }

    Ok(())
}
async fn record_session(
    connection: &Connection,
    session: &Proxy<'_>,
    handle: &OwnedObjectPath,
    token: &impl Fn(u8) -> String,
    output: &Path,
    stop: &mut watch::Receiver<bool>,
    native: bool,
) -> Result<Option<PathBuf>, String> {
    let mut closed = session
        .receive_signal("Closed")
        .await
        .map_err(|e| e.to_string())?;

    if request(
        connection,
        "SelectSources",
        &token(1),
        &(
            handle,
            HashMap::from([
                ("handle_token", option_string(token(1))),
                ("types", 1u32.into()),
                ("multiple", false.into()),
                ("cursor_mode", 2u32.into()),
            ]),
        ),
        stop,
    )
    .await?
    .is_none()
    {
        return Ok(None);
    }

    event("selecting", None, None);

    let Some(started) = request(
        connection,
        "Start",
        &token(2),
        &(
            handle,
            "",
            HashMap::from([("handle_token", option_string(token(2)))]),
        ),
        stop,
    )
    .await?
    else {
        return Ok(None);
    };

    let streams: Vec<(u32, Options)> = started
        .get("streams")
        .ok_or("Portal returned no video stream")?
        .try_clone()
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|e: zbus::zvariant::Error| e.to_string())?;

    if streams.len() != 1 {
        return Err("Recording requires exactly one display".into());
    }

    let recording = if native {
        let control = Proxy::new(
            connection,
            "org.freedesktop.impl.portal.desktop.ferese",
            "/org/ferese/ScreenRecorder",
            "org.ferese.ScreenRecorder",
        )
        .await
        .map_err(|e| e.to_string())?;
        let options: String = tokio::time::timeout(
            Duration::from_secs(3),
            control.call("RecordingOptions", &(handle,)),
        )
        .await
        .map_err(|_| "Recording options timed out")?
        .map_err(|e| {
            format!(
                "Cannot read recording options; update the Ferese portal and recorder together: {e}"
            )
        })?;
        serde_json::from_str::<Recording>(&options).map_err(|e| e.to_string())?
    } else {
        Recording::default()
    };

    let portal = Proxy::new(
        connection,
        DESKTOP,
        DESKTOP_PATH,
        "org.freedesktop.portal.ScreenCast",
    )
    .await
    .map_err(|e| e.to_string())?;
    let remote: OwnedFd = portal
        .call("OpenPipeWireRemote", &(handle, Options::new()))
        .await
        .map_err(|e| e.to_string())?;

    if *stop.borrow() {
        return Ok(None);
    }

    let stop_capture = Arc::new(AtomicBool::new(*stop.borrow()));
    let worker_stop = stop_capture.clone();
    let node = streams[0].0;
    let output = output.to_owned();
    let mut worker = tokio::task::spawn_blocking(move || {
        let source = gst::ElementFactory::make("pipewiresrc")
            .property("fd", remote.as_raw_fd())
            .property("path", node.to_string())
            .property("do-timestamp", true)
            // Keep queued frames valid when the portal revokes PipeWire buffers.
            .property("always-copy", true)
            .build()
            .map_err(|e| e.to_string())?;
        let audio = audio_sources(recording.audio)?;
        let result = encode(source, audio, recording.crop, &output, worker_stop);
        drop(remote);
        result
    });
    let result = tokio::select! {
        result = &mut worker => result,
        _ = stop.changed() => { stop_capture.store(true, Ordering::Relaxed); worker.await },
        _ = closed.next() => { stop_capture.store(true, Ordering::Relaxed); worker.await },
    };

    result.map_err(|e| e.to_string())?.map(Some)
}

fn output_directory() -> Result<PathBuf, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();

    match args.as_slice() {
        [] => dirs::video_dir()
            .or_else(|| dirs::home_dir().map(|p| p.join("Videos")))
            .map(|p| p.join("Ferese"))
            .ok_or("Cannot find your Videos folder".into()),
        [flag, path] if flag == "--output-dir" => Ok(PathBuf::from(path)),
        _ => Err("Usage: ferese-record [--output-dir DIRECTORY]".into()),
    }
}

fn recording_file(directory: &Path) -> Result<(File, PathBuf, PathBuf), String> {
    std::fs::create_dir_all(directory)
        .map_err(|e| format!("Cannot create recording folder: {e}"))?;

    let name = format!(
        "Recording-{}-{}",
        jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S"),
        std::process::id()
    );

    for suffix in 0..100 {
        let final_path = directory.join(format!("{name}-{suffix}.webm"));
        let partial = final_path.with_extension("webm.part");

        if final_path.exists() {
            continue;
        }

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
        {
            Ok(file) => return Ok((file, partial, final_path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot save recording: {e}")),
        }
    }

    Err("Cannot choose a unique recording filename".into())
}

fn audio_sources(audio: Audio) -> Result<Vec<gst::Element>, String> {
    let devices: &[&str] = match audio {
        Audio::Off => &[],
        Audio::System => &["@DEFAULT_MONITOR@"],
        Audio::Microphone => &["@DEFAULT_SOURCE@"],
        Audio::Both => &["@DEFAULT_MONITOR@", "@DEFAULT_SOURCE@"],
    };
    devices.iter().map(|device| {
        gst::ElementFactory::make("pulsesrc")
            .property("device", *device)
            .property("client-name", "Ferese Screen Recorder")
            .property("do-timestamp", true)
            .property("provide-clock", false)
            .build().map_err(|e| format!("Cannot record {}. Install the GStreamer audio plugins and run PipeWire-Pulse or PulseAudio: {e}", audio.label()))
    }).collect()
}

fn element(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|e| format!("Recording needs the GStreamer {factory} plugin: {e}"))
}

fn encode(
    source: gst::Element,
    audio: Vec<gst::Element>,
    crop: Option<crate::recording::Crop>,
    output: &Path,
    stop: Arc<AtomicBool>,
) -> Result<PathBuf, String> {
    if let Some(crop) = crop {
        crop.validate()?;
    }

    let (file, partial, destination) = recording_file(output)?;
    let pipeline = gst::Pipeline::new();
    let result = (|| {
        let queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 4u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|e| e.to_string())?;
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| e.to_string())?;
        let encoder = gst::ElementFactory::make("vp8enc")
            .property("deadline", 1i64)
            .property("cpu-used", 8i32)
            .property("threads", 4i32)
            .property("target-bitrate", 8_000_000i32)
            .property("keyframe-max-dist", 60i32)
            .build()
            .map_err(|e| e.to_string())?;
        let mux = gst::ElementFactory::make("webmmux")
            .build()
            .map_err(|e| e.to_string())?;
        let sink = gst::ElementFactory::make("fdsink")
            .property("fd", file.as_raw_fd())
            .property("sync", false)
            .build()
            .map_err(|e| e.to_string())?;

        pipeline
            .add_many([&source, &queue, &convert, &encoder, &mux, &sink])
            .map_err(|e| e.to_string())?;

        if let Some(crop) = crop {
            let filter = gst::ElementFactory::make("videocrop")
                .property("left", crop.left)
                .property("top", crop.top)
                .property("right", crop.right)
                .property("bottom", crop.bottom)
                .build()
                .map_err(|e| e.to_string())?;
            pipeline.add(&filter).map_err(|e| e.to_string())?;
            gst::Element::link_many([&source, &queue, &filter, &convert, &encoder, &mux, &sink])
                .map_err(|e| e.to_string())?;
        } else {
            gst::Element::link_many([&source, &queue, &convert, &encoder, &mux, &sink])
                .map_err(|e| e.to_string())?;
        }

        let mut audio_inputs = Vec::new();
        let audio_seen = Arc::new(AtomicBool::new(audio.is_empty()));

        if !audio.is_empty() {
            let mixer = element("audiomixer")?;
            let convert = element("audioconvert")?;
            let resample = element("audioresample")?;
            let encoder = element("opusenc")?;
            let audio_queue = element("queue")?;
            pipeline
                .add_many([&mixer, &convert, &resample, &encoder, &audio_queue])
                .map_err(|e| e.to_string())?;
            gst::Element::link_many([&mixer, &convert, &resample, &encoder, &audio_queue, &mux])
                .map_err(|e| e.to_string())?;
            let seen = audio_seen.clone();
            audio_queue
                .static_pad("src")
                .ok_or("Audio encoder has no output")?
                .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if info.buffer().is_some_and(|b| b.size() > 0) {
                        seen.store(true, Ordering::Relaxed);
                    }
                    gst::PadProbeReturn::Ok
                });

            for source in audio {
                let queue = element("queue")?;
                let convert = element("audioconvert")?;
                let resample = element("audioresample")?;
                let caps = gst::ElementFactory::make("capsfilter")
                    .property(
                        "caps",
                        gst::Caps::builder("audio/x-raw")
                            .field("rate", 48_000i32)
                            .field("channels", 2i32)
                            .build(),
                    )
                    .build()
                    .map_err(|e| e.to_string())?;
                pipeline
                    .add_many([&source, &queue, &convert, &resample, &caps])
                    .map_err(|e| e.to_string())?;
                gst::Element::link_many([&source, &queue, &convert, &resample, &caps, &mixer])
                    .map_err(|e| e.to_string())?;
                audio_inputs.push((source, queue));
            }
        }

        let ended = stop.clone();
        source
            .static_pad("src")
            .ok_or("Capture has no video output")?
            .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                if info
                    .event()
                    .is_some_and(|event| matches!(event.view(), gst::EventView::Eos(_)))
                {
                    ended.store(true, Ordering::Relaxed);
                }
                gst::PadProbeReturn::Ok
            });

        let announced = Arc::new(AtomicBool::new(false));
        let notice = announced.clone();
        source
            .static_pad("src")
            .ok_or("Capture has no video output")?
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if info.buffer().is_some_and(|b| b.size() > 0)
                    && !notice.swap(true, Ordering::Relaxed)
                {
                    event("recording", None, None);
                }
                gst::PadProbeReturn::Ok
            });

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| e.to_string())?;

        let bus = pipeline.bus().ok_or("Encoder has no message bus")?;
        let mut finishing = None;
        let started = Instant::now();
        let mut failure = None;

        loop {
            if stop.load(Ordering::Relaxed) && finishing.is_none() {
                event("saving", None, None);
                finishing = Some(Instant::now());
                // Inject after the capture source: it may already have disappeared
                // when the portal closes on lock or monitor removal.
                let _ = queue
                    .static_pad("sink")
                    .ok_or("Encoder queue has no input")?
                    .send_event(gst::event::Eos::new());
                for (source, queue) in &audio_inputs {
                    // Audio devices are still alive when video is revoked.
                    // Let each source order EOS after its own startup events and
                    // buffers; injecting into its queue can race late caps.
                    if !source.send_event(gst::event::Eos::new()) {
                        let _ = queue
                            .static_pad("sink")
                            .ok_or("Audio queue has no input")?
                            .send_event(gst::event::Eos::new());
                    }
                }
            }

            if finishing.is_some_and(|t: Instant| t.elapsed() > Duration::from_secs(10)) {
                return Err(failure.unwrap_or("Finishing the recording timed out".into()));
            }

            if !announced.load(Ordering::Relaxed) && started.elapsed() > Duration::from_secs(15) {
                return Err("No video frames arrived from the selected display".into());
            }

            if !audio_seen.load(Ordering::Relaxed) && started.elapsed() > Duration::from_secs(15) {
                failure = Some("No audio arrived from the selected audio source".into());
                stop.store(true, Ordering::Relaxed);
            }

            let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) else {
                continue;
            };

            match message.view() {
                gst::MessageView::Eos(_) => {
                    if !announced.load(Ordering::Relaxed) {
                        return Err("Recording ended before receiving video".into());
                    }

                    if !audio_seen.load(Ordering::Relaxed) {
                        return Err("Recording ended before receiving audio".into());
                    }

                    if let Some(failure) = failure {
                        return Err(failure);
                    }

                    return Ok(());
                }
                gst::MessageView::Error(error) => {
                    failure = Some(format!("Recording interrupted: {}", error.error()));
                    stop.store(true, Ordering::Relaxed);
                }
                _ => (),
            }
        }
    })();

    let _ = pipeline.set_state(gst::State::Null);
    result.map_err(|e| format!("{e}. Incomplete file: {}", partial.display()))?;
    file.sync_all()
        .map_err(|e| format!("Cannot finish saving {}: {e}", partial.display()))?;
    std::fs::rename(&partial, &destination)
        .map_err(|e| format!("Cannot finish saving {}: {e}", partial.display()))?;

    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_do_not_overwrite_existing_recordings() {
        let directory = tempfile::tempdir().unwrap();
        let (_, first, _) = recording_file(directory.path()).unwrap();
        let (_, second, _) = recording_file(directory.path()).unwrap();
        assert_ne!(first, second);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(first).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    #[ignore = "requires an explicitly provided temporary PipeWire test node"]
    fn encoder_finishes_pipewire_stream() {
        gst::init().unwrap();
        let node = std::env::var("FERESE_TEST_RECORD_NODE").expect("temporary test node required");
        let directory = tempfile::tempdir().unwrap();
        let source = gst::ElementFactory::make("pipewiresrc")
            .property("path", node)
            .property("do-timestamp", true)
            // Keep queued frames valid when the portal revokes PipeWire buffers.
            .property("always-copy", true)
            .build()
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let request_stop = stop.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            request_stop.store(true, Ordering::Relaxed);
        });
        let result = encode(source, Vec::new(), None, directory.path(), stop);
        stopper.join().unwrap();
        assert_video_decodes(&result.unwrap());
    }

    #[test]
    #[ignore = "requires GStreamer video plugins"]
    fn encoder_finishes_a_real_webm() {
        gst::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = gst::ElementFactory::make("videotestsrc")
            .property("num-buffers", 90i32)
            .property("is-live", true)
            .build()
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let request_stop = stop.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(350));
            request_stop.store(true, Ordering::Relaxed);
        });
        let path = encode(source, Vec::new(), None, directory.path(), stop).unwrap();
        stopper.join().unwrap();
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[..4], &[0x1a, 0x45, 0xdf, 0xa3]);
        assert!(data.len() > 1000);
        assert_video_decodes(&path);
    }

    #[test]
    #[ignore = "requires GStreamer video and audio plugins"]
    fn encoder_saves_cropped_video_and_mixed_audio_when_stopped() {
        assert_cropped_audio_recording(false, 2, None);
    }

    #[test]
    #[ignore = "requires GStreamer video and audio plugins"]
    fn video_eos_finishes_live_audio_without_waiting_for_the_user() {
        // Models the portal ending video on lock or monitor removal while the
        // audio devices remain available. Every track must finish together.
        assert_cropped_audio_recording(true, 1, None);
    }

    #[test]
    #[ignore = "requires an explicitly provided isolated PulseAudio test server"]
    fn encoder_captures_isolated_system_and_microphone_audio() {
        // The isolated fixture plays 440 Hz on the default output and 660 Hz
        // on the default microphone. Both tones must survive combined capture.
        let server = std::env::var("FERESE_TEST_RECORD_PULSE_SERVER")
            .expect("isolated audio server required");
        assert_eq!(std::env::var("PULSE_SERVER").unwrap(), server);
        let socket = Path::new(
            server
                .strip_prefix("unix:")
                .expect("explicit Unix socket required"),
        );
        assert!(socket.starts_with(std::env::temp_dir()));
        for audio in [
            Audio::System,
            Audio::Microphone,
            Audio::Both,
            Audio::Both,
            Audio::Both,
        ] {
            assert_cropped_audio_recording(false, 0, Some(audio));
        }
    }

    fn assert_cropped_audio_recording(
        video_eos: bool,
        audio_count: usize,
        selected_audio: Option<Audio>,
    ) {
        use crate::recording::Crop;
        use std::sync::Mutex;
        gst::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let video = gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .property("num-buffers", if video_eos { 18i32 } else { -1i32 })
            .build()
            .unwrap();
        let audio = if let Some(selected_audio) = selected_audio {
            audio_sources(selected_audio).unwrap()
        } else {
            (0..audio_count)
                .map(|index| {
                    gst::ElementFactory::make("audiotestsrc")
                        .property("is-live", true)
                        .property("freq", 440. + index as f64 * 220.)
                        .property("volume", 0.2f64)
                        .build()
                        .unwrap()
                })
                .collect()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let request_stop = stop.clone();
        let stopper = (!video_eos).then(|| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(700));
                request_stop.store(true, Ordering::Relaxed);
            })
        });
        let started = Instant::now();
        // videotestsrc negotiates 320x240. Crop before RGB conversion, including
        // odd edge offsets, and verify the saved stream's negotiated dimensions.
        let path = encode(
            video,
            audio,
            Some(Crop {
                left: 13,
                top: 17,
                right: 106,
                bottom: 94,
            }),
            directory.path(),
            stop,
        )
        .unwrap();
        if let Some(stopper) = stopper {
            stopper.join().unwrap();
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!path.with_extension("webm.part").exists());
        let decoder = gst::parse::launch(
            "filesrc name=input ! matroskademux name=demux \
             demux.video_0 ! queue ! vp8dec ! fakesink name=video \
             demux.audio_0 ! queue ! opusdec ! audioconvert ! \
             audio/x-raw,format=S16LE,rate=48000,channels=1 ! fakesink name=audio",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        decoder
            .by_name("input")
            .unwrap()
            .set_property("location", path.to_str().unwrap());
        let geometry = Arc::new(Mutex::new(None));
        let observed = geometry.clone();
        decoder
            .by_name("video")
            .unwrap()
            .static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
                let caps = pad.current_caps().unwrap();
                let structure = caps.structure(0).unwrap();
                *observed.lock().unwrap() = Some((
                    structure.get::<i32>("width").unwrap(),
                    structure.get::<i32>("height").unwrap(),
                ));
                gst::PadProbeReturn::Ok
            });
        let samples = Arc::new(Mutex::new(Vec::new()));
        let observed = samples.clone();
        decoder
            .by_name("audio")
            .unwrap()
            .static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let buffer = info.buffer().unwrap();
                let data = buffer.map_readable().unwrap();
                observed.lock().unwrap().extend(
                    data.as_slice()
                        .chunks_exact(2)
                        .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
                );
                gst::PadProbeReturn::Ok
            });
        decoder.set_state(gst::State::Playing).unwrap();
        let result = decoder.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        decoder.set_state(gst::State::Null).unwrap();
        assert!(
            matches!(
                result.as_ref().map(|m| m.view()),
                Some(gst::MessageView::Eos(_))
            ),
            "Saved recording did not decode: {result:?}"
        );
        assert_eq!(*geometry.lock().unwrap(), Some((201, 129)));
        let samples = samples.lock().unwrap();
        assert!(samples.len() >= 4800, "Less than 100 ms of decoded audio");
        let frequencies = match selected_audio {
            Some(Audio::System) => vec![440.],
            Some(Audio::Microphone) => vec![660.],
            Some(Audio::Both) => vec![440., 660.],
            Some(Audio::Off) => unreachable!("These tests require audio"),
            None => (0..audio_count)
                .map(|index| 440. + index as f64 * 220.)
                .collect(),
        };
        for frequency in frequencies {
            let amplitude = tone_amplitude(&samples, frequency);
            assert!(
                amplitude > 0.05,
                "Missing {frequency} Hz source in {selected_audio:?} recording: amplitude {amplitude:.4}"
            );
        }
    }

    fn tone_amplitude(samples: &[i16], frequency: f64) -> f64 {
        // Analyze the middle half to avoid device startup and encoder padding.
        // A Hann window limits leakage from the other tone and codec noise.
        let samples = &samples[samples.len() / 4..samples.len() * 3 / 4];
        let mut real = 0.;
        let mut imaginary = 0.;
        let mut weight = 0.;
        for (index, sample) in samples.iter().enumerate() {
            let window = 0.5
                - 0.5 * (std::f64::consts::TAU * index as f64 / (samples.len() - 1) as f64).cos();
            let angle = std::f64::consts::TAU * frequency * index as f64 / 48_000.;
            let sample = *sample as f64 / i16::MAX as f64 * window;
            real += sample * angle.cos();
            imaginary += sample * angle.sin();
            weight += window;
        }
        2. * real.hypot(imaginary) / weight
    }

    #[test]
    fn frequency_check_rejects_a_mix_with_either_source_missing() {
        let tone = |frequency: f64| {
            (0..24_000)
                .map(|index| {
                    (0.2 * i16::MAX as f64
                        * (std::f64::consts::TAU * frequency * index as f64 / 48_000.).sin())
                        as i16
                })
                .collect::<Vec<_>>()
        };
        let system = tone(440.);
        let microphone = tone(660.);
        let both = system
            .iter()
            .zip(&microphone)
            .map(|(a, b)| a + b)
            .collect::<Vec<_>>();
        assert!(tone_amplitude(&both, 440.) > 0.05);
        assert!(tone_amplitude(&both, 660.) > 0.05);
        assert!(tone_amplitude(&microphone, 440.) < 0.01);
        assert!(tone_amplitude(&system, 660.) < 0.01);
    }

    fn assert_video_decodes(path: &Path) {
        let decoder = gst::parse::launch("filesrc name=input ! matroskademux ! vp8dec ! fakesink")
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        decoder
            .by_name("input")
            .unwrap()
            .set_property("location", path.to_str().unwrap());
        decoder.set_state(gst::State::Playing).unwrap();
        let message = decoder.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let duration = decoder.query_duration::<gst::ClockTime>();
        decoder.set_state(gst::State::Null).unwrap();
        assert!(
            duration.is_some_and(|d| d > gst::ClockTime::from_mseconds(100)),
            "Invalid saved duration: {duration:?}"
        );
        assert!(
            matches!(
                message.as_ref().map(|m| m.view()),
                Some(gst::MessageView::Eos(_))
            ),
            "Saved video did not decode: {message:?}"
        );
    }
}
