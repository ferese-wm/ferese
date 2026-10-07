use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, Instant};
use std::{fs, io, thread};

use ferese_core::LayoutMode;
use ferese_ipc::{Request, Response, VERSION};

mod transport;
use ferese_layout::Direction;
use serde_json::{Value, json};
use smithay::output::Output;
use smithay::reexports::calloop::{EventLoop, LoopHandle, LoopSignal, RegistrationToken, channel, timer};
use smithay::utils::Transform;

use crate::Ferese;
use crate::config::OutputTransform;
use crate::handlers::screenshot::{Action, Geometry, OutputLayout, PartOutcome, PartSender, parse_geometry, plan};
use crate::handlers::screenshot_worker::{Encoded, Job, Worker};
use crate::handlers::{screencopy, screenshot_worker};

const REQUEST_QUEUE_CAPACITY: usize = 128;
const MAX_CONNECTIONS: usize = 64;
const RESULT_QUEUE_CAPACITY: usize = 8;
// The encode thread pushes one result per job, and at most QUEUE_CAPACITY jobs
// can be queued while a further one is in flight. Keeping the result queue
// larger than that is what stops the worker's send from ever blocking, which
// would otherwise wedge it whenever the event loop is busy.
const _: () = assert!(
    RESULT_QUEUE_CAPACITY > crate::handlers::screenshot_worker::QUEUE_CAPACITY + 1,
    "the result queue must outsize the job queue plus the in-flight job"
);
const SWEEP_INTERVAL: Duration = crate::handlers::screenshot_worker::SWEEP_INTERVAL;

#[derive(Debug)]
struct IpcCall {
    native_portal: bool,
    owner: u64,
    request: Request,
    response: SyncSender<Response>,
    events: Option<SyncSender<Arc<ferese_ipc::events::Event>>>,
}

#[derive(Debug)]
enum IpcEvent {
    Call(IpcCall),
    Closed(u64),
}

struct IpcConnection {
    owner: u64,
    sender: channel::SyncSender<IpcEvent>,
}

impl Drop for IpcConnection {
    fn drop(&mut self) {
        let _ = self.sender.send(IpcEvent::Closed(self.owner));
    }
}

struct ConnectionPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) struct IpcSocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
    _instance: Option<tempfile::TempDir>,
}

pub(crate) struct Endpoint {
    pub(crate) path: PathBuf,
    instance: Option<tempfile::TempDir>,
}

impl Endpoint {
    pub(crate) fn desktop() -> io::Result<Self> {
        Self::desktop_at(ferese_ipc::socket::runtime_default()?)
    }

    fn desktop_at(path: PathBuf) -> io::Result<Self> {
        prepare_parent(&path)?;
        Ok(Self { path, instance: None })
    }

    pub(crate) fn instance() -> io::Result<Self> {
        Self::instance_at(ferese_ipc::socket::runtime_default()?)
    }

    fn instance_at(default: PathBuf) -> io::Result<Self> {
        let root = default.parent().expect("runtime control socket has a parent");
        prepare_directory(root)?;
        let instances = root.join("instances");
        prepare_directory(&instances)?;
        let instance = tempfile::Builder::new()
            .prefix(&format!("{}-", std::process::id()))
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(instances)?;
        Ok(Self {
            path: instance.path().join("control.sock"),
            instance: Some(instance),
        })
    }
}

