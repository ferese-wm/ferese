use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use glib::gobject_ffi;
use libloading::Library;
use zeroize::{Zeroize, Zeroizing};

use crate::agent::{AuthenticationRequest, Generation, IdentityChoice, PromptEvent};

enum SessionEvent {
    Request(String, bool),
    Info(String),
    Error(String),
    Completed(bool),
}

type Object = *mut c_void;

struct Api {
    user_new: unsafe extern "C" fn(c_int) -> Object,
    session_new: unsafe extern "C" fn(Object, *const c_char) -> Object,
    initiate: unsafe extern "C" fn(Object),
    response: unsafe extern "C" fn(Object, *const c_char),
    cancel: unsafe extern "C" fn(Object),
}

impl Api {
    fn load() -> Result<Self, String> {
        // SAFETY: The typed symbols match the system polkit ABI, and the static library handles
        // keep them loaded for every session.
        unsafe {
            // GLib keeps registered types and their function pointers for the
            // lifetime of the process, including between authentication sessions.
            static LIBRARIES: OnceLock<Result<(Library, Library), String>> = OnceLock::new();
            let (agent, polkit) = LIBRARIES
                .get_or_init(|| {
                    let agent =
                        Library::new("libpolkit-agent-1.so.0").map_err(|e| e.to_string())?;
                    let polkit =
                        Library::new("libpolkit-gobject-1.so.0").map_err(|e| e.to_string())?;
                    Ok((agent, polkit))
                })
                .as_ref()
                .map_err(Clone::clone)?;
            let user_new = *polkit
                .get(b"polkit_unix_user_new\0")
                .map_err(|e| e.to_string())?;
            let session_new = *agent
                .get(b"polkit_agent_session_new\0")
                .map_err(|e| e.to_string())?;
            let initiate = *agent
                .get(b"polkit_agent_session_initiate\0")
                .map_err(|e| e.to_string())?;
            let response = *agent
                .get(b"polkit_agent_session_response\0")
                .map_err(|e| e.to_string())?;
            let cancel = *agent
                .get(b"polkit_agent_session_cancel\0")
                .map_err(|e| e.to_string())?;

            Ok(Self {
                user_new,
                session_new,
                initiate,
                response,
                cancel,
            })
        }
    }
}

pub fn check() -> Result<(), String> {
    Api::load().map(|_| ())
}

/// Display names are optional; one slow NSS lookup cannot delay the request indefinitely.
pub(crate) async fn identities(uids: &[u32]) -> Vec<IdentityChoice> {
    let mut choices: Vec<_> = uids
        .iter()
        .map(|&uid| IdentityChoice {
            uid,
            name: uid.to_string(),
        })
        .collect();
    let lookup = async {
        for choice in &mut choices {
            if let Ok(output) = tokio::process::Command::new("getent")
                .args(["passwd", &choice.uid.to_string()])
                .kill_on_drop(true)
                .output()
                .await
                && output.status.success()
                && let Ok(entry) = String::from_utf8(output.stdout)
                && let Some(name) = entry.split(':').next().filter(|name| !name.is_empty())
            {
                choice.name = name.to_owned();
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), lookup).await;
    choices
}

fn send(writer: &mut BufWriter<ChildStdin>, event: &PromptEvent) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, event).map_err(|e| e.to_string())?;
    writer.write_all(b"\n").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

struct PromptProcess {
    child: Child,
    writer: BufWriter<ChildStdin>,
    input: mpsc::Receiver<PromptEvent>,
}

impl Drop for PromptProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn prompt_process(request: AuthenticationRequest) -> Result<PromptProcess, String> {
    let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .arg("--prompt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("Prompt has no input")?;
    let stdout = child.stdout.take().ok_or("Prompt has no output")?;
    let writer = BufWriter::new(stdin);
    let (tx, rx) = mpsc::sync_channel(8);
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let _ = read_responses(&mut stdout, |event| tx.send(event).is_ok());
    });
    let mut process = PromptProcess {
        child,
        writer,
        input: rx,
    };
    send(&mut process.writer, &PromptEvent::Start(request))?;
    Ok(process)
}

