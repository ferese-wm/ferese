use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calloop::{EventLoop, LoopSignal, PostAction, channel};
use reis::calloop::{EisRequestSource, EisRequestSourceEvent};
use reis::eis;
use reis::enumflags2::BitFlags;
use reis::request::{Connection, Device, DeviceCapability, EisRequest};
use serde_json::Value;

use crate::backend::Cancel;

pub(crate) struct Worker {
    sender: channel::SyncSender<Vec<Value>>,
    signal: LoopSignal,
    pub(crate) ready: Arc<AtomicBool>,
}

impl Worker {
    pub(crate) fn start(
        capabilities: u32,
        keymap: &str,
        cancel: Arc<Cancel>,
    ) -> Result<(Self, OwnedFd), String> {
        if keymap.is_empty() || keymap.len() > 256 * 1024 {
            return Err("Invalid captured keyboard map".into());
        }
        let mut keymap_file = tempfile::tempfile().map_err(|e| e.to_string())?;
        keymap_file
            .write_all(keymap.as_bytes())
            .and_then(|_| keymap_file.write_all(&[0]))
            .map_err(|e| e.to_string())?;
        let keymap_size = keymap.len() as u32 + 1;
        let (server, client) = UnixStream::pair().map_err(|e| e.to_string())?;
        let ready = Arc::new(AtomicBool::new(false));
        let thread_ready = ready.clone();
        let (startup, started) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("ferese-input-eis".into())
            .spawn(move || {
                struct Finish(Arc<Cancel>);
                impl Drop for Finish {
                    fn drop(&mut self) {
                        self.0.stop();
                    }
                }
                let _finish = Finish(cancel);
                let run = || {
                    let context = eis::Context::new(server).map_err(|e| e.to_string())?;
                    let mut event_loop =
                        EventLoop::<State>::try_new().map_err(|e| e.to_string())?;
                    let signal = event_loop.get_signal();
                    let (sender, receiver) = channel::sync_channel(4);
                    event_loop
                        .handle()
                        .insert_source(receiver, |event, _, state| match event {
                            channel::Event::Msg(events) => state.events(events),
                            channel::Event::Closed => state.signal.stop(),
                        })
                        .map_err(|e| e.to_string())?;
                    event_loop
                        .handle()
                        .insert_source(
                            EisRequestSource::new(context, 1),
                            |event, connection, state| {
                                let action = match event {
                                    Ok(event) => state.request(connection, event),
                                    Err(_) => {
                                        state.signal.stop();
                                        PostAction::Remove
                                    }
                                };
                                if connection.flush().is_err() {
                                    state.signal.stop();
                                }
                                Ok(action)
                            },
                        )
                        .map_err(|e| e.to_string())?;
                    let mut state = State {
                        signal: signal.clone(),
                        ready: thread_ready.clone(),
                        capabilities,
                        keymap: keymap_file,
                        keymap_size,
                        connection: None,
                        keyboard: None,
                        pointer: None,
                        activation: None,
                        keyboard_ready: false,
                        pointer_ready: false,
                    };
                    startup
                        .send(Ok((sender, signal)))
                        .map_err(|_| "Input transport startup cancelled".to_owned())?;
                    let _ = event_loop.run(None, &mut state, |_| {});
                    if let Some(connection) = state.connection.take() {
                        connection
                            .disconnected(eis::connection::DisconnectReason::Disconnected, None);
                    }
                    state.ready.store(false, Ordering::SeqCst);
                    Ok::<(), String>(())
                };
                if let Err(error) = run() {
                    let _ = startup.send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        let (sender, signal) = started
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "Input transport startup timed out".to_owned())??;
        Ok((
            Self {
                sender,
                signal,
                ready,
            },
            client.into(),
        ))
    }

    pub(crate) fn send(&self, events: Vec<Value>) -> Result<(), String> {
        self.sender
            .try_send(events)
            .map_err(|_| "Input transport is closed or stalled".into())
    }

    pub(crate) fn stop(&self) {
        self.signal.stop();
        self.signal.wakeup();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

struct State {
    signal: LoopSignal,
    ready: Arc<AtomicBool>,
    capabilities: u32,
    keymap: File,
    keymap_size: u32,
    connection: Option<Connection>,
    keyboard: Option<Device>,
    pointer: Option<Device>,
    activation: Option<u32>,
    keyboard_ready: bool,
    pointer_ready: bool,
}

impl State {
    fn request(&mut self, connection: &Connection, event: EisRequestSourceEvent) -> PostAction {
        match event {
            EisRequestSourceEvent::Connected => {
                if connection.context_type() != eis::handshake::ContextType::Receiver {
                    connection.disconnected(
                        eis::connection::DisconnectReason::Mode,
                        Some("Input capture requires a receiver context"),
                    );
                    self.signal.stop();
                    return PostAction::Remove;
                }
                let mut capabilities = BitFlags::<DeviceCapability>::empty();
                if self.capabilities & 1 != 0 {
                    capabilities |= DeviceCapability::Keyboard;
                }
                if self.capabilities & 2 != 0 {
                    capabilities |= DeviceCapability::Pointer
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll;
                }
                let _ = connection.add_seat(Some("Ferese input capture"), capabilities);
                self.connection = Some(connection.clone());
            }
            EisRequestSourceEvent::Request(EisRequest::Bind(request)) => {
                let keyboard = request.capabilities.contains(DeviceCapability::Keyboard)
                    && self.capabilities & 1 != 0;
                let pointer = request.capabilities.contains(DeviceCapability::Pointer)
                    && self.capabilities & 2 != 0;
                if !keyboard && let Some(device) = self.keyboard.take() {
                    device.remove();
                    self.keyboard_ready = false;
                }
                if !pointer && let Some(device) = self.pointer.take() {
                    device.remove();
                    self.pointer_ready = false;
                }
                if keyboard && self.keyboard.is_none() {
                    let device = request.seat.add_device(
                        Some("Ferese keyboard"),
                        eis::device::DeviceType::Virtual,
                        DeviceCapability::Keyboard.into(),
                        |device| {
                            if let Some(keyboard) = device.interface::<eis::Keyboard>() {
                                keyboard.keymap(
                                    eis::keyboard::KeymapType::Xkb,
                                    self.keymap_size,
                                    self.keymap.as_fd(),
                                );
                            }
                        },
                    );
                    self.keyboard_ready = device.device().version() < 3;
                    device.resumed();
                    self.keyboard = Some(device);
                }
                if pointer && self.pointer.is_none() {
                    let caps = request.capabilities
                        & (DeviceCapability::Pointer
                            | DeviceCapability::Button
                            | DeviceCapability::Scroll);
                    let device = request.seat.add_device(
                        Some("Ferese pointer"),
                        eis::device::DeviceType::Virtual,
                        caps,
                        |_| {},
                    );
                    self.pointer_ready = device.device().version() < 3;
                    device.resumed();
                    self.pointer = Some(device);
                }
                let ready = self.keyboard.is_some() || self.pointer.is_some();
                self.ready.store(
                    ready
                        && (self.keyboard.is_none() || self.keyboard_ready)
                        && (self.pointer.is_none() || self.pointer_ready),
                    Ordering::SeqCst,
                );
                if self.activation.is_some() || !ready {
                    self.signal.stop();
                }
            }
            EisRequestSourceEvent::Request(EisRequest::Ready(request)) => {
                if self
                    .keyboard
                    .as_ref()
                    .is_some_and(|device| device.device() == request.device.device())
                {
                    self.keyboard_ready = true;
                } else if self
                    .pointer
                    .as_ref()
                    .is_some_and(|device| device.device() == request.device.device())
                {
                    self.pointer_ready = true;
                } else {
                    self.signal.stop();
                    return PostAction::Remove;
                }
                self.ready.store(
                    (self.keyboard.is_none() || self.keyboard_ready)
                        && (self.pointer.is_none() || self.pointer_ready),
                    Ordering::SeqCst,
                );
            }
            EisRequestSourceEvent::Request(
                EisRequest::Disconnect | EisRequest::DeviceClosed(_),
            ) => {
                self.signal.stop();
                return PostAction::Remove;
            }
            EisRequestSourceEvent::Request(_) => {
                connection.disconnected(
                    eis::connection::DisconnectReason::Mode,
                    Some("Input capture does not accept injected input"),
                );
                self.signal.stop();
                return PostAction::Remove;
            }
        }
        PostAction::Continue
    }

    fn events(&mut self, events: Vec<Value>) {
        for event in events {
            match event["type"].as_str() {
                Some("activated") => {
                    let activation = event["activation_id"].as_u64().unwrap_or(0) as u32;
                    self.activation = Some(activation);
                    let device_iter = [&self.keyboard, &self.pointer].into_iter().flatten();
                    for device in device_iter {
                        device.start_emulating(activation);
                    }
                }
                Some("deactivated" | "disabled" | "closed") if self.activation.take().is_some() => {
                    let device_iter = [&self.keyboard, &self.pointer].into_iter().flatten();
                    for device in device_iter {
                        device.stop_emulating();
                    }
                }
                Some("keys") if self.activation.is_some() => {
                    if let Some(device) = &self.keyboard
                        && let Some(keyboard) = device.interface::<eis::Keyboard>()
                    {
                        let key_iter = event["keys"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_u64);
                        for key in key_iter {
                            keyboard.key(key as u32, eis::keyboard::KeyState::Press);
                        }
                        device.frame(now());
                    }
                }
                Some("key") if self.activation.is_some() => {
                    if let Some(device) = &self.keyboard
                        && let Some(keyboard) = device.interface::<eis::Keyboard>()
                    {
                        keyboard.key(
                            event["key"].as_u64().unwrap_or(0) as u32,
                            if event["pressed"] == true {
                                eis::keyboard::KeyState::Press
                            } else {
                                eis::keyboard::KeyState::Released
                            },
                        );
                        device.frame(now());
                    }
                }
                Some("motion" | "button" | "scroll") if self.activation.is_some() => {
                    if let Some(device) = &self.pointer {
                        match event["type"].as_str() {
                            Some("motion") => {
                                if let Some(pointer) = device.interface::<eis::Pointer>() {
                                    pointer.motion_relative(
                                        event["x"].as_f64().unwrap_or(0.0) as f32,
                                        event["y"].as_f64().unwrap_or(0.0) as f32,
                                    );
                                }
                            }
                            Some("button") => {
                                if let Some(button) = device.interface::<eis::Button>() {
                                    button.button(
                                        event["button"].as_u64().unwrap_or(0) as u32,
                                        if event["pressed"] == true {
                                            eis::button::ButtonState::Press
                                        } else {
                                            eis::button::ButtonState::Released
                                        },
                                    );
                                }
                            }
                            Some("scroll") => {
                                if let Some(scroll) = device.interface::<eis::Scroll>() {
                                    let x = event["x"].as_f64().unwrap_or(0.0) as f32;
                                    let y = event["y"].as_f64().unwrap_or(0.0) as f32;
                                    if !event["v120_x"].is_null() || !event["v120_y"].is_null() {
                                        scroll.scroll_discrete(
                                            event["v120_x"].as_f64().unwrap_or(0.0) as i32,
                                            event["v120_y"].as_f64().unwrap_or(0.0) as i32,
                                        );
                                    } else if x != 0.0 || y != 0.0 {
                                        scroll.scroll(x, y);
                                    }
                                    let stop_x = event["stop_x"] == true;
                                    let stop_y = event["stop_y"] == true;
                                    if stop_x || stop_y {
                                        scroll.scroll_stop(stop_x as u32, stop_y as u32, 0);
                                    }
                                }
                            }
                            _ => (),
                        }
                        device.frame(now());
                    }
                }
                _ => (),
            }
        }
        if let Some(connection) = &self.connection
            && connection.flush().is_err()
        {
            self.signal.stop();
        }
    }
}

fn now() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the initialized output pointer covers a timespec allocation.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0 {
        time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;

    #[test]
    #[ignore = "requires Python and the system libei receiver library"]
    fn libei_receives_keymap_motion_keyboard_and_explicit_axis_stop() {
        let cancel = Arc::new(Cancel::default());
        let map = "xkb_keymap { xkb_keycodes { include \"evdev+aliases(qwerty)\" }; xkb_types { include \"complete\" }; xkb_compatibility { include \"complete\" }; xkb_symbols { include \"pc+us+inet(evdev)\" }; };";
        let (worker, fd) = Worker::start(3, map, cancel.clone()).unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scripts/tests/fixtures/eis-receiver.py");
        let mut client = Command::new("python3")
            .arg(fixture)
            .stdin(Stdio::from(fd))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker.ready.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !worker.ready.load(Ordering::SeqCst) {
            let _ = client.kill();
            let output = client.wait_with_output().unwrap();
            panic!(
                "Receiver did not bind: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        worker
            .send(vec![
                json!({"type":"activated", "activation_id":1}),
                json!({"type":"motion", "x":10.0, "y":-5.0}),
                json!({"type":"motion", "x":10.0, "y":-5.0}),
                json!({"type":"key", "key":30, "pressed":true}),
                json!({"type":"key", "key":30, "pressed":false}),
                json!({"type":"scroll", "x":0.0, "y":4.0}),
                json!({"type":"scroll", "x":0.0, "y":0.0, "stop_x":false, "stop_y":true}),
                json!({"type":"deactivated"}),
            ])
            .unwrap();
        let output = client.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("\"keymap\": true"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cancel.stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            cancel.stopped.load(Ordering::SeqCst),
            "Receiver disconnect must revoke capture"
        );
    }
}