impl Drop for IpcSocketGuard {
    fn drop(&mut self) {
        if self.path.symlink_metadata().is_ok_and(|metadata| {
            metadata.file_type().is_socket() && metadata.dev() == self.device && metadata.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn init(
    event_loop: &mut EventLoop<'static, Ferese>,
    endpoint: Endpoint,
) -> Result<ScreenshotInit, Box<dyn std::error::Error>> {
    let Endpoint { path, instance } = endpoint;
    prepare_parent(&path)?;
    let listener = bind_listener(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(&path)?;
    let guard = IpcSocketGuard {
        path: path.clone(),
        device: metadata.dev(),
        inode: metadata.ino(),
        _instance: instance,
    };

    let (sender, receiver): (channel::SyncSender<IpcEvent>, channel::Channel<IpcEvent>) =
        channel::sync_channel(REQUEST_QUEUE_CAPACITY);
    event_loop.handle().insert_source(receiver, |event, _, state| {
        if let channel::Event::Msg(event) = event {
            let call = match event {
                IpcEvent::Call(call) => call,
                IpcEvent::Closed(owner) => {
                    state.ipc_events.remove(owner);
                    for request in state.screenshot.terminate_owner(owner) {
                        state
                            .pending_screencopies
                            .retain(|capture| capture.request_id() != Some(request));
                    }
                    let captured = state.input_capture.active();
                    state.input_capture.remove_owner(owner);
                    if captured && !state.input_capture.active() {
                        state.restore_input_capture_focus();
                    }
                    state.portal_shortcuts.remove(owner);
                    state.portal_session.remove(owner);
                    state.theme_engine.remove(owner);
                    state.media_engine.remove(owner);
                    state.refresh_idle_inhibition();
                    return;
                }
            };
            if call.request.command == "event-stream" {
                let result = validate_request(&call.request).and_then(|()| {
                    if call.request.args["version"].as_u64() != Some(u64::from(ferese_ipc::events::VERSION)) {
                        return Err(CommandError::new(
                            "unsupported_event_version",
                            "Unsupported event schema version",
                        ));
                    }
                    state
                        .ipc_events
                        .subscribe(call.owner, call.events.expect("event stream sender"))
                        .map_err(|error| CommandError::new("subscriber_limit", error))
                });
                let response = match result {
                    Ok(()) => Response::success(call.request.id, json!({"version": ferese_ipc::events::VERSION})),
                    Err(error) => Response::error(call.request.id, error.code, error.message),
                };
                let _ = call.response.try_send(response);
            } else if call.request.command.starts_with("input-capture-") && !call.native_portal {
                let _ = call.response.try_send(Response::error(
                    call.request.id,
                    "access_denied",
                    "Input capture is restricted to the native portal",
                ));
            } else if call.request.command == "input-capture-watch" {
                if let Err(error) = validate_request(&call.request) {
                    let _ = call
                        .response
                        .try_send(Response::error(call.request.id, error.code, error.message));
                } else if let Some(id) = call.request.args["session"].as_u64() {
                    state
                        .input_capture
                        .watch(call.owner, call.request.id, id, call.response);
                } else {
                    let _ = call.response.try_send(Response::error(
                        call.request.id,
                        "invalid_argument",
                        "Missing capture session",
                    ));
                }
            } else if matches!(call.request.command.as_str(), "media-watch" | "media-action") {
                if let Err(error) = validate_request(&call.request) {
                    let _ = call
                        .response
                        .try_send(Response::error(call.request.id, error.code, error.message));
                } else if state.session_lock.active() {
                    let _ = call.response.try_send(Response::error(
                        call.request.id,
                        "session_locked",
                        "IPC unavailable while session is locked",
                    ));
                } else if call.request.command == "media-watch" {
                    if let Some(since) = call.request.args["since"].as_u64() {
                        state
                            .media_engine
                            .watch(call.owner, call.request.id, since, call.response);
                    } else {
                        let _ = call.response.try_send(Response::error(
                            call.request.id,
                            "invalid_argument",
                            "Missing media revision",
                        ));
                    }
                } else if let Err(error) = state
                    .media_engine
                    .action(call.request.args, Some((call.request.id, call.response.clone())))
                {
                    let _ = call
                        .response
                        .try_send(Response::error(call.request.id, "media_unavailable", error));
                }
            } else if call.request.command == "theme-watch" {
                if let Err(error) = validate_request(&call.request) {
                    let _ = call
                        .response
                        .try_send(Response::error(call.request.id, error.code, error.message));
                } else if let Some(since) = call.request.args["since"].as_u64() {
                    state
                        .theme_engine
                        .watch(call.owner, call.request.id, since, call.response);
                } else {
                    let _ = call.response.try_send(Response::error(
                        call.request.id,
                        "invalid_argument",
                        "Missing theme revision",
                    ));
                }
            } else if call.request.command == "session-watch" {
                if let Err(error) = validate_request(&call.request) {
                    let _ = call
                        .response
                        .send(Response::error(call.request.id, error.code, error.message));
                } else if let Some(since) = call.request.args["since"].as_u64() {
                    state
                        .portal_session
                        .watch(call.owner, call.request.id, since, call.response);
                } else {
                    let _ = call.response.send(Response::error(
                        call.request.id,
                        "invalid_argument",
                        "Missing session revision",
                    ));
                }
            } else if call.request.command == "reload-config" {
                if let Err(error) = validate_request(&call.request) {
                    let _ = call
                        .response
                        .try_send(Response::error(call.request.id, error.code, error.message));
                } else if state.session_lock.active() {
                    let _ = call.response.try_send(Response::error(
                        call.request.id,
                        "session_locked",
                        "IPC unavailable while session is locked",
                    ));
                } else if let Err(error) = state.queue_config_reload(false, Some((call.request.id, call.response))) {
                    tracing::warn!(%error, "cannot queue configuration reload");
                }
            } else if call.request.command == "screenshot-window" {
                state.start_window_screenshot(call.owner, call.request, call.response);
            } else if call.request.command == "screenshot" {
                // Deferred: this path answers the caller itself, exactly
                // once, whenever the request finishes or is terminated.
                state.start_screenshot(call.owner, call.request, call.response);
            } else {
                let response = state.handle_ipc_request(call.owner, call.request);
                let _ = call.response.send(response);
            }
        }
    })?;

    // Screenshot readbacks publish parts here, so the loop wakes as soon as a
    // readback lands, whichever backend produced it.
    let (parts, part_events) = channel::channel::<PartOutcome>();
    event_loop.handle().insert_source(part_events, |event, _, state| {
        if let channel::Event::Msg(outcome) = event {
            state.on_screenshot_part(outcome);
        }
    })?;

    let (encoded, encoded_events) = channel::sync_channel::<Encoded>(RESULT_QUEUE_CAPACITY);
    event_loop.handle().insert_source(encoded_events, |event, _, state| {
        if let channel::Event::Msg(result) = event {
            state.on_screenshot_encoded(result);
        }
    })?;
    let worker = Worker::spawn(encoded);

    let signal = event_loop.get_signal();
    thread::Builder::new()
        .name("ferese-ipc-listener".to_owned())
        .spawn(move || accept_connections(listener, sender, signal))?;

    // A client that dies between receiving a screenshot path and unlinking it
    // leaves the file behind, so staged files are reclaimed at startup and then
    // periodically. The TTL keeps this from disturbing a live request.
    screenshot_worker::sweep_stale_files();
    event_loop
        .handle()
        .insert_source(timer::Timer::from_duration(SWEEP_INTERVAL), |_, _, _| {
            screenshot_worker::sweep_stale_files();
            timer::TimeoutAction::ToDuration(SWEEP_INTERVAL)
        })?;

    // Admission and every terminal coordinator transition update this one-shot
    // source. No deadline source is registered when there are no requests.
    let deadline_timer = DeadlineTimer::new(event_loop.handle(), |state: &mut Ferese, now| {
        for request in state.screenshot.expire_at(now) {
            state
                .pending_screencopies
                .retain(|capture| capture.request_id() != Some(request));
        }
    });

    tracing::info!(path = %path.display(), "Ferese IPC is accepting connections");

    let Some(worker) = worker else {
        return Err("Could not start the screenshot worker".into());
    };
    Ok(ScreenshotInit {
        parts,
        worker,
        deadline_timer,
        _guard: guard,
    })
}

pub(crate) struct ScreenshotInit {
    pub(crate) parts: PartSender,
    pub(crate) worker: Worker,
    pub(crate) deadline_timer: DeadlineTimer<Ferese>,
    pub(crate) _guard: IpcSocketGuard,
}

// The timer callback clears its registration before expiring requests, allowing
// the coordinator observer to arm the next deadline during the same dispatch.
// Keeping absolute Instants also avoids pushing a timeout later on each rearm.
pub(crate) struct DeadlineTimer<Data: 'static> {
    handle: LoopHandle<'static, Data>,
    armed: Rc<RefCell<Option<(Instant, RegistrationToken)>>>,
    expire: fn(&mut Data, Instant),
}

impl<Data: 'static> DeadlineTimer<Data> {
    fn new(handle: LoopHandle<'static, Data>, expire: fn(&mut Data, Instant)) -> Self {
        Self {
            handle,
            armed: Rc::new(RefCell::new(None)),
            expire,
        }
    }

    pub(crate) fn update(&self, deadline: Option<Instant>) {
        if self.armed.borrow().as_ref().map(|(deadline, _)| *deadline) == deadline {
            return;
        }
        if let Some((_, token)) = self.armed.borrow_mut().take() {
            self.handle.remove(token);
        }
        let Some(deadline) = deadline else {
            return;
        };
        let armed = self.armed.clone();
        let expire = self.expire;
        // calloop's Timer registers only in its in-memory wheel and always
        // returns Ok; a failure here would break that infrastructure invariant.
        let token = self
            .handle
            .insert_source(timer::Timer::from_deadline(deadline), move |_, _, state| {
                armed.borrow_mut().take();
                expire(state, Instant::now());
                timer::TimeoutAction::Drop
            })
            .expect("could not register screenshot deadline timer");
        *self.armed.borrow_mut() = Some((deadline, token));
    }
}

impl<Data: 'static> Drop for DeadlineTimer<Data> {
    fn drop(&mut self) {
        if let Some((_, token)) = self.armed.borrow_mut().take() {
            self.handle.remove(token);
        }
    }
}

fn accept_connections(listener: UnixListener, sender: channel::SyncSender<IpcEvent>, signal: LoopSignal) {
    let active_connections = Arc::new(AtomicUsize::new(0));
    let connection_ids = AtomicU64::new(1);

    loop {
        let (stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::error!(%error, "IPC listener stopped");
                return;
            }
        };

        match peer_uid(&stream) {
            Ok(uid) if uid == effective_uid() => {}
            Ok(uid) => {
                tracing::warn!(uid, "rejected IPC connection from a different user");
                continue;
            }
            Err(error) => {
                tracing::warn!(%error, "could not authenticate IPC peer");
                continue;
            }
        }

        let Some(permit) = try_acquire_connection(&active_connections) else {
            tracing::warn!(limit = MAX_CONNECTIONS, "rejected IPC connection at worker limit");
            continue;
        };
        let sender = sender.clone();
        let owner = connection_ids.fetch_add(1, Ordering::Relaxed);
        let signal = signal.clone();
        if let Err(error) = thread::Builder::new()
            .name("ferese-ipc-client".to_owned())
            .spawn(move || {
                let _permit = permit;
                serve_connection(stream, sender, signal, owner);
            })
        {
            tracing::warn!(%error, "could not start IPC connection worker");
        }
    }
}

fn try_acquire_connection(active: &Arc<AtomicUsize>) -> Option<ConnectionPermit> {
    let mut count = active.load(Ordering::Acquire);
    loop {
        if count >= MAX_CONNECTIONS {
            return None;
        }
        match active.compare_exchange_weak(count, count + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(actual) => count = actual,
        }
    }

    Some(ConnectionPermit { active: active.clone() })
}

fn serve_connection(mut stream: UnixStream, sender: channel::SyncSender<IpcEvent>, signal: LoopSignal, owner: u64) {
    let native_portal = crate::handlers::window_capture::is_portal(&stream);
    let _connection = IpcConnection {
        owner,
        sender: sender.clone(),
    };
    let mut idle = Some(transport::FRAME_TIMEOUT);
    let mut owns_lease = false;
    loop {
        let request: Request = match transport::read_request(&stream, idle) {
            Ok(request) => request,
            Err(ferese_ipc::FrameError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                ) =>
            {
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "closing invalid IPC connection");
                return;
            }
        };
        if request.command == "theme-preview" {
            let response = match validate_request(&request) {
                Err(error) => Response::error(request.id, error.code, error.message),
                Ok(()) => match crate::theme::preview(&request.args) {
                    Ok(value) => Response::success(request.id, value),
                    Err(error) => Response::error(request.id, "invalid_config", error),
                },
            };
            if transport::write_response(&stream, &response).is_err() {
                return;
            }
            continue;
        }
        let (response, receiver) = sync_channel(1);
        let event_stream_requested = request.command == "event-stream";
        let (events, event_receiver) = sync_channel(crate::ipc_events::QUEUE_CAPACITY);
        let exit_requested = request.command == "exit";
        let registers_lease = matches!(
            request.command.as_str(),
            "portal-inhibit"
                | "portal-monitor-register"
                | "portal-shortcuts-register"
                | "input-capture-register"
                | "begin-session-end"
        );
        let screenshot_requested = matches!(request.command.as_str(), "screenshot" | "screenshot-window");
        let call = IpcEvent::Call(IpcCall {
            native_portal,
            owner,
            request,
            response,
            events: event_stream_requested.then_some(events),
        });
        if sender.try_send(call).is_err() {
            tracing::warn!("disconnecting IPC client because the request queue is full");
            return;
        }
        let response = loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(response) => break response,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let mut byte = 0_u8;
                    // SAFETY: the live stream owns this fd and byte is writable.
                    let result = unsafe {
                        libc::recv(
                            std::os::fd::AsRawFd::as_raw_fd(&stream),
                            (&mut byte as *mut u8).cast(),
                            1,
                            libc::MSG_PEEK | libc::MSG_DONTWAIT,
                        )
                    };
                    if result == 0 {
                        return;
                    }
                    if result < 0
                        && !matches!(
                            io::Error::last_os_error().kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        )
                    {
                        return;
                    }
                }
            }
        };
        let exit_accepted = exit_requested && response.error.is_none();
        let event_stream_accepted = event_stream_requested && response.error.is_none();
        owns_lease |= registers_lease && response.error.is_none();
        idle = (!owns_lease).then_some(transport::IDLE_TIMEOUT);
        if let Err(error) = transport::write_response(&stream, &response) {
            if screenshot_requested {
                discard_undelivered_screenshot(&response);
            }
            tracing::debug!(%error, "IPC client disconnected before receiving its response");
            return;
        }
        if event_stream_accepted {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            relay_events(&mut stream, event_receiver);
            return;
        }
        if exit_accepted {
            // Acknowledge before stopping so feresectl never races process exit.
            signal.stop();
            signal.wakeup();
            return;
        }
    }
}