/// Bounded, zeroizing framing avoids retaining submitted secrets in a BufReader.
fn read_responses(
    reader: &mut impl Read,
    mut receive: impl FnMut(PromptEvent) -> bool,
) -> io::Result<()> {
    let mut chunk = Zeroizing::new([0u8; 4096]);
    let mut frame = Zeroizing::new(Vec::with_capacity(32 * 1024));
    loop {
        let count = reader.read(&mut *chunk)?;
        if count == 0 {
            return Ok(());
        }
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                let event = serde_json::from_slice(&frame).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid authentication response",
                    )
                })?;
                frame.zeroize();
                if !receive(event) {
                    return Ok(());
                }
            } else if frame.len() < 32 * 1024 {
                frame.push(byte);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Authentication response is too large",
                ));
            }
        }
        chunk.zeroize();
    }
}

/// # Safety
/// `data` must be the live Sender retained by SessionGuard, and `prompt` must
/// be a NUL-terminated string from the polkit request signal.
unsafe extern "C" fn request_signal(
    _session: Object,
    prompt: *const c_char,
    echo: c_int,
    data: *mut c_void,
) {
    // SAFETY: SessionGuard retains the boxed Sender until the session is cancelled and
    // unreferenced, after its synchronous signal callbacks finish.
    let tx = unsafe { &*(data as *const Sender<SessionEvent>) };
    // SAFETY: The polkit request signal provides a live NUL-terminated prompt for the callback
    // duration.
    let prompt = unsafe { CStr::from_ptr(prompt) }
        .to_string_lossy()
        .into_owned();
    let _ = tx.send(SessionEvent::Request(prompt, echo != 0));
}

/// # Safety
/// `data` must be the live Sender retained by SessionGuard, and `text` must
/// be a NUL-terminated string from the polkit show-info signal.
unsafe extern "C" fn info(_session: Object, text: *const c_char, data: *mut c_void) {
    // SAFETY: SessionGuard retains the boxed Sender until the session is cancelled and
    // unreferenced, after its synchronous signal callbacks finish.
    let tx = unsafe { &*(data as *const Sender<SessionEvent>) };
    // SAFETY: The polkit text signal provides a live NUL-terminated string for the callback
    // duration.
    let text = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    let _ = tx.send(SessionEvent::Info(text));
}

/// # Safety
/// `data` must be the live Sender retained by SessionGuard, and `text` must
/// be a NUL-terminated string from the polkit show-error signal.
unsafe extern "C" fn error(_session: Object, text: *const c_char, data: *mut c_void) {
    // SAFETY: SessionGuard retains the boxed Sender until the session is cancelled and
    // unreferenced, after its synchronous signal callbacks finish.
    let tx = unsafe { &*(data as *const Sender<SessionEvent>) };
    // SAFETY: The polkit text signal provides a live NUL-terminated string for the callback
    // duration.
    let text = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    let _ = tx.send(SessionEvent::Error(text));
}

/// # Safety
/// `data` must be the live Sender retained by SessionGuard until this
/// synchronous polkit completed callback returns.
unsafe extern "C" fn completed(_session: Object, success: c_int, data: *mut c_void) {
    // SAFETY: SessionGuard retains the boxed Sender until the session is cancelled and
    // unreferenced, after its synchronous signal callbacks finish.
    let tx = unsafe { &*(data as *const Sender<SessionEvent>) };
    let _ = tx.send(SessionEvent::Completed(success != 0));
}