// Socket I/O stays on the bounded connection worker, never the compositor loop.
fn relay_events(stream: &mut UnixStream, events: std::sync::mpsc::Receiver<Arc<ferese_ipc::events::Event>>) {
    loop {
        match events.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                if transport::write_response(stream, event.as_ref()).is_err() {
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let mut byte = 0_u8;
                // SAFETY: the stream owns its descriptor and byte is writable.
                let result = unsafe {
                    libc::recv(
                        stream.as_raw_fd(),
                        (&raw mut byte).cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                if result >= 0 {
                    return;
                }
                if !matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    return;
                }
            }
        }
    }
}

fn u32_arg(args: &Value, key: &str) -> Result<u32, CommandError> {
    args[key]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| CommandError::new("invalid_argument", format!("Missing or invalid {key}")))
}

fn discard_undelivered_screenshot(response: &Response) {
    if response.error.is_none()
        && let Some(path) = response.result.as_ref().and_then(|result| result["path"].as_str())
        && let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(%error, "failed to remove an undelivered screenshot");
    }
}

impl Ferese {
    fn handle_ipc_request(&mut self, owner: u64, request: Request) -> Response {
        if self.session_lock.active()
            && !matches!(
                request.command.as_str(),
                "theme-get"
                    | "theme-status"
                    | "portal-shortcuts-poll"
                    | "input-capture-disable"
                    | "input-capture-release"
                    | "get-session-state"
                    | ferese_ipc::xwayland::STATUS_COMMAND
                    | "portal-inhibit"
                    | "portal-monitor-register"
                    | "portal-monitor-ack"
                    | "logind-session-ending"
                    | "cancel-session-end"
            )
        {
            return Response::error(request.id, "session_locked", "IPC unavailable while session is locked");
        }
        if let Err(error) = validate_request(&request) {
            return Response::error(request.id, error.code, error.message);
        }

        match self.dispatch_ipc_command(owner, &request.command, &request.args) {
            Ok(result) => Response::success(request.id, result),
            Err(error) => Response::error(request.id, error.code, error.message),
        }
    }

    fn dispatch_ipc_command(&mut self, owner: u64, command: &str, args: &Value) -> Result<Value, CommandError> {
        match command {
            "theme-get" | "theme-status" => return Ok(self.theme_engine.value()),
            "theme-set-mode" => {
                let mode = serde_json::from_value(args["mode"].clone())
                    .map_err(|error| CommandError::new("invalid_argument", error.to_string()))?;
                return self
                    .set_theme_mode(mode)
                    .map_err(|error| CommandError::new("invalid_config", error));
            }
            "portal-inhibit" => {
                let inhibition = serde_json::from_value(args.clone())
                    .map_err(|error| CommandError::new("invalid_argument", error.to_string()))?;
                self.portal_session
                    .register(owner, inhibition)
                    .map_err(|error| CommandError::new("invalid_argument", error))?;
                self.refresh_idle_inhibition();
                return Ok(json!({}));
            }
            "get-session-state" => {
                return Ok(self.portal_session.snapshot());
            }
            ferese_ipc::xwayland::STATUS_COMMAND => {
                return Ok(crate::xwayland::status_snapshot(self).to_value());
            }
            ferese_ipc::xwayland::RETRY_COMMAND => {
                crate::xwayland::retry(self).map_err(|error| CommandError::new("x11_unavailable", error))?;

                return Ok(crate::xwayland::status_snapshot(self).to_value());
            }
            "portal-monitor-register" => {
                self.portal_session
                    .register_monitor(owner)
                    .map_err(|error| CommandError::new("invalid_argument", error))?;
                let mut result = self.portal_session.snapshot();
                result["monitor-owner"] = json!(owner);
                return Ok(result);
            }
            "portal-monitor-ack" => {
                let token = u32_arg(args, "token")?;
                self.portal_session
                    .acknowledge(owner, token)
                    .map_err(|error| CommandError::new("invalid_argument", error))?;
                return Ok(json!({}));
            }
            "begin-session-end" => {
                self.portal_session.begin_owned_query(owner);
                return Ok(self.portal_session.snapshot());
            }
            "cancel-session-end" => {
                self.portal_session.cancel_query(u32_arg(args, "token")?);
                return Ok(json!({}));
            }
            "validate-session-end" => {
                self.portal_session
                    .validate_end(
                        u32_arg(args, "token")?,
                        u32_arg(args, "inhibitor-revision")?,
                        args["force"]
                            .as_bool()
                            .ok_or_else(|| CommandError::new("invalid_argument", "Missing force confirmation"))?,
                    )
                    .map_err(|error| CommandError::new("inhibited", error))?;
                return Ok(json!({}));
            }
            "commit-session-end" => {
                self.portal_session
                    .commit_end(
                        u32_arg(args, "token")?,
                        u32_arg(args, "inhibitor-revision")?,
                        args["force"]
                            .as_bool()
                            .ok_or_else(|| CommandError::new("invalid_argument", "Missing force confirmation"))?,
                    )
                    .map_err(|error| CommandError::new("inhibited", error))?;
                return Ok(json!({}));
            }
            "logind-session-ending" => {
                let ending = args["ending"]
                    .as_bool()
                    .ok_or_else(|| CommandError::new("invalid_argument", "Missing shutdown state"))?;
                self.portal_session.set_phase(if ending { 3 } else { 1 });
                return Ok(json!({}));
            }
            "portal-shortcuts-register" => {
                let shortcuts = serde_json::from_value(args.clone())
                    .map_err(|error| CommandError::new("invalid_argument", error.to_string()))?;
                self.portal_shortcuts
                    .register(owner, shortcuts, &self.bindings, &self.input_settings)
                    .map_err(|error| CommandError::new("shortcut_conflict", error))?;
                return Ok(json!({}));
            }
            "portal-shortcuts-poll" => {
                return Ok(self.portal_shortcuts.poll(owner, self.session_lock.active()));
            }
            "input-capture-register" => {
                if self.session_lock.active() {
                    return Err(CommandError::new("session_locked", "Session is locked"));
                }
                let id = self
                    .input_capture
                    .register(owner, u32_arg(args, "capabilities")?)
                    .map_err(|e| CommandError::new("invalid_capture", e))?;
                self.refresh_input_capture_zones();
                let keyboard = self.seat.get_keyboard().expect("seat has keyboard");
                let keymap = keyboard.with_xkb_state(self, |state| {
                    let xkb = state.xkb().lock().unwrap();
                    // SAFETY: the ref-counted keymap does not escape the locked Xkb;
                    // only its owned string is retained for the EIS keyboard.
                    unsafe { xkb.keymap() }.get_as_string(smithay::input::keyboard::xkb::KEYMAP_FORMAT_TEXT_V1)
                });
                return Ok(json!({"session":id, "keymap":keymap}));
            }
            "input-capture-zones" => {
                self.refresh_input_capture_zones();
                return self
                    .input_capture
                    .zones(
                        args["session"]
                            .as_u64()
                            .ok_or_else(|| CommandError::new("invalid_capture", "Missing session"))?,
                    )
                    .map_err(|e| CommandError::new("invalid_capture", e));
            }
            "input-capture-barriers" => {
                self.refresh_input_capture_zones();
                let id = args["session"]
                    .as_u64()
                    .ok_or_else(|| CommandError::new("invalid_capture", "Missing session"))?;
                let barriers = serde_json::from_value(args["barriers"].clone())
                    .map_err(|_| CommandError::new("invalid_capture", "Invalid barriers"))?;
                let active = self.input_capture.active();
                let failed = self
                    .input_capture
                    .set_barriers(id, u32_arg(args, "zone_set")?, barriers)
                    .map_err(|e| CommandError::new("invalid_capture", e))?;
                if active && !self.input_capture.active() {
                    self.restore_input_capture_focus();
                }
                return Ok(json!({"failed_barriers":failed}));
            }
            "input-capture-enable" => {
                self.refresh_input_capture_zones();
                let id = args["session"]
                    .as_u64()
                    .ok_or_else(|| CommandError::new("invalid_capture", "Missing session"))?;
                self.input_capture
                    .enable(id)
                    .map_err(|e| CommandError::new("invalid_capture", e))?;
            }
            "input-capture-disable" | "input-capture-release" => {
                let id = args["session"]
                    .as_u64()
                    .ok_or_else(|| CommandError::new("invalid_capture", "Missing session"))?;
                let position = args
                    .get("cursor_position")
                    .filter(|position| !position.is_null())
                    .map(|position| {
                        let position = position
                            .as_array()
                            .filter(|values| values.len() == 2)
                            .ok_or("Invalid cursor position")?;
                        let point = (
                            position[0].as_f64().ok_or("Invalid cursor position")?,
                            position[1].as_f64().ok_or("Invalid cursor position")?,
                        );
                        self.input_capture
                            .valid_position(point)
                            .then_some(point)
                            .ok_or("Cursor position is outside the capture zones")
                    })
                    .transpose()
                    .map_err(|e| CommandError::new("invalid_capture", e))?;
                let active = self.input_capture.active();
                let result = if command == "input-capture-disable" {
                    self.input_capture.disable(id)
                } else {
                    self.input_capture
                        .release(id, args["activation_id"].as_u64().and_then(|id| u32::try_from(id).ok()))
                };
                result.map_err(|e| CommandError::new("invalid_capture", e))?;
                if active && !self.input_capture.active() {
                    if let Some(position) = position
                        && let Some(pointer) = self.seat.get_pointer()
                    {
                        pointer.set_location(position.into());
                    }
                    self.restore_input_capture_focus();
                }
            }
            "exit" => {} // The IPC worker stops the loop after writing the response.
            "request-logout" => self.request_logout_confirmation(),
            "reload-config" => self
                .reload_config()
                .map_err(|e| CommandError::new("invalid_config", e))?,
            "focus" => self.focus_direction(direction_arg(args)?),
            "focus-last-window" => self.focus_last_window(),
            "focus-floating" => self.focus_floating(),
            "focus-mru-next" => self.cycle_focus(false, false),
            "focus-mru-previous" => self.cycle_focus(true, false),
            "move" => self.move_direction(direction_arg(args)?),
            "resize" => self.resize_direction(direction_arg(args)?),
            "workspace" => self.switch_workspace(workspace_arg(args)?),
            "workspace-back-and-forth" => self.workspace_back_and_forth(),
            "move-to-workspace" => self.move_focused_to_workspace(workspace_arg(args)?),
            "toggle-floating" => self.toggle_focused_floating(),
            "toggle-fullscreen" => self.toggle_focused_fullscreen(),
            "toggle-maximized" => self.toggle_focused_maximized(),
            "toggle-layout" => self.toggle_layout_mode(),
            "toggle-overview" => self.toggle_overview(),
            "toggle-display-mode" => {
                self.toggle_display_mode();
            }
            "toggle-keybinding-guide" => {
                if !self.toggle_keybinding_guide() {
                    return Err(CommandError::new(
                        "unavailable",
                        "Shortcut hint requires an unlocked session and an updated Ferese shell",
                    ));
                }
            }
            "cycle-column-width" => self.cycle_focused_column_width(),
            "center-column" => self.center_focused_column(),
            "consume" => self.consume_focused_window(),
            "expel" => self.expel_focused_window(),
            "close" => self.close_focused_window(),
            "get-focused-window" => return Ok(self.focused_window_json()),
            "get-windows" => return Ok(self.windows_json()),
            "media-get" => return Ok(self.media_engine.value()),
            "get-idle-inhibition" => {
                return Ok(json!({
                    "inhibited": self.idle_notifier_state.is_inhibited(),
                    "automatic": self.automatic_idle_inhibited,
                    "fullscreen-playback": self.idle_inhibit.fullscreen_playback,
                    "portal": self.portal_session.idle_inhibited(),
                    "players": self.media_players.iter().map(|player| json!({
                        "name": player.name, "desktop-entry": player.desktop_entry, "playing": player.playing,
                    })).collect::<Vec<_>>(),
                }));
            }
            "get-keybindings" => {
                let map = crate::config::physical_keymap(&self.input_settings).ok();
                return Ok(json!(
                    self.bindings
                        .iter()
                        .filter_map(|binding| binding.guide_entry(map.as_ref()))
                        .take(256)
                        .collect::<Vec<_>>()
                ));
            }
            "has-client-surfaces" => {
                let pid = args
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                    .ok_or_else(|| CommandError::new("invalid_argument", "pid must be an unsigned process ID"))?;
                return Ok(json!(self.has_client_surfaces(pid)));
            }
            "get-workspaces" => return Ok(self.workspaces_json()),
            "outputs" | "get-outputs" => return Ok(self.outputs_json()),
            "output-profiles" => {
                return Ok(json!({
                    "confirmation_pending": self.direct_backend.as_ref().is_some_and(|backend| backend.confirmation_pending()),
                    "manual_profile": self.direct_backend.as_ref().and_then(|backend| backend.manual_outputs.profile.as_ref()),
                    "manual_layout": self.direct_backend.as_ref().and_then(|backend| backend.manual_outputs.layout),
                    "manual_internal": self.direct_backend.as_ref().and_then(|backend| backend.manual_outputs.internal),
                    "profiles": self.output_profiles.iter().map(|profile| json!({
                        "name": profile.name, "layout": profile.layout,
                        "confirm_timeout": profile.confirm_timeout, "lid_policy": profile.lid_policy, "lid_closed": profile.lid_closed, "mirror_source": profile.mirror_source,
                        "outputs": profile.outputs.iter().map(|output| json!({"selector": output.matcher, "required": output.required, "enabled": output.enabled})).collect::<Vec<_>>()
                    })).collect::<Vec<_>>()
                }));
            }
            "output-confirm" | "output-revert" => {
                crate::backends::direct::confirm_output_configuration(self, command == "output-confirm")
                    .map_err(|error| CommandError::new("output_configuration_failed", error))?;
                return Ok(self.outputs_json());
            }
            "output-profile" => {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| CommandError::new("invalid_arguments", "output-profile requires a name or auto"))?;
                crate::backends::direct::set_output_profile(self, name)
                    .map_err(|error| CommandError::new("output_configuration_failed", error))?;
                return Ok(self.outputs_json());
            }
            "output-layout" => {
                let layout = serde_json::from_value::<crate::config::OutputLayout>(args["layout"].clone())
                    .map_err(|_| CommandError::new("invalid_arguments", "unknown display layout"))?;
                crate::backends::direct::set_output_layout(self, layout)
                    .map_err(|error| CommandError::new("output_configuration_failed", error))?;
                return Ok(self.outputs_json());
            }
            "output-internal" => {
                let enabled = args["enabled"]
                    .as_bool()
                    .ok_or_else(|| CommandError::new("invalid_arguments", "output-internal requires on or off"))?;
                crate::backends::direct::set_internal_output(self, enabled)
                    .map_err(|error| CommandError::new("output_configuration_failed", error))?;
                return Ok(self.outputs_json());
            }
            _ => {
                return Err(CommandError::new(
                    "unknown_command",
                    format!("unknown command {command:?}"),
                ));
            }
        }