/// # Safety
/// `session` must be a live GObject. The callback must match the named
/// signal ABI, and `data` must remain valid until the session is destroyed.
unsafe fn connect(
    session: Object,
    name: &'static CStr,
    callback: gobject_ffi::GCallback,
    data: *mut c_void,
) {
    // SAFETY: The caller supplies a live session, a static NUL-terminated signal name and a
    // callback matching that signal; its data remains alive until disconnection.
    unsafe {
        gobject_ffi::g_signal_connect_data(
            session.cast(),
            name.as_ptr(),
            callback,
            data,
            None,
            gobject_ffi::G_CONNECT_DEFAULT,
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SessionOutcome {
    Completed(bool),
    Cancelled,
    IdentityChanged { uid: u32, generation: Generation },
}

struct SessionGuard {
    object: Object,
    signal_data: *mut Sender<SessionEvent>,
    cancel: unsafe extern "C" fn(Object),
    completed: bool,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // SAFETY: This guard owns the session reference and boxed Sender. Cancellation and final
        // unref finish callbacks before the Sender is reclaimed exactly once.
        unsafe {
            if !self.completed {
                (self.cancel)(self.object);
            }
            gobject_ffi::g_object_unref(self.object.cast());
            drop(Box::from_raw(self.signal_data));
        }
    }
}

#[must_use]
fn permitted_selection(request: &AuthenticationRequest, uid: u32) -> bool {
    request
        .identities
        .iter()
        .any(|identity| identity.uid == uid)
}

#[must_use]
fn response_buffer(value: &str) -> Option<Zeroizing<Vec<u8>>> {
    if value.as_bytes().contains(&0) {
        return None;
    }
    // Allocate room for the terminator up front so appending it cannot leave
    // a password copy in an old allocation. Drop wipes the entire FFI buffer.
    let mut response = Zeroizing::new(Vec::with_capacity(value.len() + 1));
    response.extend_from_slice(value.as_bytes());
    response.push(0);
    Some(response)
}

fn run_session(
    api: &Api,
    request: &AuthenticationRequest,
    cookie: &CString,
    context: &glib::MainContext,
    process: &mut PromptProcess,
    cancelled: &AtomicBool,
) -> Result<SessionOutcome, String> {
    let generation = request.generation;
    send(
        &mut process.writer,
        &PromptEvent::IdentityChanged {
            generation,
            uid: request.selected_uid,
        },
    )?;
    if generation.attempt > 0 {
        send(
            &mut process.writer,
            &PromptEvent::Error {
                generation,
                text: "Password not accepted. Try again.".into(),
            },
        )?;
    }
    // SAFETY: user_new is a typed symbol retained by Api and accepts the selected integer UID.
    let identity = unsafe { (api.user_new)(request.selected_uid as c_int) };
    if identity.is_null() {
        return Err("Could not create polkit identity".into());
    }
    // SAFETY: identity is a non-null live polkit object and cookie is a NUL-terminated CString
    // retained for this call.
    let session = unsafe { (api.session_new)(identity, cookie.as_ptr()) };
    // SAFETY: The caller owns the identity reference returned by user_new; session_new has
    // retained what it needs.
    unsafe { gobject_ffi::g_object_unref(identity.cast()) };
    if session.is_null() {
        return Err("Could not create polkit session".into());
    }

    let (tx, rx) = mpsc::channel();
    let signal_data = Box::into_raw(Box::new(tx));
    let mut session_guard = SessionGuard {
        object: session,
        signal_data,
        cancel: api.cancel,
        completed: false,
    };
    let signal_data = signal_data.cast();
    // SAFETY: The session and boxed signal data outlive all callbacks. Each callback signature
    // matches its named GLib signal before erasure to GCallback; initiate uses that same live
    // session.
    unsafe {
        connect(
            session,
            c"request",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(Object, *const c_char, c_int, *mut c_void),
                unsafe extern "C" fn(),
            >(
                request_signal as unsafe extern "C" fn(Object, *const c_char, c_int, *mut c_void),
            )),
            signal_data,
        );
        connect(
            session,
            c"show-info",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(Object, *const c_char, *mut c_void),
                unsafe extern "C" fn(),
            >(
                info as unsafe extern "C" fn(Object, *const c_char, *mut c_void),
            )),
            signal_data,
        );
        connect(
            session,
            c"show-error",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(Object, *const c_char, *mut c_void),
                unsafe extern "C" fn(),
            >(
                error as unsafe extern "C" fn(Object, *const c_char, *mut c_void),
            )),
            signal_data,
        );
        connect(
            session,
            c"completed",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(Object, c_int, *mut c_void),
                unsafe extern "C" fn(),
            >(
                completed as unsafe extern "C" fn(Object, c_int, *mut c_void),
            )),
            signal_data,
        );

        (api.initiate)(session);
    }

    let mut outcome = None;
    let mut stopped = false;
    let mut awaiting_response = false;
    let mut selected_uid = None;
    while outcome.is_none() {
        while context.pending() {
            context.iteration(false);
        }
        while let Ok(event) = rx.try_recv() {
            match event {
                SessionEvent::Request(prompt, echo) => {
                    awaiting_response = true;
                    send(
                        &mut process.writer,
                        &PromptEvent::Request {
                            generation,
                            prompt,
                            echo,
                        },
                    )?;
                }
                SessionEvent::Info(text) => {
                    send(&mut process.writer, &PromptEvent::Info { generation, text })?
                }
                SessionEvent::Error(text) => send(
                    &mut process.writer,
                    &PromptEvent::Error { generation, text },
                )?,
                SessionEvent::Completed(success) => outcome = Some(success),
            }
        }
        // Drain UI cancellation and account changes before committing a terminal result.
        loop {
            let event = match process.input.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    cancelled.store(true, Ordering::Release);
                    break;
                }
            };
            match event {
                PromptEvent::Response {
                    generation: response_generation,
                    uid,
                    mut value,
                } => {
                    if uid == request.selected_uid
                        && response_generation == generation
                        && !stopped
                        && outcome.is_none()
                        && awaiting_response
                        && !cancelled.load(Ordering::Acquire)
                        && selected_uid.is_none()
                        && let Some(response) = response_buffer(&value)
                    {
                        value.zeroize();
                        awaiting_response = false;
                        // SAFETY: SessionGuard keeps the session alive and response_buffer
                        // supplies a NUL-terminated response for the synchronous call.
                        unsafe { (api.response)(session, response.as_ptr().cast()) };
                    }
                    value.zeroize();
                }
                PromptEvent::SelectIdentity {
                    generation: selection_generation,
                    uid,
                } if permitted_selection(request, uid)
                    && selection_generation.attempt == 0
                    && selection_generation.selection > generation.selection
                    && selected_uid.is_none_or(|(_, pending): (u32, Generation)| {
                        selection_generation.selection > pending.selection
                    })
                    && (!stopped || selected_uid.is_some()) =>
                {
                    selected_uid = Some((uid, selection_generation));
                }
                PromptEvent::Cancel => cancelled.store(true, Ordering::Release),
                _ => {}
            }
        }
        if !stopped
            && (selected_uid.is_some()
                || cancelled.load(Ordering::Acquire)
                || process
                    .child
                    .try_wait()
                    .map_err(|e| e.to_string())?
                    .is_some())
        {
            if outcome.is_none() {
                // SAFETY: SessionGuard owns this live session and the symbol remains loaded.
                unsafe { (api.cancel)(session) };
            }
            stopped = true;
        }
        if outcome.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    session_guard.completed = true;
    if cancelled.load(Ordering::Acquire) {
        Ok(SessionOutcome::Cancelled)
    } else if let Some((uid, generation)) = selected_uid {
        Ok(SessionOutcome::IdentityChanged { uid, generation })
    } else if stopped {
        Ok(SessionOutcome::Cancelled)
    } else {
        Ok(SessionOutcome::Completed(outcome.expect(
            "the session loop finished only after receiving a completion outcome",
        )))
    }
}