        Ok(json!({}))
    }

    // Screenshot replies are deferred: this either answers the caller now, or
    // hands the reply to the coordinator, which answers exactly once.
    pub(crate) fn start_screenshot(&mut self, owner: u64, request: Request, response: SyncSender<Response>) {
        // A macro rather than a closure: the message may be a borrowed str or an
        // owned String, and Response::error already accepts either.
        macro_rules! reject {
            ($code:expr, $message:expr) => {
                let _ = response.try_send(Response::error(request.id, $code, $message));
            };
        }
        if self.session_lock.active() {
            reject!("session_locked", "IPC unavailable while session is locked");
            return;
        }
        if !screencopy::capture_allowed() {
            reject!(
                "capture_disabled",
                "Screen capture is disabled for this session; start it with \
                 FERESE_ENABLE_SCREENCOPY=1 to enable screenshots"
            );
            return;
        }
        if let Err(error) = validate_request(&request) {
            reject!(error.code, error.message);
            return;
        }
        let geometry = match geometry_arg(&request.args) {
            Ok(geometry) => geometry,
            Err(message) => {
                reject!("invalid_argument", &message);
                return;
            }
        };
        if self.screenshot_parts.is_none() || self.screenshot_worker.is_none() {
            reject!("unavailable", "Screenshot capture is not available");
            return;
        }
        let Some(parts) = self.screenshot_parts.clone() else {
            reject!("unavailable", "Screenshot capture is not available");
            return;
        };

        // Snapshot the layout, so a later move, rescale, or transform cannot
        // change what this request captures.
        let targets: Vec<(Output, OutputLayout)> = self
            .space
            .outputs()
            .filter_map(|output| {
                let mode = output.current_mode()?;
                let geometry = self.space.output_geometry(output)?;
                Some((
                    output.clone(),
                    OutputLayout {
                        mode_size: mode.size,
                        scale: output.current_scale().fractional_scale(),
                        transform: output.current_transform(),
                        location: geometry.loc,
                    },
                ))
            })
            .collect();
        let layouts: Vec<OutputLayout> = targets.iter().map(|(_, layout)| layout.clone()).collect();
        let planned = match plan(&geometry, &layouts) {
            Ok(planned) => planned,
            Err(message) => {
                reject!("invalid_request", &message);
                return;
            }
        };
        let specs = planned.iter().map(|part| part.spec(&layouts[part.index])).collect();

        // admit stores the reply only on success, so a clone survives the
        // rejection path and every caller is answered exactly once.
        let fallback = response.clone();
        let id = match self.screenshot.admit(owner, request.id, response, specs) {
            Ok(id) => id,
            Err(message) => {
                let _ = fallback.try_send(Response::error(request.id, "screenshot_rejected", message));
                return;
            }
        };

        for (position, part) in planned.iter().enumerate() {
            let (output, _) = &targets[part.index];
            self.pending_screencopies.push(
                crate::handlers::screencopy::PendingScreencopy::owned(
                    id,
                    position,
                    parts.clone(),
                    output.clone(),
                    part.buffer,
                )
                .with_permit(self.screenshot.permit(id)),
            );
        }

        // The nested backend redraws on its refresh timer; this drives the
        // direct backend immediately and is a no-op otherwise.
        let outputs = planned
            .iter()
            .map(|part| targets[part.index].0.clone())
            .collect::<Vec<_>>();
        crate::backends::direct::render_on(self, &outputs);
    }

    fn start_window_screenshot(&mut self, owner: u64, request: Request, response: SyncSender<Response>) {
        let reject = |message: String| {
            let _ = response.try_send(Response::error(request.id, "window_capture_failed", message));
        };
        if self.session_lock.active() || !screencopy::capture_allowed() {
            reject("Screen capture is unavailable".into());
            return;
        }
        if let Err(error) = validate_request(&request) {
            reject(error.message);
            return;
        }
        let Some(id) = request
            .args
            .get("window")
            .and_then(Value::as_u64)
            .map(ferese_layout::WindowId)
        else {
            reject("Expected a window ID".into());
            return;
        };
        let Some(window) = self
            .windows
            .ids()
            .iter()
            .find(|(_, candidate)| **candidate == id)
            .map(|(window, _)| window.clone())
        else {
            reject("Window no longer exists".into());
            return;
        };
        if self.capture_protected(&window) {
            reject("Window is protected from capture".into());
            return;
        }
        let Some(output) = self
            .space
            .outputs()
            .find(|output| self.window_belongs_to_output(id, output))
            .cloned()
        else {
            reject("Window output is unavailable".into());
            return;
        };
        let geometry = window.geometry();
        let scale = output.current_scale().fractional_scale();
        let size = geometry.size.to_physical_precise_round(scale);
        let spec = crate::handlers::screenshot::PartSpec {
            preserve_alpha: true,
            transform: Transform::Normal,
            scale,
            location: (0, 0),
            logical_width: geometry.size.w,
            logical_height: geometry.size.h,
            buffer_width: size.w,
            buffer_height: size.h,
        };
        let fallback = response.clone();
        let capture = match self.screenshot.admit(owner, request.id, response, vec![spec]) {
            Ok(capture) => capture,
            Err(error) => {
                let _ = fallback.try_send(Response::error(request.id, "screenshot_rejected", error));
                return;
            }
        };
        let result = if let Some(backend) = &self.nested_backend {
            match backend.try_borrow_mut() {
                Ok(mut backend) => crate::render::capture_window_buffer(backend.renderer(), &window, geometry, scale),
                Err(_) => Err("Window renderer is busy".into()),
            }
        } else if let Some(backend) = &mut self.direct_backend {
            backend.capture_window_buffer(&window, geometry, &output)
        } else {
            Err("Window renderer is unavailable".into())
        };
        self.on_screenshot_part(PartOutcome {
            request: capture,
            part: 0,
            result,
            permit: self.screenshot.permit(capture),
        });
    }

    pub(crate) fn on_screenshot_part(&mut self, outcome: PartOutcome) {
        let PartOutcome {
            request,
            part,
            result,
            permit,
        } = outcome;
        let action = self.screenshot.on_part(request, part, result);
        // Pixels have moved into the coordinator/job before the channel lease is dropped.
        drop(permit);
        self.apply_screenshot_action(action);
    }

    pub(crate) fn on_screenshot_encoded(&mut self, result: Encoded) {
        let action = self.screenshot.on_encoded(result.request, result.result);
        self.apply_screenshot_action(action);
    }

    fn apply_screenshot_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Encode {
                request,
                frames,
                permit,
            } => {
                let submitted = self.screenshot_worker.as_ref().is_some_and(|worker| {
                    worker
                        .submit(Job {
                            request,
                            frames,
                            _permit: Some(permit),
                        })
                        .is_ok()
                });
                if !submitted {
                    self.screenshot.reject(request, "Screenshot encoding queue is full");
                }
            }
            Action::Delivered(path) => {
                // The caller opens and unlinks it; the path was already sent.
                tracing::debug!(path = %path.display(), "delivered a screenshot");
            }
            Action::DiscardFile(path) => {
                if let Err(error) = fs::remove_file(&path)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    tracing::debug!(
                        %error,
                        path = %path.display(),
                        "failed to remove a staged screenshot"
                    );
                }
            }
        }
    }

    pub(crate) fn ipc_event_snapshot(&self) -> ferese_ipc::events::Snapshot {
        let locked = self.session_lock.active();
        let mut outputs = self.outputs_json();
        if locked {
            for output in outputs.as_array_mut().into_iter().flatten() {
                if let Some(fields) = output.as_object_mut() {
                    fields.remove("workspace");
                    fields.remove("focused");
                }
            }
        }
        let windows = if locked {
            json!([])
        } else {
            Value::Array(self.windows.ordered_ids().filter_map(|id| {
                let record = self.windows.record(id)?;
                let workspace = self.workspaces.workspace_for_window(id)?;
                Some(json!({
                    "id": id.0, "app_id": record.app_id, "title": record.title,
                    "focused": self.focused_window == Some(id), "workspace": workspace.0,
                    "floating": matches!(self.workspaces.placement(id), Some(ferese_core::WindowPlacement::Floating { .. })),
                    "fullscreen": self.workspaces.workspace(workspace).is_some_and(|workspace| workspace.fullscreen == Some(id)),
                }))
            }).collect())
        };
        let live = &self.theme_engine.live;
        let theme = json!({
            "mode": live.mode, "theme": live.theme, "families": live.families,
            "warnings": live.warnings, "error": live.error, "fallback_note": live.fallback_note,
        });
        ferese_ipc::events::Snapshot {
            outputs,
            workspaces: if locked { json!([]) } else { self.workspaces_json() },
            windows,
            focus: if locked {
                Value::Null
            } else {
                json!({
                    "window": self.focused_window.map(|id| id.0),
                    "output": self.output_workspaces.focused_output().map(|id| id.0),
                    "workspace": self.output_workspaces.focused_output().and_then(|id| self.output_workspaces.active_workspace(id)).map(|id| id.0),
                })
            },
            config: ferese_ipc::events::ConfigState {
                revision: self.ipc_events.config_revision,
                error: self.theme_engine.live.error.clone(),
            },
            theme,
            lock: self.session_lock.event_state(),
        }
    }

    fn focused_window_json(&self) -> Value {
        self.focused_window
            .map(|window| json!({ "id": window.0 }))
            .unwrap_or(Value::Null)
    }

    fn has_client_surfaces(&self, pid: u32) -> bool {
        use smithay::reexports::wayland_server::Resource;
        let belongs_to = |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            surface
                .client()
                .and_then(|client| client.get_credentials(&self.display_handle).ok())
                .is_some_and(|credentials| credentials.pid as u32 == pid)
        };
        self.windows
            .ids()
            .keys()
            .filter_map(|window| window.toplevel())
            .any(|toplevel| belongs_to(toplevel.wl_surface()))
            || self.space.outputs().any(|output| {
                smithay::desktop::layer_map_for_output(output)
                    .layers()
                    .any(|layer| belongs_to(layer.wl_surface()))
            })
    }

    fn windows_json(&self) -> Value {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
        let mut windows = self
            .windows
            .ids()
            .iter()
            .filter_map(|(window, id)| {
                let toplevel = window.toplevel()?;
                let (app_id, title) = with_states(toplevel.wl_surface(), |states| {
                    let attributes = states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
                    (
                        attributes.app_id.clone().unwrap_or_default(),
                        attributes.title.clone().unwrap_or_default(),
                    )
                });
                let rect = self.visual_rect_for_window(window);
                Some(json!({
                    "id": id.0,
                    "app_id": app_id,
                    "title": title,
                    "focused": self.focused_window == Some(*id),
                    "mapped": self.space.element_location(window).is_some(),
                    "workspace": self.workspaces.workspace_for_window(*id).map(|workspace| workspace.0),
                    "x": rect.as_ref().map(|rect| rect.loc.x),
                    "y": rect.as_ref().map(|rect| rect.loc.y),
                    "capture_width": window.geometry().size.w,
                    "capture_height": window.geometry().size.h,
                    "width": rect.as_ref().map(|rect| rect.size.w),
                    "height": rect.as_ref().map(|rect| rect.size.h),
                }))
            })
            .collect::<Vec<_>>();
        windows.sort_by_key(|window| window["id"].as_u64());
        Value::Array(windows)
    }

    fn workspaces_json(&self) -> Value {
        let workspaces = self
            .output_workspaces
            .workspace_views(&self.workspaces)
            .into_iter()
            .map(|view| {
                let workspace = self.workspaces.workspace(view.id).expect("workspace view exists");
                let mode = match workspace.layout.mode() {
                    LayoutMode::Scrolling => "scrolling",
                    LayoutMode::Tree => "tree",
                };
                json!({
                    "id": workspace.id.0,
                    "name": view.index.to_string(),
                    "index": view.index,
                    "output": view.output.0,
                    "window_count": view.window_count,
                    "visible": view.visible,
                    "focused": view.focused,
                    "active": view.focused,
                    "layout": mode,
                    "focused_window": workspace.last_focused.map(|window| window.0),
                    "fullscreen_window": workspace.fullscreen.map(|window| window.0),
                })
            })
            .collect::<Vec<_>>();
        Value::Array(workspaces)
    }

    fn outputs_json(&self) -> Value {
        if let Some(backend) = self.direct_backend.as_ref() {
            let focused = self.output_workspaces.focused_output();
            let mut outputs = backend
                .connected_outputs
                .iter()
                .map(|info| {
                    let mapped = self.output_by_identity(&info.identity);
                    let id = self.persistent_output_id(&info.identity);
                    let geometry = mapped.and_then(|output| self.space.output_geometry(output));
                    let workspace = id.and_then(|id| self.output_workspaces.active_workspace(id));
                    let position = geometry
                        .map(|geometry| [geometry.loc.x, geometry.loc.y])
                        .or(info.configured_position);
                    let scale = mapped
                        .map(|output| output.current_scale().fractional_scale())
                        .unwrap_or(info.scale);
                    let transform = mapped
                        .map(|output| transform_name(output.current_transform()))
                        .unwrap_or_else(|| configured_transform_name(info.transform));
                    let modes = info
                        .available_modes
                        .iter()
                        .map(|mode| {
                            json!({
                                "width": mode.width,
                                "height": mode.height,
                                "refresh_millihertz": mode.refresh_millihertz,
                                "refresh_hz": f64::from(mode.refresh_millihertz) / 1_000.0,
                                "preferred": mode.preferred,
                            })
                        })
                        .collect::<Vec<_>>();
                    let current_mode = info.current_mode.map(|mode| {
                        json!({
                            "width": mode.width,
                            "height": mode.height,
                            "refresh_millihertz": mode.refresh_millihertz,
                            "refresh_hz": f64::from(mode.refresh_millihertz) / 1_000.0,
                        })
                    });

                    json!({
                        "id": id.map(|id| id.0),
                        "name": info.connector,
                        "connector": info.connector,
                        "identity": info.identity,
                        "connected": info.connected,
                        "internal": info.internal,
                        "requested_enabled": info.requested_enabled,
                        "applied_enabled": info.enabled,
                        "mirror_source": info.mirror_source,
                        "enabled": info.enabled,
                        "profile": info.profile,
                        "requested_profile": info.requested_profile,
                        "configuration_error": backend.output_configuration_error(),
                        "confirmation_pending": backend.confirmation_pending(),
                        "auto_refresh": info.auto_refresh,
                        "low_power": backend.low_power,
                        "focused": id.is_some() && id == focused,
                        "workspace": workspace.map(|workspace| workspace.0),
                        "x": position.map(|position| position[0]),
                        "y": position.map(|position| position[1]),
                        "width": geometry.map(|geometry| geometry.size.w),
                        "height": geometry.map(|geometry| geometry.size.h),
                        "physical_width_mm": info.physical_size.map(|size| size.0),
                        "physical_height_mm": info.physical_size.map(|size| size.1),
                        "scale": scale,
                        "transform": transform,
                        "current_mode": current_mode,
                        "available_modes": modes,
                    })
                })
                .collect::<Vec<_>>();
            outputs.sort_by(|left, right| left["connector"].as_str().cmp(&right["connector"].as_str()));
            return Value::Array(outputs);
        }

        let focused = self.output_workspaces.focused_output();
        let mut outputs = self
            .space
            .outputs()
            .filter_map(|output| {
                let id = self.output_id(output)?;
                let geometry = self.space.output_geometry(output)?;
                let workspace = self.output_workspaces.active_workspace(id)?;
                let mode = output.current_mode();
                let physical = output.physical_properties().size;
                Some(json!({
                    "id": id.0,
                    "name": output.name(),
                    "connector": output.name(),
                    "identity": Value::Null,
                    "connected": true,
                    "enabled": true,
                    "focused": Some(id) == focused,
                    "workspace": workspace.0,
                    "x": geometry.loc.x,
                    "y": geometry.loc.y,
                    "width": geometry.size.w,
                    "height": geometry.size.h,
                    "physical_width_mm": physical.w,
                    "physical_height_mm": physical.h,
                    "scale": output.current_scale().fractional_scale(),
                    "transform": transform_name(output.current_transform()),
                    "current_mode": mode.map(|mode| json!({
                        "width": mode.size.w,
                        "height": mode.size.h,
                        "refresh_millihertz": mode.refresh,
                        "refresh_hz": f64::from(mode.refresh) / 1_000.0,
                    })),
                    "available_modes": output.modes().into_iter().map(|mode| json!({
                        "width": mode.size.w,
                        "height": mode.size.h,
                        "refresh_millihertz": mode.refresh,
                        "refresh_hz": f64::from(mode.refresh) / 1_000.0,
                        "preferred": Some(mode) == output.preferred_mode(),
                    })).collect::<Vec<_>>(),
                }))
            })
            .collect::<Vec<_>>();
        outputs.sort_by_key(|output| output["id"].as_u64());
        Value::Array(outputs)
    }
}

fn geometry_arg(args: &Value) -> Result<Geometry, String> {
    match args.get("geometry") {
        None | Some(Value::Null) => Ok(Geometry::All),
        Some(value) => {
            let text = value.as_str().ok_or("geometry must be a string like \"x,y WxH\"")?;
            parse_geometry(text)
        }
    }
}

fn validate_request(request: &Request) -> Result<(), CommandError> {
    if request.version != VERSION {
        return Err(CommandError::new(
            "unsupported_version",
            format!("supported version is {VERSION}"),
        ));
    }
    if request.kind != "command" {
        return Err(CommandError::new("invalid_type", "expected type \"command\""));
    }

    Ok(())
}

fn transform_name(transform: Transform) -> &'static str {
    match transform {
        Transform::Normal => "normal",
        Transform::_90 => "rotate_90",
        Transform::_180 => "rotate_180",
        Transform::_270 => "rotate_270",
        Transform::Flipped => "flipped",
        Transform::Flipped90 => "flipped_90",
        Transform::Flipped180 => "flipped_180",
        Transform::Flipped270 => "flipped_270",
    }
}

fn configured_transform_name(transform: OutputTransform) -> &'static str {
    match transform {
        OutputTransform::Normal => "normal",
        OutputTransform::Rotate90 => "rotate_90",
        OutputTransform::Rotate180 => "rotate_180",
        OutputTransform::Rotate270 => "rotate_270",
        OutputTransform::Flipped => "flipped",
        OutputTransform::Flipped90 => "flipped_90",
        OutputTransform::Flipped180 => "flipped_180",
        OutputTransform::Flipped270 => "flipped_270",
    }
}