pub fn authenticate(
    mut request: AuthenticationRequest,
    cookie: String,
    cancelled: Arc<AtomicBool>,
) -> Result<bool, String> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(false);
    }
    let api = Api::load()?;
    let cookie = CString::new(cookie).map_err(|_| "Invalid authentication cookie")?;
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let mut process = prompt_process(request.clone())?;
            authenticate_sessions(
                &api,
                &mut request,
                &cookie,
                &context,
                &mut process,
                &cancelled,
            )
        })
        .map_err(|error| error.to_string())?
}

const MAX_AUTH_ATTEMPTS: usize = 3;

fn authenticate_sessions(
    api: &Api,
    request: &mut AuthenticationRequest,
    cookie: &CString,
    context: &glib::MainContext,
    process: &mut PromptProcess,
    cancelled: &AtomicBool,
) -> Result<bool, String> {
    let mut failures = 0;
    loop {
        match run_session(api, request, cookie, context, process, cancelled) {
            Ok(SessionOutcome::Completed(true)) => break Ok(true),
            Ok(SessionOutcome::IdentityChanged { uid, generation })
                if !cancelled.load(Ordering::Acquire) =>
            {
                request.selected_uid = uid;
                request.generation = generation;
            }
            Ok(SessionOutcome::Completed(false)) if !cancelled.load(Ordering::Acquire) => {
                failures += 1;
                // Switching accounts never resets this request's attempt budget.
                if failures >= MAX_AUTH_ATTEMPTS {
                    let failure =
                        format!("Authentication failed after {MAX_AUTH_ATTEMPTS} attempts");
                    show_failure(process, &failure, cancelled)?;
                    break Err(failure);
                }
                request.generation.attempt += 1;
            }
            Ok(_) => break Ok(false),
            Err(error) => break Err(error),
        }
    }
}