#[derive(Debug)]
struct CommandError {
    code: &'static str,
    message: String,
}

impl CommandError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn direction_arg(args: &Value) -> Result<Direction, CommandError> {
    match args.get("direction").and_then(Value::as_str) {
        Some("left") => Ok(Direction::Left),
        Some("right") => Ok(Direction::Right),
        Some("up") => Ok(Direction::Up),
        Some("down") => Ok(Direction::Down),
        _ => Err(CommandError::new(
            "invalid_argument",
            "direction must be left, right, up, or down",
        )),
    }
}

fn workspace_arg(args: &Value) -> Result<u32, CommandError> {
    args.get("index")
        .and_then(Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .filter(|index| *index > 0)
        .ok_or_else(|| CommandError::new("invalid_argument", "index must be a positive 32-bit integer"))
}

fn prepare_parent(socket: &Path) -> Result<(), io::Error> {
    let parent = socket.parent().expect("the Ferese control socket always has a parent");
    prepare_directory(parent)
}

fn prepare_directory(parent: &Path) -> Result<(), io::Error> {
    match fs::create_dir(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    match fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a real directory", parent.display()),
                ));
            }
            if metadata.uid() != effective_uid() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is owned by another user", parent.display()),
                ));
            }
        }
        Err(error) => return Err(error),
    }

    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
}

fn bind_listener(path: &Path) -> Result<UnixListener, io::Error> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another Ferese IPC server is already running",
                ));
            }
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.file_type().is_socket() || metadata.uid() != effective_uid() {
                return Err(error);
            }
            fs::remove_file(path)?;
            UnixListener::bind(path)
        }
        Err(error) => Err(error),
    }
}

fn peer_uid(stream: &UnixStream) -> Result<u32, io::Error> {
    let mut credentials = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    // SAFETY: `credentials` and `length` point to initialized, correctly sized
    // storage for Linux SO_PEERCRED, and the stream owns a live file descriptor.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(credentials.uid)
    }
}

fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[derive(Default)]
    struct DeadlineState {
        fired: usize,
        next: Option<Instant>,
        timer: Option<DeadlineTimer<DeadlineState>>,
    }

    fn deadline_loop() -> (EventLoop<'static, DeadlineState>, DeadlineState) {
        let event_loop = EventLoop::try_new().unwrap();
        let timer = DeadlineTimer::new(event_loop.handle(), |state: &mut DeadlineState, _| {
            state.fired += 1;
            state.timer.as_ref().unwrap().update(state.next.take());
        });
        (
            event_loop,
            DeadlineState {
                timer: Some(timer),
                ..Default::default()
            },
        )
    }

    #[test]
    fn screenshot_deadline_idle_has_no_timer_and_admission_arms_immediately() {
        let (mut event_loop, mut state) = deadline_loop();
        let timer = state.timer.as_ref().unwrap();
        timer.update(None);
        assert!(timer.armed.borrow().is_none(), "idle registers no timeout source");
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 0);

        let deadline = Instant::now() + Duration::from_secs(3600);
        let timer = state.timer.as_ref().unwrap();
        timer.update(Some(deadline));
        assert_eq!(
            timer.armed.borrow().as_ref().map(|(deadline, _)| *deadline),
            Some(deadline)
        );
        timer.update(None);
        assert!(timer.armed.borrow().is_none());
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 0);
    }

    #[test]
    fn screenshot_deadline_replacement_and_cancellation_remove_old_sources() {
        let (mut event_loop, mut state) = deadline_loop();
        let due = Instant::now() - Duration::from_secs(1);
        let future = Instant::now() + Duration::from_secs(3600);
        let timer = state.timer.as_ref().unwrap();
        timer.update(Some(due));
        let original = timer.armed.borrow().as_ref().unwrap().1;
        timer.update(Some(due));
        assert_eq!(
            timer.armed.borrow().as_ref().unwrap().1,
            original,
            "unchanged deadline keeps its source"
        );
        timer.update(Some(future));
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 0, "the replaced due timer must never fire");
        state.timer.as_ref().unwrap().update(Some(due));
        state.timer.as_ref().unwrap().update(None);
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 0, "cancelling the due timer removes its wakeup");
        assert!(state.timer.as_ref().unwrap().armed.borrow().is_none());
    }

    #[test]
    fn screenshot_deadline_drop_removes_a_registered_due_source() {
        let (mut event_loop, mut state) = deadline_loop();
        state
            .timer
            .as_ref()
            .unwrap()
            .update(Some(Instant::now() - Duration::from_secs(1)));
        drop(state.timer.take());
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 0, "dropping the observer removes its timer callback");
    }

    #[test]
    fn screenshot_deadline_expiry_can_rearm_during_dispatch_then_returns_to_idle() {
        let (mut event_loop, mut state) = deadline_loop();
        let due = Instant::now() - Duration::from_secs(1);
        let future = Instant::now() + Duration::from_secs(3600);
        state.next = Some(future);
        state.timer.as_ref().unwrap().update(Some(due));
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 1);
        assert_eq!(
            state
                .timer
                .as_ref()
                .unwrap()
                .armed
                .borrow()
                .as_ref()
                .map(|(deadline, _)| *deadline),
            Some(future)
        );
        state.timer.as_ref().unwrap().update(Some(due));
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 2);
        assert!(state.timer.as_ref().unwrap().armed.borrow().is_none());
        event_loop.dispatch(Duration::ZERO, &mut state).unwrap();
        assert_eq!(state.fired, 2, "an empty queue has no repeat wakeup");
    }

    fn request(version: u32, kind: &str) -> Request {
        Request {
            version,
            id: 7,
            kind: kind.to_owned(),
            command: "get-outputs".to_owned(),
            args: json!({}),
        }
    }

    #[test]
    fn parses_direction_arguments() {
        assert_eq!(direction_arg(&json!({ "direction": "left" })).unwrap(), Direction::Left);
        assert!(direction_arg(&json!({ "direction": "diagonal" })).is_err());
    }

    #[test]
    fn validates_workspace_arguments() {
        assert_eq!(workspace_arg(&json!({ "index": 9 })).unwrap(), 9);
        assert!(workspace_arg(&json!({ "index": 0 })).is_err());
        assert!(workspace_arg(&json!({ "index": -1 })).is_err());
    }

    #[test]
    fn rejects_unsupported_versions_and_request_types_with_stable_codes() {
        let version = validate_request(&request(VERSION + 1, "command")).unwrap_err();
        let kind = validate_request(&request(VERSION, "event")).unwrap_err();

        assert_eq!(version.code, "unsupported_version");
        assert_eq!(kind.code, "invalid_type");
        assert!(validate_request(&request(VERSION, "command")).is_ok());
    }

    #[test]
    fn reads_same_user_peer_credentials() {
        let (left, right) = UnixStream::pair().unwrap();

        assert_eq!(peer_uid(&left).unwrap(), effective_uid());
        assert_eq!(peer_uid(&right).unwrap(), effective_uid());
    }

    #[test]
    fn silent_clients_release_all_connection_slots_after_the_initial_deadline() {
        let active = Arc::new(AtomicUsize::new(0));
        let (sender, _receiver) = channel::sync_channel(MAX_CONNECTIONS);
        let event_loop = EventLoop::<()>::try_new().unwrap();
        let mut clients = Vec::new();
        let mut workers = Vec::new();
        for owner in 0..MAX_CONNECTIONS {
            let (client, server) = UnixStream::pair().unwrap();
            clients.push(client);
            let permit = try_acquire_connection(&active).unwrap();
            let sender = sender.clone();
            let signal = event_loop.get_signal();
            workers.push(thread::spawn(move || {
                let _permit = permit;
                serve_connection(server, sender, signal, owner as u64);
            }));
        }
        assert!(try_acquire_connection(&active).is_none());
        let deadline = Instant::now() + transport::FRAME_TIMEOUT + Duration::from_secs(3);
        while active.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline, "silent peers retained connection slots");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(try_acquire_connection(&active).is_some());
        // Clients deliberately stay open: the server must recover on its own.
        assert_eq!(clients.len(), MAX_CONNECTIONS);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn malformed_ipc_connection_is_closed_without_reaching_the_request_queue() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (sender, _receiver) = channel::sync_channel(1);
        let event_loop = EventLoop::<()>::try_new().unwrap();
        let signal = event_loop.get_signal();
        let worker = thread::spawn(move || serve_connection(server, sender, signal, 1));
        let invalid = b"not-json";

        client.write_all(&(invalid.len() as u32).to_be_bytes()).unwrap();
        client.write_all(invalid).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        let mut byte = [0_u8; 1];

        assert_eq!(client.read(&mut byte).unwrap(), 0);
        worker.join().unwrap();
    }

    #[test]
    fn failed_screenshot_delivery_removes_only_its_staging_file() {
        let root = unique_test_directory("undelivered-screenshot");
        fs::create_dir(&root).unwrap();
        let path = root.join("capture.png");
        fs::write(&path, b"capture").unwrap();
        let response = Response::success(1, serde_json::json!({"path": path}));

        discard_undelivered_screenshot(&response);
        assert!(!path.exists());
        discard_undelivered_screenshot(&response);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn runtime_socket_directory_is_private() {
        let root = unique_test_directory("private-parent");
        let parent = root.join("ferese");
        let socket = parent.join("control.sock");

        fs::create_dir(&root).unwrap();
        prepare_parent(&socket).unwrap();
        let metadata = fs::metadata(&parent).unwrap();

        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_endpoints_reserve_private_unique_directories_and_preserve_the_default() {
        let runtime = tempfile::tempdir().unwrap();
        let default = runtime.path().join("ferese/control.sock");
        let drm = Endpoint::desktop_at(default.clone()).unwrap();
        let host = bind_listener(&drm.path).unwrap();
        let first = Endpoint::instance_at(default.clone()).unwrap();
        let second = Endpoint::instance_at(default.clone()).unwrap();
        assert_eq!(drm.path, default);
        assert_ne!(first.path, second.path);
        assert_eq!(
            first.path.parent().unwrap().parent().unwrap(),
            default.parent().unwrap().join("instances")
        );
        for endpoint in [&first, &second] {
            assert_eq!(
                fs::metadata(endpoint.path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let first_directory = first.path.parent().unwrap().to_owned();
        drop(first);
        assert!(!first_directory.exists());
        assert!(second.path.parent().unwrap().is_dir());
        assert!(default.symlink_metadata().unwrap().file_type().is_socket());
        assert_eq!(bind_listener(&default).unwrap_err().kind(), io::ErrorKind::AddrInUse);
        drop(host);
    }

    #[test]
    fn nested_endpoint_permissions_do_not_depend_on_session_umask() {
        for mask in [0o000, 0o022, 0o077] {
            // Set the mask after exec so the parent and its parallel tests
            // retain their umask and socket ownership.
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("umask {mask:03o}\nexec \"$@\""))
                .arg("ferese-ipc-umask-test")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ipc::tests::nested_endpoints_reserve_private_unique_directories_and_preserve_the_default",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "umask {mask:03o}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn nested_endpoint_rejects_an_instances_symlink() {
        let runtime = tempfile::tempdir().unwrap();
        let default = runtime.path().join("ferese/control.sock");
        let _drm = Endpoint::desktop_at(default.clone()).unwrap();
        let outside = runtime.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&outside, default.parent().unwrap().join("instances")).unwrap();
        assert!(Endpoint::instance_at(default).is_err());
        assert_eq!(fs::metadata(outside).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn listener_reclaims_only_a_stale_owned_socket() {
        // A concurrent test can fork while this listener is open, retaining
        // its descriptor until exec. Keep the close/rebind check in a process
        // that does not spawn other children during the assertion.
        const CHILD: &str = "FERESE_STALE_SOCKET_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ipc::tests::listener_reclaims_only_a_stale_owned_socket",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = unique_test_directory("stale-socket");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let first = bind_listener(&socket).unwrap();

        assert_eq!(bind_listener(&socket).unwrap_err().kind(), io::ErrorKind::AddrInUse);
        drop(first);
        let replacement = bind_listener(&socket).unwrap();

        drop(replacement);
        fs::remove_file(socket).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn listener_never_replaces_a_non_socket_path() {
        let root = unique_test_directory("non-socket");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        fs::write(&socket, b"keep").unwrap();

        assert_eq!(bind_listener(&socket).unwrap_err().kind(), io::ErrorKind::AddrInUse);
        assert_eq!(fs::read(&socket).unwrap(), b"keep");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ipc_worker_count_is_bounded_and_permits_are_reusable() {
        let active = Arc::new(AtomicUsize::new(0));
        let mut permits = (0..MAX_CONNECTIONS)
            .map(|_| try_acquire_connection(&active).unwrap())
            .collect::<Vec<_>>();

        assert!(try_acquire_connection(&active).is_none());
        permits.pop();
        assert_eq!(active.load(Ordering::Acquire), MAX_CONNECTIONS - 1);
        assert!(try_acquire_connection(&active).is_some());
    }

    #[test]
    fn idle_event_stream_disconnect_is_detected_without_an_event() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (_sender, receiver) = sync_channel(1);
        let (finished, completion) = sync_channel(1);
        let worker = thread::spawn(move || {
            relay_events(&mut server, receiver);
            finished.send(()).unwrap();
        });
        drop(client);
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn stalled_event_socket_write_expires_on_the_worker() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        server.set_write_timeout(Some(Duration::from_millis(50))).unwrap();
        let (sender, receiver) = sync_channel(1);
        let (finished, completion) = sync_channel(1);
        sender
            .send(Arc::new(ferese_ipc::events::Event {
                version: ferese_ipc::events::VERSION,
                generation: 1,
                last: true,
                change: ferese_ipc::events::Change::WindowsChanged {
                    windows: json!([{"title": "x".repeat(900_000)}]),
                },
            }))
            .unwrap();
        let worker = thread::spawn(move || {
            relay_events(&mut server, receiver);
            finished.send(()).unwrap();
        });
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(
            sender
                .try_send(Arc::new(ferese_ipc::events::Event {
                    version: ferese_ipc::events::VERSION,
                    generation: 2,
                    last: true,
                    change: ferese_ipc::events::Change::FocusChanged { focus: json!(null) },
                }))
                .is_err()
        );
    }

    fn unique_test_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("ferese-ipc-{label}-{}-{nonce}", std::process::id()))
    }
}