fn show_failure(
    process: &mut PromptProcess,
    text: &str,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    send(
        &mut process.writer,
        &PromptEvent::Finished { text: text.into() },
    )?;
    // Give the user a chance to read the outcome, while bounding ownership of the prompt.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !cancelled.load(Ordering::Acquire) {
        match process.input.recv_timeout(Duration::from_millis(50)) {
            Ok(PromptEvent::Cancel) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {}
        }
        if process
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use super::*;
    use crate::agent::tests;

    static RESPONSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// # Safety
    /// `session` must be the live test polkit session with its registered signals.
    unsafe extern "C" fn fake_initiate(session: Object) {
        // SAFETY: The fake receives a live test session and emits request with the registered
        // string and integer argument types.
        unsafe {
            gobject_ffi::g_signal_emit_by_name(
                session.cast(),
                c"request".as_ptr(),
                c"Password:".as_ptr(),
                0 as c_int,
            );
        }
    }

    /// # Safety
    /// `session` must be the live test polkit session with its registered signals.
    unsafe extern "C" fn fake_cancel(session: Object) {
        // SAFETY: The fake receives a live test session and emits completed with its registered
        // gboolean argument type.
        unsafe {
            gobject_ffi::g_signal_emit_by_name(session.cast(), c"completed".as_ptr(), 0 as c_int);
        }
    }

    /// # Safety
    /// `session` must be the live test polkit session with its registered signals.
    unsafe extern "C" fn fake_response(session: Object, _value: *const c_char) {
        RESPONSES.fetch_add(1, Ordering::SeqCst);
        // SAFETY: The fake receives a live test session and emits completed with its registered
        // gboolean argument type.
        unsafe {
            gobject_ffi::g_signal_emit_by_name(session.cast(), c"completed".as_ptr(), 1 as c_int);
        }
    }

    unsafe extern "C" fn completed_at_start(session: Object) {
        // SAFETY: The fixture uses the real session signal signature, without a PAM helper.
        unsafe {
            gobject_ffi::g_signal_emit_by_name(session.cast(), c"completed".as_ptr(), 1 as c_int);
        }
    }

    #[test]
    fn queued_cancel_and_account_switch_beat_already_completed_authentication() {
        for event in [
            PromptEvent::Cancel,
            PromptEvent::SelectIdentity {
                generation: Generation {
                    selection: 1,
                    attempt: 0,
                },
                uid: 1001,
            },
        ] {
            let mut api = Api::load().unwrap();
            api.initiate = completed_at_start;
            api.cancel = fake_cancel;
            let context = glib::MainContext::new();
            context
                .with_thread_default(|| {
                    let mut child = Command::new("cat")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .spawn()
                        .unwrap();
                    let writer = BufWriter::new(child.stdin.take().unwrap());
                    let (tx, input) = mpsc::channel();
                    let is_cancel = matches!(event, PromptEvent::Cancel);
                    tx.send(event).unwrap();
                    let mut process = PromptProcess {
                        child,
                        writer,
                        input,
                    };
                    let result = run_session(
                        &api,
                        &tests::request_fixture(),
                        &CString::new("test-cookie").unwrap(),
                        &context,
                        &mut process,
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                    if is_cancel {
                        assert_eq!(result, SessionOutcome::Cancelled);
                    } else {
                        assert_eq!(
                            result,
                            SessionOutcome::IdentityChanged {
                                uid: 1001,
                                generation: Generation {
                                    selection: 1,
                                    attempt: 0
                                }
                            }
                        );
                    }
                })
                .unwrap();
        }
    }

    #[test]
    fn disconnected_prompt_cannot_commit_success() {
        let mut api = Api::load().unwrap();
        api.initiate = completed_at_start;
        api.cancel = fake_cancel;
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let mut child = Command::new("cat")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap();
                let writer = BufWriter::new(child.stdin.take().unwrap());
                let (tx, input) = mpsc::channel();
                drop(tx);
                let mut process = PromptProcess {
                    child,
                    writer,
                    input,
                };
                assert_eq!(
                    run_session(
                        &api,
                        &tests::request_fixture(),
                        &CString::new("test-cookie").unwrap(),
                        &context,
                        &mut process,
                        &AtomicBool::new(false)
                    )
                    .unwrap(),
                    SessionOutcome::Cancelled
                );
            })
            .unwrap();
    }

    #[test]
    fn response_framing_handles_chunks_and_rejects_oversized_or_invalid_frames() {
        let response = PromptEvent::Response {
            generation: Generation::default(),
            uid: 1000,
            value: "dummy-secret".to_owned().into(),
        };
        let encoded = format!(
            "{}\n{{\"type\":\"cancel\"}}\n",
            serde_json::to_string(&response).unwrap()
        );
        let mut events = Vec::new();
        read_responses(&mut encoded.as_bytes(), |event| {
            events.push(event);
            true
        })
        .unwrap();
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], PromptEvent::Response { value, .. } if value.as_str() == "dummy-secret")
        );
        assert!(!format!("{:?}", events[0]).contains("dummy-secret"));
        assert!(read_responses(&mut vec![b'x'; 32769].as_slice(), |_| true).is_err());
        assert!(read_responses(&mut &b"invalid\n"[..], |_| true).is_err());
    }

    #[test]
    fn registered_identity_types_survive_between_api_loads() {
        for _ in 0..3 {
            let api = Api::load().unwrap();
            // SAFETY: the identity constructor accepts the current uid and
            // returns an owned GObject reference.
            unsafe {
                let identity = (api.user_new)(libc::getuid() as c_int);
                assert!(!identity.is_null());
                gobject_ffi::g_object_unref(identity.cast());
            }
        }
    }

    #[test]
    fn retries_stop_after_three_failures_even_when_the_account_changes() {
        let mut api = Api::load().unwrap();
        api.initiate = fake_initiate;
        api.response = fake_failed_response;
        api.cancel = fake_cancel;
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let mut child = Command::new("cat")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let writer = BufWriter::new(child.stdin.take().unwrap());
                let output = child.stdout.take().unwrap();
                let (tx, input) = mpsc::channel();
                let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let counter = starts.clone();
                let prompt = std::thread::spawn(move || {
                    let mut uid = 1000;
                    for line in BufReader::new(output).lines() {
                        let Ok(line) = line else { break };
                        let event = serde_json::from_str::<PromptEvent>(&line);
                        if matches!(event, Ok(PromptEvent::Finished { .. })) {
                            let _ = tx.send(PromptEvent::Cancel);
                            break;
                        }
                        if let Ok(PromptEvent::Request { generation, .. }) = event {
                            let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
                            let event = if count == 3 {
                                uid = 1001;
                                PromptEvent::SelectIdentity {
                                    uid,
                                    generation: Generation {
                                        selection: generation.selection + 1,
                                        attempt: 0,
                                    },
                                }
                            } else {
                                PromptEvent::Response {
                                    generation,
                                    uid,
                                    value: "incorrect".to_owned().into(),
                                }
                            };
                            if tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                });
                let mut process = PromptProcess {
                    child,
                    writer,
                    input,
                };
                let mut request = tests::request_fixture();
                let result = authenticate_sessions(
                    &api,
                    &mut request,
                    &CString::new("test-cookie").unwrap(),
                    &context,
                    &mut process,
                    &AtomicBool::new(false),
                );
                drop(process);
                prompt.join().unwrap();
                assert_eq!(result, Err("Authentication failed after 3 attempts".into()));
                assert_eq!(starts.load(Ordering::SeqCst), 4);
                assert_eq!(request.selected_uid, 1001);
            })
            .unwrap();
    }

    /// # Safety
    /// `session` must be the live test polkit session with its registered signals.
    unsafe extern "C" fn fake_failed_response(session: Object, _value: *const c_char) {
        // SAFETY: The fake receives a live test session and emits completed with its registered
        // gboolean argument type.
        unsafe {
            gobject_ffi::g_signal_emit_by_name(session.cast(), c"completed".as_ptr(), 0 as c_int);
        }
    }

    #[test]
    fn account_changes_cancel_old_sessions_and_reject_stale_passwords() {
        // Use actual GLib session objects and callbacks, with no PAM helper or
        // authority contacted. Only the session's initiate/response/cancel calls are fake.
        let mut api = Api::load().unwrap();
        api.initiate = fake_initiate;
        api.response = fake_response;
        api.cancel = fake_cancel;
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let mut child = Command::new("sleep")
                    .arg("5")
                    .stdin(Stdio::piped())
                    .spawn()
                    .unwrap();
                let writer = BufWriter::new(child.stdin.take().unwrap());
                let cookie = CString::new("test-cookie").unwrap();
                let cancelled = AtomicBool::new(false);
                let mut request = tests::request_fixture();
                let (tx, input) = mpsc::channel();
                let mut process = PromptProcess {
                    child,
                    writer,
                    input,
                };
                tx.send(PromptEvent::SelectIdentity {
                    uid: 1001,
                    generation: Generation {
                        selection: 1,
                        attempt: 0,
                    },
                })
                .unwrap();
                tx.send(PromptEvent::Response {
                    generation: Generation::default(),
                    uid: 1000,
                    value: "stale-response".to_owned().into(),
                })
                .unwrap();
                assert_eq!(
                    run_session(&api, &request, &cookie, &context, &mut process, &cancelled)
                        .unwrap(),
                    SessionOutcome::IdentityChanged {
                        uid: 1001,
                        generation: Generation {
                            selection: 1,
                            attempt: 0
                        }
                    }
                );
                assert_eq!(RESPONSES.load(Ordering::SeqCst), 0);
                request.selected_uid = 1001;
                request.generation = Generation {
                    selection: 1,
                    attempt: 0,
                };
                tx.send(PromptEvent::Response {
                    generation: Generation::default(),
                    uid: 1000,
                    value: "stale-response".to_owned().into(),
                })
                .unwrap();
                // Matching UID alone is insufficient: reject responses from a
                // previous account selection and from an earlier retry.
                tx.send(PromptEvent::Response {
                    generation: Generation::default(),
                    uid: 1001,
                    value: "same-user-old-generation".to_owned().into(),
                })
                .unwrap();
                request.generation.attempt = 1;
                tx.send(PromptEvent::Response {
                    generation: Generation {
                        attempt: 0,
                        ..request.generation
                    },
                    uid: 1001,
                    value: "same-user-old-attempt".to_owned().into(),
                })
                .unwrap();
                tx.send(PromptEvent::Response {
                    generation: request.generation,
                    uid: 1001,
                    value: "new-response".to_owned().into(),
                })
                .unwrap();
                assert_eq!(
                    run_session(&api, &request, &cookie, &context, &mut process, &cancelled)
                        .unwrap(),
                    SessionOutcome::Completed(true)
                );
                assert_eq!(RESPONSES.load(Ordering::SeqCst), 1);
                tx.send(PromptEvent::Cancel).unwrap();
                assert_eq!(
                    run_session(&api, &request, &cookie, &context, &mut process, &cancelled)
                        .unwrap(),
                    SessionOutcome::Cancelled
                );
            })
            .unwrap();
    }

    #[test]
    fn account_switches_keep_the_latest_generation() {
        let mut api = Api::load().unwrap();
        api.initiate = fake_initiate;
        api.cancel = fake_cancel;
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let mut child = Command::new("sleep")
                    .arg("5")
                    .stdin(Stdio::piped())
                    .spawn()
                    .unwrap();
                let writer = BufWriter::new(child.stdin.take().unwrap());
                let (tx, input) = mpsc::channel();
                let mut process = PromptProcess {
                    child,
                    writer,
                    input,
                };
                for (selection, uid) in [(2, 1001), (3, 1000), (1, 1001), (2, 1001)] {
                    tx.send(PromptEvent::SelectIdentity {
                        generation: Generation {
                            selection,
                            attempt: 0,
                        },
                        uid,
                    })
                    .unwrap();
                }
                assert_eq!(
                    run_session(
                        &api,
                        &tests::request_fixture(),
                        &CString::new("test-cookie").unwrap(),
                        &context,
                        &mut process,
                        &AtomicBool::new(false),
                    )
                    .unwrap(),
                    SessionOutcome::IdentityChanged {
                        uid: 1000,
                        generation: Generation {
                            selection: 3,
                            attempt: 0
                        }
                    }
                );
            })
            .unwrap();
    }

    #[test]
    fn ffi_response_is_nul_terminated_and_owned_by_a_zeroizing_buffer() {
        let response = response_buffer("test-password").unwrap();
        assert_eq!(
            CStr::from_bytes_with_nul(&response).unwrap().to_bytes(),
            b"test-password"
        );
        assert!(response_buffer("invalid\0password").is_none());
        assert_eq!(response_buffer("").unwrap().as_slice(), &[0]);
    }

    #[test]
    fn account_switches_accept_only_offered_identities() {
        let request = tests::request_fixture();
        assert!(permitted_selection(&request, 1001));
        assert!(permitted_selection(&request, 1000));
        assert!(!permitted_selection(&request, 0));
        assert!(!permitted_selection(&request, u32::MAX));
    }
}
